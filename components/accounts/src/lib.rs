//! `accounts` component — session-based authentication for the editorial plane.
//!
//! V0 provides:
//! - `Principal` and `Session` types (parse, don't validate)
//! - `SessionToken` newtype (private field — no stringly-typed tokens)
//! - `SessionStore` trait
//! - `DevSessionStore` with HMAC-SHA256 signed tokens
//! - axum `FromRequestParts` extractor that returns 401 on missing/invalid cookies

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use axum::extract::FromRef;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;

type HmacSha256 = Hmac<Sha256>;

// ── Principal ────────────────────────────────────────────────────────────────

/// Who is authenticated. V0: only a dev session with a label.
/// V1 will add real accounts.
#[derive(Debug, Clone)]
pub enum Principal {
    Dev { label: String },
}

// ── SessionToken ─────────────────────────────────────────────────────────────

/// Opaque newtype wrapping the raw cookie value. Private field prevents
/// constructing from untrusted strings.
#[derive(Debug, Clone)]
pub struct SessionToken(String);

impl SessionToken {
    /// Expose the raw string value for setting the cookie header.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wrap a raw string into a `SessionToken` without any verification.
    /// Crate-private: callers outside this crate must go through
    /// `SessionStore::verify_str` to obtain a verified `Session`.
    fn from_raw(s: &str) -> Self {
        Self(s.to_string())
    }
}

// ── Session ──────────────────────────────────────────────────────────────────

/// Proof of authentication. Only produced by `SessionStore::verify`.
/// Holding a `Session` value = the request has been authenticated.
pub struct Session {
    principal: Principal,
}

impl Session {
    /// Access the authenticated principal.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Internal-only constructor — not pub so external code cannot forge sessions.
    fn new(principal: Principal) -> Self {
        Self { principal }
    }
}

// ── SessionStore ─────────────────────────────────────────────────────────────

/// Abstract session verification. Implement this trait to plug in different
/// auth backends; V0 ships `DevSessionStore`.
pub trait SessionStore: Send + Sync + 'static {
    /// Verify a token extracted from the `presemble_session` cookie.
    /// Returns `Some(Session)` if valid, `None` if missing/tampered/expired.
    fn verify(&self, token: &SessionToken) -> Option<Session>;

    /// Verify an arbitrary raw string (e.g. a URL query parameter value) by
    /// wrapping it into a `SessionToken` crate-privately and calling `verify`.
    /// This keeps the raw-string → `SessionToken` construction inside the crate
    /// boundary, so callers cannot bypass verification.
    fn verify_str(&self, raw: &str) -> Option<Session> {
        let token = SessionToken::from_raw(raw);
        self.verify(&token)
    }
}

// ── DevSessionStore ──────────────────────────────────────────────────────────

/// HMAC-SHA256–signed session tokens for local development use.
///
/// The signing key defaults to a random per-process value; set the
/// `PRESEMBLE_SESSION_KEY` environment variable to pin the key across restarts
/// (useful for reproducible tests).
pub struct DevSessionStore {
    /// Raw HMAC key bytes.
    key: Vec<u8>,
}

impl DevSessionStore {
    /// Create a `DevSessionStore` with a random per-process key.
    pub fn new_random() -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let key = if let Ok(env_key) = std::env::var("PRESEMBLE_SESSION_KEY") {
            env_key.into_bytes()
        } else {
            // Mix process ID + timestamp + a stack address for a simple random key.
            let mut h = DefaultHasher::new();
            std::process::id().hash(&mut h);
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
                .hash(&mut h);
            // Stack address adds entropy across separate process invocations.
            let ptr: usize = &h as *const _ as usize;
            ptr.hash(&mut h);
            let seed = h.finish();
            seed.to_le_bytes().to_vec()
        };
        Self { key }
    }

    /// Create a `DevSessionStore` with a specific key (for tests).
    pub fn with_key(key: impl Into<Vec<u8>>) -> Self {
        Self { key: key.into() }
    }

    /// Mint a new session token for the given label.
    ///
    /// Token format: `<base64url(payload)>.<base64url(hmac)>`
    /// where payload = `dev:<label>`.
    pub fn mint(&self, label: impl AsRef<str>) -> SessionToken {
        let payload = format!("dev:{}", label.as_ref());
        let sig = self.sign(payload.as_bytes());
        let token = format!(
            "{}.{}",
            base64url_encode(payload.as_bytes()),
            base64url_encode(&sig)
        );
        SessionToken(token)
    }

    /// Sign bytes and return the raw HMAC digest.
    fn sign(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error (HMAC accepts any key length)");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Verify and decode a token.
    /// Returns the label if the signature is valid, else `None`.
    fn decode(&self, token: &SessionToken) -> Option<String> {
        let raw = token.as_str();
        let dot_pos = raw.find('.')?;
        let (b64_payload, b64_sig) = raw.split_at(dot_pos);
        let b64_sig = &b64_sig[1..]; // skip '.'

        let payload_bytes = base64url_decode(b64_payload)?;
        let sig_bytes = base64url_decode(b64_sig)?;

        // Constant-time HMAC verification
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error");
        mac.update(&payload_bytes);
        mac.verify_slice(&sig_bytes).ok()?;

        // Parse payload: expect "dev:<label>"
        let payload = std::str::from_utf8(&payload_bytes).ok()?;
        let label = payload.strip_prefix("dev:")?;
        Some(label.to_string())
    }
}

