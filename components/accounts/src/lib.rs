//! `accounts` component — session-based authentication for the editorial plane.
//!
//! V0 provides:
//! - `Principal` and `Session` types (parse, don't validate)
//! - `SessionToken` newtype (private field — no stringly-typed tokens)
//! - `SessionStore` trait
//! - `DevSessionStore` with HMAC-SHA256 signed tokens
//! - axum `FromRequestParts` extractor that returns 401 on missing/invalid cookies
//!
//! V1 adds:
//! - `AccountId` opaque newtype and `Account` model
//! - `Principal::Account` variant
//! - `MagicLinkSessionStore` — HMAC-signed sessions keyed to an `AccountId`
//! - Magic-link login token flow: `issue_login_token` / `verify_login_token`
//! - `AccountStore` trait + `InMemoryAccountStore`
//! - `MailSender` trait + `ConsoleMailSender` + `SmtpMailSender` stub

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use axum::extract::FromRef;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

// ── AccountId ─────────────────────────────────────────────────────────────────

/// Opaque identifier for a user account. Not stringly-typed at call sites.
/// Internally represented as a 32-char lowercase-hex string (16 random bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccountId(String);

impl AccountId {
    /// Generate a fresh random `AccountId`.
    pub fn generate() -> Self {
        let bytes = random_bytes_16();
        let hex = hex_encode(&bytes);
        Self(hex)
    }

    /// Expose the inner string (e.g. to embed in a URL or store in a DB).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse an `AccountId` from a previously serialised string.
    /// Returns `None` if the input does not look like a valid id (32 hex chars).
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }
}

// ── Account ───────────────────────────────────────────────────────────────────

/// A registered user account.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
}

// ── Principal ────────────────────────────────────────────────────────────────

/// Who is authenticated.
#[derive(Debug, Clone)]
pub enum Principal {
    /// Local-development session (V0, preserved for dev/testing use).
    Dev { label: String },
    /// A real account session (V1).
    Account { account: AccountId },
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
/// auth backends; V0 ships `DevSessionStore`, V1 ships `MagicLinkSessionStore`.
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

// ── MagicLinkSessionStore ─────────────────────────────────────────────────────

/// HMAC-SHA256–signed session tokens for real account sessions (V1).
///
/// Unlike `DevSessionStore`, verification is pure-HMAC over the token payload —
/// no per-process state — so a token minted by a control plane (same key) can
/// be verified inside a per-account `presemble serve`.
///
/// Session token payload: `acct:<account_id>` (unambiguous from `dev:<label>`).
///
/// The signing key is sourced from `PRESEMBLE_SESSION_KEY`; use
/// `with_key(key)` for tests.
pub struct MagicLinkSessionStore {
    key: Vec<u8>,
}

impl MagicLinkSessionStore {
    /// Create with a specific key (for tests and explicit wiring).
    pub fn with_key(key: impl Into<Vec<u8>>) -> Self {
        Self { key: key.into() }
    }

    /// Create from the `PRESEMBLE_SESSION_KEY` environment variable.
    /// Panics if the env var is not set (fail-fast at startup).
    pub fn from_env() -> Self {
        let key = std::env::var("PRESEMBLE_SESSION_KEY")
            .expect("PRESEMBLE_SESSION_KEY must be set for MagicLinkSessionStore")
            .into_bytes();
        Self { key }
    }

    /// Mint a session token for the given account id.
    ///
    /// Format: `<base64url("acct:<id>")>.<base64url(hmac)>`
    pub fn mint(&self, account_id: &AccountId) -> SessionToken {
        let payload = format!("acct:{}", account_id.as_str());
        let sig = self.hmac_sign(payload.as_bytes());
        let token = format!(
            "{}.{}",
            base64url_encode(payload.as_bytes()),
            base64url_encode(&sig)
        );
        SessionToken(token)
    }

