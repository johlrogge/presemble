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
    extract::{FromRef, Path, Query, State},
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
    /// Base domain for hosted sites (e.g. `presemble.pub`).
    /// Used to build the editor hand-off URL `https://<subdomain>.<base_domain>/_presemble/login?token=`.
    pub base_domain: String,
}

impl AppState {
    /// Construct an `AppState` from explicit dependencies (dependency injection).
    ///
    /// All heavy construction (registry hydration, provisioner config) is done
    /// by the caller (`main`) before passing the fully-constructed values here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provisioner: Arc<dyn provisioning::ConductorProvisioner>,
        registry: Arc<site_registry::SiteRegistry>,
        sessions: Arc<accounts::MagicLinkSessionStore>,
        login_tokens: accounts::LoginTokenStore,
        accounts: Arc<dyn accounts::AccountStore>,
        mail: Arc<dyn accounts::MailSender>,
        public_base_url: String,
        base_domain: String,
    ) -> Self {
        Self {
            provisioner,
            registry,
            sessions,
            login_tokens,
            accounts,
            mail,
            public_base_url,
            base_domain,
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

#[derive(Deserialize)]
pub struct CreateSiteForm {
    pub subdomain: String,
    pub template: String,
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

/// `POST /sites` — create a new hosted site for the authenticated account.
///
/// Flow:
/// 1. Reject `Dev` principals with 403 (only real accounts can own hosted sites).
/// 2. Validate the subdomain; return 400 on `SubdomainError`.
/// 3. Validate the template name; return 400 if unknown.
/// 4. Register the site in the registry; return 409 on duplicate subdomain.
/// 5. Build a `Starter` and call the provisioner.
/// 6. On success: 303 redirect to `/`.
async fn create_site(
    session: accounts::Session,
    State(state): State<ControlPlaneState>,
    Form(form): Form<CreateSiteForm>,
) -> Response {
    use accounts::Principal;
    use site_registry::RegistryError;
    use site_registry::Subdomain;

    // Step 1: Only real accounts can own hosted sites.
    let account_id = match session.principal() {
        Principal::Account { account } => account.clone(),
        Principal::Dev { .. } => {
            return (
                StatusCode::FORBIDDEN,
                "Dev principals cannot create hosted sites",
            )
                .into_response();
        }
    };

    // Step 2: Validate subdomain.
    let subdomain = match Subdomain::new(&form.subdomain) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    // Step 3: Validate template name.
    if site_templates::template_by_name(&form.template).is_none() {
        return (StatusCode::BAD_REQUEST, "unknown template").into_response();
    }

    // Step 4: Register the site.
    let site = match state.registry.register_site(&account_id, subdomain.clone()) {
        Ok(site) => site,
        Err(RegistryError::DuplicateSubdomain(_)) => {
            return (StatusCode::CONFLICT, "subdomain already taken").into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // Step 5: Build starter and provision.
    let starter = provisioning::Starter {
        template_name: form.template,
        format: "hiccup".into(),
        style: site_templates::StyleConfig::default(),
    };

    if let Err(e) = state
        .provisioner
        .provision(&account_id, &site.id, &subdomain, &starter)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Step 6: Redirect to dashboard.
    let mut response = (StatusCode::SEE_OTHER, "").into_response();
    response
        .headers_mut()
        .insert("location", HeaderValue::from_static("/"));
    response
}

/// `GET /` — dashboard listing for an authenticated account.
///
/// Shows a create-site form and the account's existing sites.
/// "Open editor" and "Preview" links are shown only for `Ready` sites.
/// Dev principals receive 403.
async fn get_dashboard(
    session: accounts::Session,
    State(state): State<ControlPlaneState>,
) -> Response {
    use accounts::Principal;
    use site_registry::SiteStatus;

    let account_id = match session.principal() {
        Principal::Account { account } => account.clone(),
        Principal::Dev { .. } => {
            return (StatusCode::FORBIDDEN, "Dev principals cannot use the dashboard")
                .into_response();
        }
    };

    let sites = state.registry.sites_for(&account_id);

    // Build template options.
    let template_options: String = site_templates::available_templates()
        .iter()
        .map(|t| {
            format!(
                r#"<option value="{}">{} — {}</option>"#,
                html_escape(t.name),
                html_escape(t.name),
                html_escape(t.description),
            )
        })
        .collect::<Vec<_>>()
        .join("\n      ");

    // Build site list rows.
    let site_rows: String = if sites.is_empty() {
        "<li><em>No sites yet. Create one above.</em></li>".to_string()
    } else {
        sites
            .iter()
            .map(|site| {
                let subdomain = html_escape(site.subdomain.as_str());
                let status = html_escape(&site.status.to_string());
                let links = if site.status == SiteStatus::Ready {
                    format!(
                        r#" — <a href="/sites/{id}/open">Open editor</a> | <a href="https://{sub}.{base}/" target="_blank">Preview</a>"#,
                        id = html_escape(site.id.as_str()),
                        sub = subdomain,
                        base = html_escape(&state.base_domain),
                    )
                } else {
                    String::new()
                };
                format!(
                    "<li><strong>{subdomain}</strong> ({status}){links}</li>",
                    subdomain = subdomain,
                    status = status,
                    links = links,
                )
            })
            .collect::<Vec<_>>()
            .join("\n    ")
    };

    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Dashboard — Presemble</title></head>
<body>
  <h1>Your sites</h1>
  <h2>Create a new site</h2>
  <form method="post" action="/sites">
    <label for="subdomain">Subdomain</label><br>
    <input id="subdomain" name="subdomain" type="text" required><br>
    <label for="template">Template</label><br>
    <select id="template" name="template">
      {template_options}
    </select><br>
    <button type="submit">Create site</button>
  </form>
  <h2>Existing sites</h2>
  <ul>
    {site_rows}
  </ul>
</body>
</html>"#,
        template_options = template_options,
        site_rows = site_rows,
    ))
    .into_response()
}

/// `GET /sites/{id}/open` — issue a session token and redirect to the site's editor.
///
/// Guards:
/// - Dev principal → 403
/// - Unknown `SiteId` format → 404
/// - Site not owned by this account → 404 (cross-account authz)
/// - Site not Ready → 409
///
/// On success: 302 to `https://<subdomain>.<base_domain>/_presemble/login?token=<token>`.
///
/// The session token is base64url-encoded and contains only URL-safe characters
/// (`A-Z`, `a-z`, `0-9`, `-`, `_`, `.`), so no additional percent-encoding is needed.
async fn open_editor(
    session: accounts::Session,
    State(state): State<ControlPlaneState>,
    Path(id): Path<String>,
) -> Response {
    use accounts::Principal;
    use site_registry::{SiteId, SiteStatus};

    let account_id = match session.principal() {
        Principal::Account { account } => account.clone(),
        Principal::Dev { .. } => {
            return (StatusCode::FORBIDDEN, "Dev principals cannot open hosted sites")
                .into_response();
        }
    };

    // Validate SiteId format.
    let site_id = match SiteId::parse(&id) {
        Some(id) => id,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    // Only look up sites owned by this account (cross-account authz guard).
    let site = match state
        .registry
        .sites_for(&account_id)
        .into_iter()
        .find(|s| s.id == site_id)
    {
        Some(s) => s,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    // Require the site to be Ready.
    if site.status != SiteStatus::Ready {
        return StatusCode::CONFLICT.into_response();
    }

    // Mint a session token for the hand-off.
    let token = state.sessions.mint(&account_id);

    // The base64url token alphabet is A-Za-z0-9-_.  These are all URL-safe
    // characters per RFC 3986 §2.3, so we embed the token directly without
    // additional percent-encoding.
    let url = format!(
        "https://{}.{}/_presemble/login?token={}",
        site.subdomain.as_str(),
        state.base_domain,
        token.as_str(),
    );

    let mut response = (StatusCode::FOUND, "").into_response();
    response.headers_mut().insert(
        "location",
        HeaderValue::from_str(&url).expect("redirect URL must be a valid header value"),
    );
    response
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
/// - `GET  /`               → dashboard with site list + create form (authenticated)
/// - `GET  /login`          → email input form (public)
/// - `POST /login`          → issue + send magic link (public)
/// - `GET  /auth/verify`    → exchange login token for session cookie + redirect (public)
/// - `POST /sites`          → create a new hosted site (authenticated)
/// - `GET  /sites/{id}/open` → issue session token and redirect to site editor (authenticated)
pub fn router(state: ControlPlaneState) -> Router {
    Router::new()
        .route("/", get(get_dashboard))
        .route("/login", get(get_login))
        .route("/login", post(post_login))
        .route("/auth/verify", get(get_verify))
        .route("/sites", post(create_site))
        .route("/sites/{id}/open", get(open_editor))
        .with_state(state)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    use accounts::{AccountId, InMemoryAccountStore, LoginTokenStore, MagicLinkSessionStore, SessionStore};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use provisioning::FakeProvisioner;
    use site_registry::{FileSiteStore, SiteStatus, Subdomain};
    use tempfile::TempDir;
    use tower::ServiceExt;

    // ── Test AppState builder ─────────────────────────────────────────────────

    /// Returns `(ControlPlaneState, Arc<FakeProvisioner>, Arc<SiteRegistry>, Arc<Mutex<Vec<String>>>, TempDir)`.
    /// `TempDir` must be kept alive for the duration of the test.
    fn test_state() -> (
        ControlPlaneState,
        Arc<FakeProvisioner>,
        Arc<site_registry::SiteRegistry>,
        Arc<Mutex<Vec<String>>>,
        TempDir,
    ) {
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

        // A temporary file-backed registry — shared between provisioner and AppState.
        let dir = TempDir::new().expect("tempdir for test registry");
        let registry = site_registry::SiteRegistry::new(
            FileSiteStore::new(dir.path().join("registry.json")),
        )
        .expect("test registry");

        // FakeProvisioner holds a clone of the shared registry Arc.
        let fake_provisioner = FakeProvisioner::new(Arc::clone(&registry));

        let provisioner: Arc<dyn provisioning::ConductorProvisioner> =
            Arc::clone(&fake_provisioner) as Arc<dyn provisioning::ConductorProvisioner>;

        let state = ControlPlaneState::new(Arc::new(AppState::new(
            provisioner,
            Arc::clone(&registry),
            sessions,
            login_tokens,
            account_store,
            mail,
            "http://test.local".to_string(),
            "test.local".to_string(),
        )));
        (state, fake_provisioner, registry, captured, dir)
    }

    /// Mint a session cookie string for a given `AccountId` using the test key.
    fn session_cookie_for(account_id: &AccountId) -> String {
        let store = MagicLinkSessionStore::with_key(b"test-session-key-32-bytes-padded".to_vec());
        let token = store.mint(account_id);
        format!("presemble_session={}", token.as_str())
    }

    // ── Test: GET /login returns a form ───────────────────────────────────────

    #[tokio::test]
    async fn get_login_returns_form_with_email_input() {
        let (state, _provisioner, _registry, _captured, _dir) = test_state();
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
        let (state, _provisioner, _registry, captured, _dir) = test_state();
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

    async fn issue_token(
        state: &ControlPlaneState,
        captured: &Arc<Mutex<Vec<String>>>,
    ) -> String {
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
        let (state, _provisioner, _registry, captured, _dir) = test_state();
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
        let (state, _provisioner, _registry, _captured, _dir) = test_state();
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
        let (state, _provisioner, _registry, captured, _dir) = test_state();
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

    // ── Helper: POST /sites with a session cookie ─────────────────────────────

    async fn post_create_site(
        app: axum::Router,
        cookie: &str,
        body: &str,
    ) -> axum::http::Response<axum::body::Body> {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/sites")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", cookie)
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        app.oneshot(req).await.unwrap()
    }

    // ── Test: Authenticated POST /sites → 303 + site visible in registry ─────

    #[tokio::test]
    async fn create_site_authenticated_success() {
        let (state, fake_provisioner, registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        let app = router(state);
        let resp =
            post_create_site(app, &cookie, "subdomain=demo&template=personal").await;

        assert_eq!(
            resp.status(),
            StatusCode::SEE_OTHER,
            "successful create_site must redirect (303)"
        );
        assert_eq!(
            resp.headers().get("location").map(|v| v.as_bytes()),
            Some(b"/".as_ref()),
            "redirect must be to /"
        );

        // Site visible in registry with correct subdomain and Ready status.
        let sites = registry.sites_for(&account_id);
        assert_eq!(sites.len(), 1, "exactly one site must be registered");
        let site = &sites[0];
        assert_eq!(
            site.subdomain,
            Subdomain::new("demo").unwrap(),
            "subdomain must match"
        );
        assert_eq!(
            site.status,
            SiteStatus::Ready,
            "FakeProvisioner must have set the site to Ready"
        );

        // FakeProvisioner must have recorded one provision call with correct template.
        let provisions = fake_provisioner.provisions().await;
        assert_eq!(provisions.len(), 1, "exactly one provision call must be recorded");
        assert_eq!(
            provisions[0].starter.template_name,
            "personal",
            "starter.template_name must match form input"
        );
    }

    // ── Test: Duplicate subdomain → 409 ──────────────────────────────────────

    #[tokio::test]
    async fn create_site_duplicate_subdomain_returns_409() {
        let (state, _fake_provisioner, _registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        // First creation — must succeed.
        {
            let app = router(state.clone());
            let resp =
                post_create_site(app, &cookie, "subdomain=demo&template=personal").await;
            assert_eq!(
                resp.status(),
                StatusCode::SEE_OTHER,
                "first create_site must succeed (303)"
            );
        }

        // Second creation with same subdomain — must be 409.
        {
            let app = router(state.clone());
            let resp =
                post_create_site(app, &cookie, "subdomain=demo&template=personal").await;
            assert_eq!(
                resp.status(),
                StatusCode::CONFLICT,
                "duplicate subdomain must return 409"
            );
        }
    }

    // ── Test: Invalid subdomain → 400 ────────────────────────────────────────

    #[tokio::test]
    async fn create_site_invalid_subdomain_returns_400() {
        let (state, _fake_provisioner, _registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        let app = router(state);
        // Uppercase is invalid per Subdomain rules.
        let resp =
            post_create_site(app, &cookie, "subdomain=UPPER_CASE&template=personal").await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "invalid subdomain must return 400"
        );
    }

    // ── Test: Unknown template → 400 ─────────────────────────────────────────

    #[tokio::test]
    async fn create_site_unknown_template_returns_400() {
        let (state, _fake_provisioner, _registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        let app = router(state);
        let resp =
            post_create_site(app, &cookie, "subdomain=mysite&template=nope").await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "unknown template must return 400"
        );
    }

    // ── Test: Unauthenticated POST /sites → 401 ───────────────────────────────

    #[tokio::test]
    async fn create_site_unauthenticated_returns_401() {
        let (state, _fake_provisioner, _registry, _captured, _dir) = test_state();

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/sites")
            .header("content-type", "application/x-www-form-urlencoded")
            // No cookie — no session.
            .body(axum::body::Body::from("subdomain=demo&template=personal"))
            .unwrap();

        let app = router(state);
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unauthenticated request must return 401"
        );
    }

    // ── Helper: GET request with optional cookie ──────────────────────────────

    async fn get_with_cookie(
        app: axum::Router,
        uri: &str,
        cookie: Option<&str>,
    ) -> axum::http::Response<axum::body::Body> {
        let mut builder = axum::http::Request::builder().uri(uri);
        if let Some(c) = cookie {
            builder = builder.header("cookie", c);
        }
        let req = builder.body(axum::body::Body::empty()).unwrap();
        app.oneshot(req).await.unwrap()
    }

    // ── Test: Unauthenticated GET / → 401 ────────────────────────────────────

    #[tokio::test]
    async fn dashboard_unauthenticated_returns_401() {
        let (state, _provisioner, _registry, _captured, _dir) = test_state();
        let app = router(state);
        let resp = get_with_cookie(app, "/", None).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unauthenticated GET / must return 401"
        );
    }

    // ── Test: Authenticated GET / → 200 with site info ───────────────────────

    #[tokio::test]
    async fn dashboard_authenticated_shows_sites() {
        let (state, _provisioner, registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        // Register a Ready site.
        let ready_site = registry
            .register_site(&account_id, Subdomain::new("mysite").unwrap())
            .unwrap();
        registry
            .set_status(&ready_site.id, SiteStatus::Ready)
            .unwrap();

        // Register a Provisioning site (not yet Ready).
        let _prov_site = registry
            .register_site(&account_id, Subdomain::new("pending").unwrap())
            .unwrap();

        let app = router(state);
        let resp = get_with_cookie(app, "/", Some(&cookie)).await;
        assert_eq!(resp.status(), StatusCode::OK, "authenticated GET / must return 200");

        let body = to_bytes(resp.into_body(), 32_768).await.unwrap();
        let html = std::str::from_utf8(&body).unwrap();

        // Both subdomains appear in the page.
        assert!(html.contains("mysite"), "Ready site subdomain must appear in dashboard");
        assert!(html.contains("pending"), "Provisioning site subdomain must appear in dashboard");

        // Ready site shows "Open editor" link; Provisioning site must NOT.
        assert!(
            html.contains(&format!("/sites/{}/open", ready_site.id.as_str())),
            "Ready site must have an Open editor link"
        );
        assert!(
            html.contains("https://mysite.test.local/"),
            "Ready site must have a Preview link"
        );

        // Provisioning site must not have an Open editor link.
        let prov_link_marker = format!("/sites/{}/open", _prov_site.id.as_str());
        assert!(
            !html.contains(&prov_link_marker),
            "Provisioning site must NOT have an Open editor link"
        );
    }

    // ── Test: GET /sites/{id}/open for owned Ready site → 302 ────────────────

    #[tokio::test]
    async fn open_editor_ready_site_redirects() {
        let (state, _provisioner, registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        let site = registry
            .register_site(&account_id, Subdomain::new("coolsite").unwrap())
            .unwrap();
        registry.set_status(&site.id, SiteStatus::Ready).unwrap();

        let app = router(state.clone());
        let resp = get_with_cookie(
            app,
            &format!("/sites/{}/open", site.id.as_str()),
            Some(&cookie),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::FOUND,
            "open_editor for Ready site must redirect (302)"
        );

        let location = resp
            .headers()
            .get("location")
            .expect("302 response must have Location header")
            .to_str()
            .unwrap();

        // Must start with the expected prefix.
        let expected_prefix = "https://coolsite.test.local/_presemble/login?token=";
        assert!(
            location.starts_with(expected_prefix),
            "Location must start with {expected_prefix}; got: {location}"
        );

        // Extract the token and verify it round-trips through the same session store.
        let token_str = location
            .strip_prefix(expected_prefix)
            .expect("token must follow the prefix");
        let verify_store = accounts::MagicLinkSessionStore::with_key(
            b"test-session-key-32-bytes-padded".to_vec(),
        );
        let session = verify_store.verify_str(token_str);
        assert!(
            session.is_some(),
            "minted token must verify under the same key; token: {token_str}"
        );
        if let Some(s) = session {
            match s.principal() {
                accounts::Principal::Account { account } => {
                    assert_eq!(
                        *account, account_id,
                        "session principal must be the requesting account"
                    );
                }
                accounts::Principal::Dev { .. } => {
                    panic!("expected Account principal, got Dev");
                }
            }
        }
    }

    // ── Test: Cross-account access → 404 ─────────────────────────────────────

    #[tokio::test]
    async fn open_editor_cross_account_returns_404() {
        let (state, _provisioner, registry, _captured, _dir) = test_state();

        let account_a = AccountId::generate();
        let account_b = AccountId::generate();

        // Register a Ready site under account B.
        let site_b = registry
            .register_site(&account_b, Subdomain::new("bsite").unwrap())
            .unwrap();
        registry.set_status(&site_b.id, SiteStatus::Ready).unwrap();

        // Authenticate as account A and try to open account B's site.
        let cookie_a = session_cookie_for(&account_a);
        let app = router(state);
        let resp = get_with_cookie(
            app,
            &format!("/sites/{}/open", site_b.id.as_str()),
            Some(&cookie_a),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "accessing another account's site must return 404"
        );
    }

    // ── Test: Bad SiteId format → 404 ────────────────────────────────────────

    #[tokio::test]
    async fn open_editor_bad_site_id_returns_404() {
        let (state, _provisioner, _registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        let app = router(state);
        let resp = get_with_cookie(app, "/sites/not-a-valid-id/open", Some(&cookie)).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "invalid SiteId format must return 404"
        );
    }

    // ── Test: Non-Ready site → 409 ────────────────────────────────────────────

    #[tokio::test]
    async fn open_editor_provisioning_site_returns_409() {
        let (state, _provisioner, registry, _captured, _dir) = test_state();
        let account_id = AccountId::generate();
        let cookie = session_cookie_for(&account_id);

        // Site starts in Provisioning status.
        let site = registry
            .register_site(&account_id, Subdomain::new("notready").unwrap())
            .unwrap();

        let app = router(state);
        let resp = get_with_cookie(
            app,
            &format!("/sites/{}/open", site.id.as_str()),
            Some(&cookie),
        )
        .await;

        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "Provisioning site must return 409"
        );
    }

    // ── Test: Unauthenticated GET /sites/:id/open → 401 ──────────────────────

    #[tokio::test]
    async fn open_editor_unauthenticated_returns_401() {
        let (state, _provisioner, _registry, _captured, _dir) = test_state();
        // Use a plausibly-formatted SiteId.
        let fake_id = "a".repeat(32);
        let app = router(state);
        let resp = get_with_cookie(app, &format!("/sites/{fake_id}/open"), None).await;
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unauthenticated GET /sites/:id/open must return 401"
        );
    }
}
