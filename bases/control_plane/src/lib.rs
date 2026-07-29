//! Control plane shared state, router, and HTTP handlers.
//!
//! `AppState` holds all shared dependencies for the control plane service.
//! `ControlPlaneState` is the axum state type — a clonable `Arc` wrapper over
//! `AppState` that satisfies axum's `Clone + Send + Sync` requirement.
//! `router()` mounts all routes and returns an `axum::Router<()>`.

use std::sync::Arc;

use axum::{
    Form,
    Router,
    extract::{FromRef, Query, State},
    http::{HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

// ── AppState ──────────────────────────────────────────────────────────────────

/// Shared state for the control plane service.
///
/// All fields are either `Arc<dyn Trait>` for polymorphic dependencies or
/// concrete value types that are themselves internally thread-safe.
/// `AppState` is non-Clone; pass it inside a `ControlPlaneState` (see below).
pub struct AppState {
    /// Lifecycle manager for per-account `presemble serve` conductor processes.
    pub provisioner: Arc<dyn provisioning::ConductorProvisioner>,
    /// Source of truth for site registrations, placements, and port allocation.
    pub registry: Arc<site_registry::SiteRegistry>,
    /// HMAC-signed session store for real account sessions (V1 magic-link flow).
    /// Stored as `Arc` so it can be upcast to `Arc<dyn SessionStore>` for the
    /// axum `Session` extractor (which requires `FromRef<S>` for `Arc<dyn SessionStore>`).
    pub sessions: Arc<accounts::MagicLinkSessionStore>,
    /// Single-use HMAC-signed login token store (magic-link email tokens).
    pub login_tokens: accounts::LoginTokenStore,
    /// Account lookup / create-on-first-login.
    pub accounts: Arc<dyn accounts::AccountStore>,
    /// Mail sender for dispatching magic-link emails.
    pub mail: Arc<dyn accounts::MailSender>,
    /// Absolute base URL of this control plane instance (e.g. `https://app.presemble.pub`).
    /// Used to build the `/auth/verify?token=…` magic-link URL.
    pub public_base_url: String,
}

impl AppState {
    /// Construct an `AppState` from explicit dependencies (dependency injection).
    ///
    /// All heavy construction (registry hydration, provisioner config) is done
    /// by the caller (`main`) before passing the fully-constructed values here.
    pub fn new(
        provisioner: Arc<dyn provisioning::ConductorProvisioner>,
        registry: Arc<site_registry::SiteRegistry>,
        sessions: Arc<accounts::MagicLinkSessionStore>,
        login_tokens: accounts::LoginTokenStore,
        accounts: Arc<dyn accounts::AccountStore>,
        mail: Arc<dyn accounts::MailSender>,
        public_base_url: String,
    ) -> Self {
        Self {
            provisioner,
            registry,
            sessions,
            login_tokens,
            accounts,
            mail,
            public_base_url,
        }
    }
}

// ── ControlPlaneState — axum state type ──────────────────────────────────────
//
// axum requires the state type to be `Clone`. `AppState` is non-Clone (it holds
// non-Clone fields like `LoginTokenStore`), so we wrap it in a clonable newtype
// over `Arc<AppState>`.
//
// This is also the local type that lets us implement `FromRef` for
// `Arc<dyn SessionStore>` without hitting the orphan rule — we implement
// `FromRef<ControlPlaneState>` which is defined in this crate.

/// Clonable axum state wrapper over `Arc<AppState>`.
///
/// Handlers receive `State<ControlPlaneState>` (or use `FromRef` extractors).
/// Deref gives access to `&AppState` without any per-handler `.0` ceremony.
#[derive(Clone)]
pub struct ControlPlaneState(pub Arc<AppState>);

impl ControlPlaneState {
    /// Wrap an `Arc<AppState>`.
    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }
}

impl std::ops::Deref for ControlPlaneState {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ── FromRef: ControlPlaneState → Arc<dyn SessionStore> ───────────────────────
//
// The axum `Session` extractor in `accounts` requires
// `Arc<dyn SessionStore>: FromRef<S>` where S is the axum state type.
// Our state type is `ControlPlaneState` (defined in this crate — no orphan issue).
// `MagicLinkSessionStore: SessionStore`, so the unsizing coercion compiles.

impl FromRef<ControlPlaneState> for Arc<dyn accounts::SessionStore> {
    fn from_ref(s: &ControlPlaneState) -> Self {
        s.sessions.clone() as Arc<dyn accounts::SessionStore>
    }
}

// ── Form inputs ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginForm {
    pub email: String,
}

#[derive(Deserialize)]
pub struct VerifyQuery {
    pub token: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /login` — serve a minimal HTML email form.
async fn get_login() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Sign in — Presemble</title></head>
<body>
  <h1>Sign in</h1>
  <form method="post" action="/login">
    <label for="email">Email address</label><br>
    <input id="email" name="email" type="email" required autofocus>
    <button type="submit">Send magic link</button>
  </form>
</body>
</html>"#,
    )
}

/// `POST /login` — issue a magic-link and send it via the `MailSender`.
///
/// Always returns 200 (no email-enumeration leak): `find_or_create` creates the
/// account on first login; the user just needs to check their email.
async fn post_login(
    State(s): State<ControlPlaneState>,
    Form(form): Form<LoginForm>,
) -> Html<String> {
    let account = s.accounts.find_or_create(&form.email);
    let login_token = s.login_tokens.issue_login_token(&account.id);
    let url = format!(
        "{}/auth/verify?token={}",
        s.public_base_url,
        login_token.as_str()
    );
    s.mail.send_login_link(&form.email, &url);

    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Check your email — Presemble</title></head>
<body>
  <h1>Check your email</h1>
  <p>We sent a magic link to <strong>{email}</strong>. Click it to sign in.</p>
  <p>The link expires in 15 minutes and can only be used once.</p>
</body>
</html>"#,
        email = html_escape(&form.email),
    ))
}

/// `GET /auth/verify` — exchange a login token for a session cookie and redirect.
///
/// On success: 302 redirect to `/` with `Set-Cookie: presemble_session=…`.
/// On failure (tampered, expired, or already consumed): 401 with no cookie.
async fn get_verify(
    State(s): State<ControlPlaneState>,
    Query(query): Query<VerifyQuery>,
) -> Response {
    // `LoginToken` has no public from-string constructor; we use
    // `LoginTokenStore::verify_login_token_str` which wraps internally.
    match s.login_tokens.verify_login_token_str(&query.token) {
        Some(account_id) => {
            let session_token = s.sessions.mint(&account_id);
            let cookie = accounts::set_session_cookie_str(session_token.as_str());
            let mut response = (StatusCode::FOUND, "").into_response();
            response.headers_mut().insert(
                "set-cookie",
                HeaderValue::from_str(&cookie)
                    .expect("cookie value is always a valid header value"),
            );
            response
                .headers_mut()
                .insert("location", HeaderValue::from_static("/"));
            response
        }
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Minimal HTML escaping for user-controlled strings embedded in HTML.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Build the axum router for the control plane, wiring `state` into all handlers.
///
/// Routes mounted:
/// - `GET  /login`       → email input form (public)
/// - `POST /login`       → issue + send magic link (public)
/// - `GET  /auth/verify` → exchange login token for session cookie + redirect (public)
pub fn router(state: ControlPlaneState) -> Router {
    Router::new()
        .route("/login", get(get_login))
        .route("/login", post(post_login))
        .route("/auth/verify", get(get_verify))
        .with_state(state)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use accounts::{AccountId, InMemoryAccountStore, LoginTokenStore, MagicLinkSessionStore};
    use async_trait::async_trait;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use provisioning::{ConductorProvisioner, ProvisionError, Starter};
    use site_registry::{FileSiteStore, Placement, SiteId, Subdomain};
    use tempfile::TempDir;
    use tower::ServiceExt;

    // ── Fake provisioner ──────────────────────────────────────────────────────

    struct FakeProvisioner;

    #[async_trait]
    impl ConductorProvisioner for FakeProvisioner {
        async fn provision(
            &self,
            _account: &AccountId,
            _site_id: &SiteId,
            _subdomain: &Subdomain,
            _starter: &Starter,
        ) -> Result<Placement, ProvisionError> {
            Ok(Placement {
                host: "127.0.0.1".to_string(),
                port: 9999,
                site_path: std::path::PathBuf::from("/fake/path"),
            })
        }

        async fn deprovision(
            &self,
            _subdomain: &Subdomain,
            _site_id: &SiteId,
        ) -> Result<(), ProvisionError> {
            Ok(())
        }

        async fn reconcile(&self) -> Result<(), ProvisionError> {
            Ok(())
        }
    }

    // ── Test AppState builder ─────────────────────────────────────────────────

    /// Returns `(ControlPlaneState, Arc<Mutex<Vec<String>>>, TempDir)`.
    /// `TempDir` must be kept alive for the duration of the test.
    fn test_state() -> (ControlPlaneState, Arc<Mutex<Vec<String>>>, TempDir) {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);

        let sessions = Arc::new(MagicLinkSessionStore::with_key(
            b"test-session-key-32-bytes-padded".to_vec(),
        ));
        let login_tokens =
            LoginTokenStore::with_key(b"test-login-key-32-bytes-padded!!".to_vec());
        let account_store: Arc<dyn accounts::AccountStore> =
            Arc::new(InMemoryAccountStore::new());
        let mail: Arc<dyn accounts::MailSender> =
            Arc::new(accounts::ConsoleMailSender::with_sink(move |msg| {
                captured_clone.lock().unwrap().push(msg);
            }));

        // A temporary file-backed registry
        let dir = TempDir::new().expect("tempdir for test registry");
        let registry = site_registry::SiteRegistry::new(
            FileSiteStore::new(dir.path().join("registry.json")),
        )
        .expect("test registry");
        // SiteRegistry::new returns Arc<SiteRegistry> directly — no double-wrap needed

        let provisioner: Arc<dyn provisioning::ConductorProvisioner> = Arc::new(FakeProvisioner);

        let state = ControlPlaneState::new(Arc::new(AppState::new(
            provisioner,
            registry,
            sessions,
            login_tokens,
            account_store,
            mail,
            "http://test.local".to_string(),
        )));
        (state, captured, dir)
    }

    // ── Test: GET /login returns a form ───────────────────────────────────────

    #[tokio::test]
    async fn get_login_returns_form_with_email_input() {
        let (state, _captured, _dir) = test_state();
        let app = router(state);

        let req = axum::http::Request::builder()
            .uri("/login")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET /login must return 200");

        let body = to_bytes(resp.into_body(), 16_384).await.unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(
            html.contains("<input"),
            "response must contain an <input> element"
        );
        assert!(
            html.contains(r#"name="email""#),
            "input must have name=\"email\""
        );
        assert!(
            html.contains(r#"action="/login""#),
            "form must post to /login"
        );
    }

    // ── Test: POST /login sends magic link ────────────────────────────────────

    #[tokio::test]
    async fn post_login_sends_magic_link_email() {
        let (state, captured, _dir) = test_state();
        let app = router(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from("email=alice%40example.com"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "POST /login must return 200");

        let msgs = captured.lock().unwrap();
        assert_eq!(msgs.len(), 1, "exactly one email must be sent");
        assert!(
            msgs[0].contains("http://test.local/auth/verify?token="),
            "sent URL must be absolute and contain the token path; got: {}",
            msgs[0]
        );
    }

    // ── Helper: do a POST /login and extract the token from the captured mail ──

    async fn issue_token(state: &ControlPlaneState, captured: &Arc<Mutex<Vec<String>>>) -> String {
        let app = router(state.clone());
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from("email=bob%40example.com"))
            .unwrap();
        app.oneshot(req).await.unwrap();

        let msgs = captured.lock().unwrap().clone();
        let last_msg = msgs.last().expect("mail must have been sent");
        // The URL line contains "http://test.local/auth/verify?token=<TOKEN>"
        let url_part = last_msg
            .lines()
            .find(|l| l.contains("http://test.local/auth/verify?token="))
            .expect("captured mail must contain the verify URL");
        url_part
            .trim()
            .split("token=")
            .nth(1)
            .expect("URL must contain token= param")
            .to_string()
    }

    // ── Test: GET /auth/verify with valid token → 302 + cookie ───────────────

    #[tokio::test]
    async fn verify_valid_token_redirects_with_session_cookie() {
        let (state, captured, _dir) = test_state();
        let token = issue_token(&state, &captured).await;

        let app = router(state.clone());
        let req = axum::http::Request::builder()
            .uri(format!("/auth/verify?token={token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::FOUND, "verify must redirect (302)");
        assert_eq!(
            resp.headers().get("location").map(|v| v.as_bytes()),
            Some(b"/".as_ref()),
            "redirect must be to /"
        );
        let cookie_hdr = resp
            .headers()
            .get("set-cookie")
            .expect("verify must set a session cookie");
        assert!(
            cookie_hdr
                .to_str()
                .unwrap()
                .starts_with("presemble_session="),
            "cookie must be presemble_session; got: {}",
            cookie_hdr.to_str().unwrap()
        );
    }

    // ── Test: GET /auth/verify with garbage token → 401 ──────────────────────

    #[tokio::test]
    async fn verify_garbage_token_returns_401() {
        let (state, _captured, _dir) = test_state();
        let app = router(state);

        let req = axum::http::Request::builder()
            .uri("/auth/verify?token=this-is-garbage-not-a-real-token")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "garbage token must return 401"
        );
        assert!(
            resp.headers().get("set-cookie").is_none(),
            "garbage token must NOT set a cookie"
        );
    }

    // ── Test: Single-use — second verify → 401 ────────────────────────────────

    #[tokio::test]
    async fn verify_single_use_second_use_returns_401() {
        let (state, captured, _dir) = test_state();
        let token = issue_token(&state, &captured).await;

        // First use — should succeed (302)
        {
            let app = router(state.clone());
            let req = axum::http::Request::builder()
                .uri(format!("/auth/verify?token={token}"))
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FOUND, "first use must redirect");
        }

        // Second use — same token, same shared state → 401
        {
            let app = router(state.clone());
            let req = axum::http::Request::builder()
                .uri(format!("/auth/verify?token={token}"))
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "second use of same token must return 401"
            );
        }
    }
}