impl SessionStore for DevSessionStore {
    fn verify(&self, token: &SessionToken) -> Option<Session> {
        let label = self.decode(token)?;
        Some(Session::new(Principal::Dev { label }))
    }
}

// ── Base64url helpers ─────────────────────────────────────────────────────────

fn base64url_encode(data: &[u8]) -> String {
    // Standard base64 with + → - and / → _ and no padding
    let encoded = base64_encode(data);
    encoded.replace('+', "-").replace('/', "_").replace('=', "")
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    // Restore standard base64 characters and add padding
    let s = s.replace('-', "+").replace('_', "/");
    let pad = match s.len() % 4 {
        2 => "==",
        3 => "=",
        _ => "",
    };
    let padded = format!("{s}{pad}");
    base64_decode(&padded)
}

/// Minimal base64 encoder (alphabet = A-Z a-z 0-9 + /).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((combined >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((combined >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((combined >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(combined & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Minimal base64 decoder (standard alphabet with padding).
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn char_to_val(c: char) -> Option<u8> {
        match c {
            'A'..='Z' => Some(c as u8 - b'A'),
            'a'..='z' => Some(c as u8 - b'a' + 26),
            '0'..='9' => Some(c as u8 - b'0' + 52),
            '+' => Some(62),
            '/' => Some(63),
            '=' => None, // padding
            _ => None,
        }
    }

    let chars: Vec<char> = input.chars().collect();
    if !chars.len().is_multiple_of(4) {
        return None;
    }

    let mut out = Vec::new();
    for chunk in chars.chunks(4) {
        let v0 = char_to_val(chunk[0])?;
        let v1 = char_to_val(chunk[1])?;
        out.push((v0 << 2) | (v1 >> 4));
        if chunk[2] != '=' {
            let v2 = char_to_val(chunk[2])?;
            out.push(((v1 & 0xf) << 4) | (v2 >> 2));
            if chunk[3] != '=' {
                let v3 = char_to_val(chunk[3])?;
                out.push(((v2 & 0x3) << 6) | v3);
            }
        }
    }
    Some(out)
}

// ── Cookie helpers ────────────────────────────────────────────────────────────

const COOKIE_NAME: &str = "presemble_session";

/// Build a `Set-Cookie` header value for the session token.
/// HttpOnly, SameSite=Lax, Path=/ (Secure omitted — Caddy handles TLS in prod).
pub fn set_session_cookie(token: &SessionToken) -> String {
    format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/",
        COOKIE_NAME,
        token.as_str()
    )
}

/// Build a `Set-Cookie` header value from a raw (already-verified) token string.
/// Use this when the token was verified via `SessionStore::verify_str` and you
/// need to set the cookie without holding a `SessionToken` value.
pub fn set_session_cookie_str(raw: &str) -> String {
    format!("{COOKIE_NAME}={raw}; HttpOnly; SameSite=Lax; Path=/")
}

/// Extract the session token from a `Cookie` header string.
pub fn read_session_cookie(cookie_header: &str) -> Option<SessionToken> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(COOKIE_NAME)
            && let Some(val) = val.strip_prefix('=')
        {
            return Some(SessionToken(val.to_string()));
        }
    }
    None
}

// ── Axum extractor ───────────────────────────────────────────────────────────

/// Error returned by the `Session` extractor when the request is unauthenticated.
pub struct Unauthorized;

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Cookie realm=\"presemble\"")],
            "401 Unauthorized",
        )
            .into_response()
    }
}