    fn hmac_sign(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn hmac_verify(&self, data: &[u8], sig: &[u8]) -> bool {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error");
        mac.update(data);
        mac.verify_slice(sig).is_ok()
    }
}

impl SessionStore for MagicLinkSessionStore {
    fn verify(&self, token: &SessionToken) -> Option<Session> {
        let raw = token.as_str();
        let dot_pos = raw.find('.')?;
        let (b64_payload, b64_sig) = raw.split_at(dot_pos);
        let b64_sig = &b64_sig[1..];

        let payload_bytes = base64url_decode(b64_payload)?;
        let sig_bytes = base64url_decode(b64_sig)?;

        if !self.hmac_verify(&payload_bytes, &sig_bytes) {
            return None;
        }

        let payload = std::str::from_utf8(&payload_bytes).ok()?;
        let id_str = payload.strip_prefix("acct:")?;
        let account_id = AccountId::parse(id_str)?;
        Some(Session::new(Principal::Account { account: account_id }))
    }
}

// ── LoginToken ────────────────────────────────────────────────────────────────

/// Opaque token embedded in a magic-link email. Short-lived and single-use.
/// Not a `SessionToken` — it must be exchanged for a session token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoginToken(String);

impl LoginToken {
    /// Expose the raw token string (e.g. to embed in a URL query parameter).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Default TTL for login tokens: 15 minutes.
const LOGIN_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

/// Store for issuing and consuming magic-link login tokens.
///
/// Verification is HMAC-signed, single-use, and TTL-checked.
/// The in-memory consumed-token set is appropriate for alpha; swap to a
/// persistent backend later by replacing this type.
pub struct LoginTokenStore {
    key: Vec<u8>,
    ttl: Duration,
    /// Set of base64url-encoded payloads that have already been consumed.
    consumed: Mutex<HashSet<String>>,
}

impl LoginTokenStore {
    /// Create with an explicit key (for tests).
    pub fn with_key(key: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            ttl: LOGIN_TOKEN_TTL,
            consumed: Mutex::new(HashSet::new()),
        }
    }

    /// Create with an explicit key and a custom TTL (for tests with short TTLs).
    pub fn with_key_and_ttl(key: impl Into<Vec<u8>>, ttl: Duration) -> Self {
        Self {
            key: key.into(),
            ttl,
            consumed: Mutex::new(HashSet::new()),
        }
    }

    /// Create from `PRESEMBLE_SESSION_KEY` env var.
    pub fn from_env() -> Self {
        let key = std::env::var("PRESEMBLE_SESSION_KEY")
            .expect("PRESEMBLE_SESSION_KEY must be set for LoginTokenStore")
            .into_bytes();
        Self::with_key(key)
    }