impl<S> FromRequestParts<S> for Session
where
    Arc<dyn SessionStore>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let store: Arc<dyn SessionStore> = Arc::from_ref(state);

        // Find the Cookie header and extract the presemble_session value.
        let token = parts
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(read_session_cookie);

        match token {
            Some(tok) => store.verify(&tok).ok_or(Unauthorized),
            None => Err(Unauthorized),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use axum::{Router, routing::get, extract::State};
    use axum::extract::FromRef;

    // Minimal state for extractor tests
    #[derive(Clone)]
    struct TestState {
        session_store: Arc<dyn SessionStore>,
    }

    impl FromRef<TestState> for Arc<dyn SessionStore> {
        fn from_ref(state: &TestState) -> Self {
            state.session_store.clone()
        }
    }

    // ── DevSessionStore unit tests ────────────────────────────────────────────

    #[test]
    fn verify_minted_token_is_some() {
        let store = DevSessionStore::with_key(b"test-key-for-unit-test");
        let token = store.mint("alice");
        let session = store.verify(&token);
        assert!(session.is_some(), "minted token must verify");
    }

    #[test]
    fn verified_session_has_matching_principal() {
        let store = DevSessionStore::with_key(b"test-key-abc");
        let token = store.mint("bob");
        let session = store.verify(&token).unwrap();
        assert!(
            matches!(session.principal(), Principal::Dev { label } if label == "bob"),
            "principal label should match mint label"
        );
    }

    #[test]
    fn tampered_payload_returns_none() {
        let store = DevSessionStore::with_key(b"test-key-tamper");
        let token = store.mint("alice");
        // Tamper: change a character in the payload portion
        let raw = token.as_str();
        let dot = raw.find('.').unwrap();
        let mut chars: Vec<char> = raw.chars().collect();
        // Flip first char of payload
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let tampered = SessionToken(chars.iter().collect());
        assert!(store.verify(&tampered).is_none(), "tampered token must not verify");
    }

    #[test]
    fn garbage_token_returns_none() {
        let store = DevSessionStore::with_key(b"test-key-garbage");
        let garbage = SessionToken("not-a-valid-token".to_string());
        assert!(store.verify(&garbage).is_none());
    }

    #[test]
    fn wrong_key_returns_none() {
        let store_a = DevSessionStore::with_key(b"key-a");
        let store_b = DevSessionStore::with_key(b"key-b");
        let token = store_a.mint("alice");
        assert!(
            store_b.verify(&token).is_none(),
            "token minted with key-a must not verify with key-b"
        );
    }

    // ── Cookie helpers tests ──────────────────────────────────────────────────

    #[test]
    fn set_session_cookie_format() {
        let store = DevSessionStore::with_key(b"test-key");
        let token = store.mint("dev");
        let cookie = set_session_cookie(&token);
        assert!(cookie.starts_with("presemble_session="), "must start with cookie name");
        assert!(cookie.contains("HttpOnly"), "must be HttpOnly");
        assert!(cookie.contains("SameSite=Lax"), "must be SameSite=Lax");
        assert!(cookie.contains("Path=/"), "must be Path=/");
    }

    #[test]
    fn read_session_cookie_extracts_value() {
        let store = DevSessionStore::with_key(b"test-key-cookie");
        let token = store.mint("dev");
        let cookie_header = format!("other=123; presemble_session={}; another=456", token.as_str());
        let extracted = read_session_cookie(&cookie_header);
        assert!(extracted.is_some(), "must extract token from multi-value cookie");
        // Verify the extracted token is valid
        assert!(store.verify(&extracted.unwrap()).is_some(), "extracted token must be valid");
    }

    #[test]
    fn read_session_cookie_missing_returns_none() {
        assert!(read_session_cookie("other=value; unrelated=stuff").is_none());
    }

    // ── Extractor tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn extractor_with_valid_cookie_returns_session() {
        use tower::ServiceExt;

        let store = Arc::new(DevSessionStore::with_key(b"extractor-test-key")) as Arc<dyn SessionStore>;
        let token = {
            let ds = DevSessionStore::with_key(b"extractor-test-key");
            ds.mint("testuser")
        };
        let cookie_header = format!("presemble_session={}", token.as_str());

        let state = TestState { session_store: store };

        async fn handler(session: Session, State(_s): State<TestState>) -> &'static str {
            let _ = session;
            "ok"
        }

        let app = Router::new()
            .route("/test", get(handler))
            .with_state(state);

        let req = axum::http::Request::builder()
            .uri("/test")
            .header("cookie", cookie_header)
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn extractor_without_cookie_returns_401() {
        use tower::ServiceExt;

        let store = Arc::new(DevSessionStore::with_key(b"extractor-test-key2")) as Arc<dyn SessionStore>;
        let state = TestState { session_store: store };

        async fn handler(session: Session, State(_s): State<TestState>) -> &'static str {
            let _ = session;
            "ok"
        }

        let app = Router::new()
            .route("/test", get(handler))
            .with_state(state);

        let req = axum::http::Request::builder()
            .uri("/test")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn extractor_with_invalid_cookie_returns_401() {
        use tower::ServiceExt;

        let store = Arc::new(DevSessionStore::with_key(b"extractor-test-key3")) as Arc<dyn SessionStore>;
        let state = TestState { session_store: store };

        async fn handler(session: Session, State(_s): State<TestState>) -> &'static str {
            let _ = session;
            "ok"
        }

        let app = Router::new()
            .route("/test", get(handler))
            .with_state(state);

        let req = axum::http::Request::builder()
            .uri("/test")
            .header("cookie", "presemble_session=garbage-token-value")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