    fn hmac_sign(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Issue a new single-use login token for the given account.
    ///
    /// Token payload: `login:<account_id>:<expires_unix_secs>`
    /// Token format: `<base64url(payload)>.<base64url(hmac)>`
    pub fn issue_login_token(&self, account_id: &AccountId) -> LoginToken {
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_add(self.ttl)
            .as_secs();
        let payload = format!("login:{}:{}", account_id.as_str(), expires);
        let sig = self.hmac_sign(payload.as_bytes());
        let token = format!(
            "{}.{}",
            base64url_encode(payload.as_bytes()),
            base64url_encode(&sig)
        );
        LoginToken(token)
    }

    /// Verify a login token.
    ///
    /// Returns `Some(AccountId)` on the first valid presentation.
    /// Returns `None` if the token is tampered, expired, or already consumed.
    pub fn verify_login_token(&self, token: &LoginToken) -> Option<AccountId> {
        let raw = token.as_str();
        let dot_pos = raw.find('.')?;
        let b64_payload = &raw[..dot_pos];
        let b64_sig = &raw[dot_pos + 1..];

        let payload_bytes = base64url_decode(b64_payload)?;
        let sig_bytes = base64url_decode(b64_sig)?;

        // Constant-time HMAC check
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .expect("HMAC key length error");
        mac.update(&payload_bytes);
        mac.verify_slice(&sig_bytes).ok()?;

        let payload = std::str::from_utf8(&payload_bytes).ok()?;

        // payload = "login:<account_id>:<expires_secs>"
        let rest = payload.strip_prefix("login:")?;
        // AccountId is 32 chars, then a colon, then the timestamp
        if rest.len() < 34 {
            return None;
        }
        let (id_str, colon_expires) = rest.split_at(32);
        let expires_str = colon_expires.strip_prefix(':')?;
        let expires_secs: u64 = expires_str.parse().ok()?;

        // TTL check
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now_secs > expires_secs {
            return None;
        }

        // Single-use check (use the base64url payload as the consumption key)
        let mut consumed = self.consumed.lock().expect("consumed lock poisoned");
        if !consumed.insert(b64_payload.to_string()) {
            // Already present — already consumed
            return None;
        }

        AccountId::parse(id_str)
    }
}

// ── AccountStore ──────────────────────────────────────────────────────────────

/// Resolves email addresses to accounts (create-on-first-login semantics).
pub trait AccountStore: Send + Sync + 'static {
    /// Find an existing account for the given email, or create one.
    fn find_or_create(&self, email: &str) -> Account;
}

/// In-memory implementation of `AccountStore` for alpha/testing.
pub struct InMemoryAccountStore {
    accounts: Mutex<HashMap<String, Account>>,
}

impl InMemoryAccountStore {
    pub fn new() -> Self {
        Self {
            accounts: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryAccountStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountStore for InMemoryAccountStore {
    fn find_or_create(&self, email: &str) -> Account {
        let mut map = self.accounts.lock().expect("account store lock poisoned");
        if let Some(account) = map.get(email) {
            return account.clone();
        }
        let account = Account {
            id: AccountId::generate(),
            email: email.to_string(),
        };
        map.insert(email.to_string(), account.clone());
        account
    }
}

// ── MailSender ────────────────────────────────────────────────────────────────

/// Sends magic-link emails to users.
pub trait MailSender: Send + Sync + 'static {
    /// Send a magic-link login URL to the given email address.
    fn send_login_link(&self, email: &str, login_url: &str);
}

/// Development/demo mail sender: writes the magic-link to a configurable sink.
///
/// The default sink prints to stdout. Inject a custom sink (e.g. a
/// `Arc<Mutex<Vec<String>>>`) in tests to capture sent links.
pub struct ConsoleMailSender {
    sink: Arc<dyn Fn(String) + Send + Sync>,
}

impl ConsoleMailSender {
    /// Create a sender that prints to stdout.
    pub fn new() -> Self {
        Self {
            sink: Arc::new(|msg| println!("{msg}")),
        }
    }

    /// Create a sender with a custom sink function.
    /// Used in tests to capture output.
    pub fn with_sink(sink: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }
}

impl Default for ConsoleMailSender {
    fn default() -> Self {
        Self::new()
    }
}

impl MailSender for ConsoleMailSender {
    fn send_login_link(&self, email: &str, login_url: &str) {
        let msg = format!(
            "[presemble] Magic-link login for {email}\n  URL: {login_url}\n  (console sender — for demo/dev only)"
        );
        (self.sink)(msg);
    }
}

/// Stub SMTP mail sender. Wired for future implementation; not required for alpha.
pub struct SmtpMailSender;

impl SmtpMailSender {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SmtpMailSender {
    fn default() -> Self {
        Self::new()
    }
}

impl MailSender for SmtpMailSender {
    fn send_login_link(&self, _email: &str, _login_url: &str) {
        unimplemented!("SmtpMailSender is not yet implemented — use ConsoleMailSender for alpha")
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

// ── Random helpers ────────────────────────────────────────────────────────────

/// Generate 16 random bytes using `rand::random`.
fn random_bytes_16() -> [u8; 16] {
    rand::random()
}

/// Hex-encode a byte slice to a lowercase string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

    // ── AccountId tests ───────────────────────────────────────────────────────

    #[test]
    fn account_id_generate_is_32_hex() {
        let id = AccountId::generate();
        assert_eq!(id.as_str().len(), 32, "AccountId must be 32 chars");
        assert!(
            id.as_str().chars().all(|c| c.is_ascii_hexdigit()),
            "AccountId must be all hex"
        );
    }

    #[test]
    fn account_id_parse_roundtrip() {
        let id = AccountId::generate();
        let parsed = AccountId::parse(id.as_str());
        assert_eq!(parsed.as_ref(), Some(&id));
    }

    #[test]
    fn account_id_parse_rejects_invalid() {
        assert!(AccountId::parse("tooshort").is_none());
        assert!(AccountId::parse("not_hex_not_hex_not_hex_not_hex!").is_none());
        assert!(AccountId::parse("").is_none());
    }

    #[test]
    fn account_ids_are_unique() {
        let a = AccountId::generate();
        let b = AccountId::generate();
        assert_ne!(a, b, "two generated AccountIds should differ");
    }

    // ── MagicLinkSessionStore tests ───────────────────────────────────────────

    #[test]
    fn magic_link_store_mints_and_verifies() {
        let store = MagicLinkSessionStore::with_key(b"v1-test-key");
        let account_id = AccountId::generate();
        let token = store.mint(&account_id);
        let session = store.verify(&token);
        assert!(session.is_some(), "minted token must verify");
    }

    #[test]
    fn magic_link_store_yields_account_principal() {
        let store = MagicLinkSessionStore::with_key(b"v1-key-abc");
        let account_id = AccountId::generate();
        let token = store.mint(&account_id);
        let session = store.verify(&token).unwrap();
        match session.principal() {
            Principal::Account { account } => {
                assert_eq!(account, &account_id, "principal account_id must match");
            }
            other => panic!("expected Principal::Account, got {other:?}"),
        }
    }

    #[test]
    fn magic_link_wrong_key_rejected() {
        let store_a = MagicLinkSessionStore::with_key(b"key-x");
        let store_b = MagicLinkSessionStore::with_key(b"key-y");
        let account_id = AccountId::generate();
        let token = store_a.mint(&account_id);
        assert!(
            store_b.verify(&token).is_none(),
            "token minted with key-x must not verify under key-y"
        );
    }

    #[test]
    fn magic_link_tampered_token_rejected() {
        let store = MagicLinkSessionStore::with_key(b"tamper-key");
        let account_id = AccountId::generate();
        let token = store.mint(&account_id);
        let raw = token.as_str();
        let mut chars: Vec<char> = raw.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let tampered = SessionToken(chars.iter().collect());
        assert!(store.verify(&tampered).is_none());
    }

    #[test]
    fn dev_token_does_not_verify_as_account() {
        // A "dev:" payload must not parse as an "acct:" session
        let dev_store = DevSessionStore::with_key(b"shared-key");
        let acct_store = MagicLinkSessionStore::with_key(b"shared-key");
        let dev_token = dev_store.mint("evil");
        // acct store should reject the dev: payload prefix
        assert!(
            acct_store.verify(&dev_token).is_none(),
            "dev token must not verify as account session"
        );
    }

    // ── LoginToken tests ──────────────────────────────────────────────────────

    #[test]
    fn login_token_fresh_verifies() {
        let store = LoginTokenStore::with_key(b"login-test-key");
        let account_id = AccountId::generate();
        let token = store.issue_login_token(&account_id);
        let result = store.verify_login_token(&token);
        assert_eq!(result.as_ref(), Some(&account_id), "fresh login token must verify");
    }

    #[test]
    fn login_token_single_use() {
        let store = LoginTokenStore::with_key(b"login-single-use-key");
        let account_id = AccountId::generate();
        let token = store.issue_login_token(&account_id);
        // First use succeeds
        assert!(store.verify_login_token(&token).is_some(), "first use must succeed");
        // Second use must fail
        assert!(
            store.verify_login_token(&token).is_none(),
            "second use must fail (single-use)"
        );
    }

    #[test]
    fn login_token_expired_rejected() {
        let store = LoginTokenStore::with_key_and_ttl(b"expire-key", Duration::from_secs(0));
        let account_id = AccountId::generate();
        let token = store.issue_login_token(&account_id);
        // With TTL=0 the token expires immediately (expires_secs == now_secs at mint time,
        // and by the time we verify, now > expires).
        // We sleep 1 second to be sure.
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            store.verify_login_token(&token).is_none(),
            "expired token must be rejected"
        );
    }

    #[test]
    fn login_token_wrong_key_rejected() {
        let store_a = LoginTokenStore::with_key(b"login-key-a");
        let store_b = LoginTokenStore::with_key(b"login-key-b");
        let account_id = AccountId::generate();
        let token = store_a.issue_login_token(&account_id);
        assert!(
            store_b.verify_login_token(&token).is_none(),
            "login token signed with key-a must not verify under key-b"
        );
    }

    #[test]
    fn login_token_roundtrip_to_session() {
        // issue login token → verify → mint session → verify session → same AccountId
        let login_store = LoginTokenStore::with_key(b"roundtrip-key");
        let session_store = MagicLinkSessionStore::with_key(b"roundtrip-session-key");
        let accounts = InMemoryAccountStore::new();

        let account = accounts.find_or_create("alice@example.com");
        let login_token = login_store.issue_login_token(&account.id);

        // Exchange login token for account id
        let verified_id = login_store
            .verify_login_token(&login_token)
            .expect("login token must verify");
        assert_eq!(verified_id, account.id, "verified id must match original account");

        // Mint session
        let session_token = session_store.mint(&verified_id);
        let session = session_store
            .verify(&session_token)
            .expect("session token must verify");

        match session.principal() {
            Principal::Account { account: a } => {
                assert_eq!(a, &account.id, "session principal must match original account id");
            }
            other => panic!("expected Principal::Account, got {other:?}"),
        }
    }

    // ── InMemoryAccountStore tests ────────────────────────────────────────────

    #[test]
    fn account_store_creates_on_first_login() {
        let store = InMemoryAccountStore::new();
        let account = store.find_or_create("new@example.com");
        assert_eq!(account.email, "new@example.com");
        assert_eq!(account.id.as_str().len(), 32);
    }

    #[test]
    fn account_store_returns_same_account_for_same_email() {
        let store = InMemoryAccountStore::new();
        let a1 = store.find_or_create("same@example.com");
        let a2 = store.find_or_create("same@example.com");
        assert_eq!(a1.id, a2.id, "same email must return same account id");
    }

    #[test]
    fn account_store_different_emails_get_different_accounts() {
        let store = InMemoryAccountStore::new();
        let a = store.find_or_create("alice@example.com");
        let b = store.find_or_create("bob@example.com");
        assert_ne!(a.id, b.id, "different emails must get different account ids");
    }

    // ── ConsoleMailSender tests ───────────────────────────────────────────────

    #[test]
    fn console_mail_sender_captures_login_link() {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let sender = ConsoleMailSender::with_sink(move |msg| {
            captured_clone.lock().unwrap().push(msg);
        });

        let login_store = LoginTokenStore::with_key(b"mail-test-key");
        let account_id = AccountId::generate();
        let login_token = login_store.issue_login_token(&account_id);
        let login_url = format!("https://example.com/login?token={}", login_token.as_str());

        sender.send_login_link("user@example.com", &login_url);

        let msgs = captured.lock().unwrap();
        assert_eq!(msgs.len(), 1, "must have sent exactly one message");
        assert!(
            msgs[0].contains(login_token.as_str()),
            "captured message must contain the login token"
        );
        assert!(
            msgs[0].contains("user@example.com"),
            "captured message must contain the email"
        );
    }
}
