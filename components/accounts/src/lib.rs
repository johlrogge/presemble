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
//!
//! Key handling (ADR-055 amendment):
//! - `SessionKey` — validated HMAC key (non-empty, at least 32 bytes, redacted
//!   Debug). Every HMAC store requires one; `SessionKey::from_env` is the only
//!   reader of `PRESEMBLE_SESSION_KEY`.

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use axum::extract::FromRef;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

// ── AccountId ─────────────────────────────────────────────────────────────────

/// Opaque identifier for a user account. Not stringly-typed at call sites.
/// Internally represented as a 32-char lowercase-hex string (16 random bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Whether this store can ever produce a session. Stores that can issue
    /// sessions report `Open`; a store that never verifies anything (editing
    /// disabled) reports `Closed`, which selects the 401 explanation.
    fn login_availability(&self) -> LoginAvailability {
        LoginAvailability::Open
    }
}

/// Whether a `SessionStore` can ever yield a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAvailability {
    /// Sessions can be obtained (via a login link or a control-plane token).
    Open,
    /// No session can ever be obtained — editing is disabled on this server.
    Closed,
}

// ── ClosedSessionStore ───────────────────────────────────────────────────────

/// A session store that never verifies any token. Used when the server runs
/// without `--dev-session` and without a valid `PRESEMBLE_SESSION_KEY`: the editorial
/// plane is closed, and 401 responses explain how to enable it.
pub struct ClosedSessionStore;

impl SessionStore for ClosedSessionStore {
    fn verify(&self, _token: &SessionToken) -> Option<Session> {
        None
    }

    fn login_availability(&self) -> LoginAvailability {
        LoginAvailability::Closed
    }
}

// ── SessionKey ───────────────────────────────────────────────────────────────

/// Name of the environment variable carrying the shared session key (ADR-055).
pub const SESSION_KEY_ENV: &str = "PRESEMBLE_SESSION_KEY";
/// Minimum key length in bytes (HMAC-SHA256 output size; RFC 2104 guidance).
pub const MIN_SESSION_KEY_BYTES: usize = 32;

/// A validated HMAC session key. Cannot be empty or shorter than
/// `MIN_SESSION_KEY_BYTES`. Debug is redacted; no Display, no Serialize,
/// no PartialEq (never compare secrets).
#[derive(Clone)]
pub struct SessionKey(String);

impl SessionKey {
    /// Generate a fresh random key: 32 bytes from a CSPRNG, base64url-encoded
    /// (43 characters). Never reads the environment.
    pub fn generate() -> Self {
        let bytes: [u8; 32] = rand::random();
        Self(base64url_encode(&bytes))
    }

    /// Read the key from `PRESEMBLE_SESSION_KEY`.
    ///
    /// `Ok(None)` when the variable is unset; `Err` when it is set but invalid.
    /// This is the only place in the crate that reads `SESSION_KEY_ENV`.
    pub fn from_env() -> Result<Option<Self>, SessionKeyError> {
        Self::from_env_value(std::env::var_os(SESSION_KEY_ENV))
    }

    /// Validate a raw environment value (pure; does not touch the environment).
    pub fn from_env_value(
        raw: Option<std::ffi::OsString>,
    ) -> Result<Option<Self>, SessionKeyError> {
        match raw {
            None => Ok(None),
            Some(os) => {
                let s = os.into_string().map_err(|_| SessionKeyError::NotUnicode)?;
                Self::try_from(s).map(Some)
            }
        }
    }

    /// Only for handing the key to a child process environment. Never log.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Raw HMAC key bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl TryFrom<String> for SessionKey {
    type Error = SessionKeyError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.trim().is_empty() {
            return Err(SessionKeyError::Empty);
        }
        if s.len() < MIN_SESSION_KEY_BYTES {
            return Err(SessionKeyError::TooShort { len: s.len() });
        }
        Ok(Self(s))
    }
}

impl TryFrom<&str> for SessionKey {
    type Error = SessionKeyError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::try_from(s.to_string())
    }
}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(<redacted>)")
    }
}

/// Why a session key was rejected. Messages never carry key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKeyError {
    /// Set, but empty or whitespace-only.
    Empty,
    /// Shorter than `MIN_SESSION_KEY_BYTES`.
    TooShort { len: usize },
    /// Not valid UTF-8.
    NotUnicode,
}

impl std::fmt::Display for SessionKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionKeyError::Empty => write!(
                f,
                "PRESEMBLE_SESSION_KEY is set but empty. Unset it to run without hosted sessions, or set it to a random secret of at least 32 bytes (generate one with: openssl rand -base64 32)."
            ),
            SessionKeyError::TooShort { len } => write!(
                f,
                "PRESEMBLE_SESSION_KEY is too short ({len} bytes; at least 32 required). Generate one with: openssl rand -base64 32"
            ),
            SessionKeyError::NotUnicode => write!(
                f,
                "PRESEMBLE_SESSION_KEY is not valid UTF-8. Generate one with: openssl rand -base64 32"
            ),
        }
    }
}

impl std::error::Error for SessionKeyError {}

// ── DevSessionStore ──────────────────────────────────────────────────────────

/// HMAC-SHA256–signed session tokens for local development use.
///
/// `new_random()` uses a random per-process key from a CSPRNG; never reads the
/// environment. Tokens never survive a restart.
pub struct DevSessionStore {
    key: SessionKey,
}

impl DevSessionStore {
    /// Create a `DevSessionStore` with a random per-process key from a CSPRNG;
    /// never reads the environment.
    pub fn new_random() -> Self {
        Self::with_key(SessionKey::generate())
    }

    /// Create a `DevSessionStore` with a specific key.
    pub fn with_key(key: SessionKey) -> Self {
        Self { key }
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
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
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
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
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
/// The signing key is a validated `SessionKey` (typically parsed from
/// `PRESEMBLE_SESSION_KEY` at process start via `SessionKey::from_env`).
pub struct MagicLinkSessionStore {
    key: SessionKey,
}

impl MagicLinkSessionStore {
    /// Create with a validated key.
    pub fn with_key(key: SessionKey) -> Self {
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
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
            .expect("HMAC key length error");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn hmac_verify(&self, data: &[u8], sig: &[u8]) -> bool {
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
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
    key: SessionKey,
    ttl: Duration,
    /// Set of base64url-encoded payloads that have already been consumed.
    consumed: Mutex<HashSet<String>>,
}

impl LoginTokenStore {
    /// Create with a validated key and the default TTL.
    pub fn with_key(key: SessionKey) -> Self {
        Self::with_key_and_ttl(key, LOGIN_TOKEN_TTL)
    }

    /// Create with a validated key and a custom TTL (for tests with short TTLs).
    pub fn with_key_and_ttl(key: SessionKey, ttl: Duration) -> Self {
        Self {
            key,
            ttl,
            consumed: Mutex::new(HashSet::new()),
        }
    }

    fn hmac_sign(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
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

    /// Verify a login token supplied as a raw string (e.g. from a URL query parameter).
    ///
    /// Wraps the raw string into a `LoginToken` crate-internally and delegates to
    /// `verify_login_token`. Returns `Some(AccountId)` on the first valid
    /// presentation; `None` if tampered, expired, or already consumed.
    pub fn verify_login_token_str(&self, raw: &str) -> Option<AccountId> {
        let token = LoginToken(raw.to_string());
        self.verify_login_token(&token)
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
        let mut mac = HmacSha256::new_from_slice(self.key.as_bytes())
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

/// Error returned when a request is unauthenticated. The variant explains why,
/// so the 401 body can tell the user what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unauthorized {
    /// Sessions can be obtained, but this request carries no valid one.
    NotLoggedIn,
    /// The server has no way to issue sessions — editing is disabled.
    EditingDisabled,
}

impl Unauthorized {
    /// The unauthorized reason for a store, based on its login availability.
    pub fn for_store(store: &dyn SessionStore) -> Self {
        match store.login_availability() {
            LoginAvailability::Open => Unauthorized::NotLoggedIn,
            LoginAvailability::Closed => Unauthorized::EditingDisabled,
        }
    }

    /// Human-readable explanation. Never includes token, principal, or key material.
    pub fn message(&self) -> &'static str {
        match self {
            Unauthorized::NotLoggedIn => {
                "Not logged in, or your session has expired (for example after a server restart). Open the editor login link to start a session."
            }
            Unauthorized::EditingDisabled => {
                "Editing is disabled on this server. Restart presemble serve with --dev-session to enable it."
            }
        }
    }
}

#[derive(Serialize)]
struct UnauthorizedBody {
    ok: bool,
    error: &'static str,
}

impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Cookie realm=\"presemble\"")],
            axum::Json(UnauthorizedBody {
                ok: false,
                error: self.message(),
            }),
        )
            .into_response()
    }
}

/// Authenticate a request from its headers against `store`.
///
/// Reads the `presemble_session` cookie and verifies it. On failure the
/// `Unauthorized` variant is chosen from the store's login availability.
pub fn authenticate(store: &dyn SessionStore, headers: &HeaderMap) -> Result<Session, Unauthorized> {
    headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(read_session_cookie)
        .and_then(|tok| store.verify(&tok))
        .ok_or_else(|| Unauthorized::for_store(store))
}

impl<S> FromRequestParts<S> for Session
where
    Arc<dyn SessionStore>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Unauthorized;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let store: Arc<dyn SessionStore> = Arc::from_ref(state);
        authenticate(store.as_ref(), &parts.headers)
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

    /// Build a valid test key: `tag` right-padded with '-' to 32 bytes.
    fn key(tag: &str) -> SessionKey {
        SessionKey::try_from(format!("{tag:-<32}")).unwrap()
    }

    // ── SessionKey tests ──────────────────────────────────────────────────────

    #[test]
    fn session_key_rejects_empty() {
        assert_eq!(SessionKey::try_from("").unwrap_err(), SessionKeyError::Empty);
    }

    #[test]
    fn session_key_rejects_whitespace_only() {
        let spaces = " ".repeat(40);
        assert_eq!(
            SessionKey::try_from(spaces.as_str()).unwrap_err(),
            SessionKeyError::Empty
        );
    }

    #[test]
    fn session_key_rejects_31_bytes() {
        let s = "a".repeat(31);
        assert_eq!(
            SessionKey::try_from(s).unwrap_err(),
            SessionKeyError::TooShort { len: 31 }
        );
    }

    #[test]
    fn session_key_accepts_32_bytes() {
        let s = "a".repeat(32);
        assert!(SessionKey::try_from(s).is_ok());
    }

    #[test]
    fn session_key_accepts_openssl_base64_32() {
        let s = "q9ZJ0sQm3x1b7Yp2Vt8LwE4nKc6Hf5Ra0Gd9Ui3Oe1M=";
        assert_eq!(s.len(), 44);
        let k = SessionKey::try_from(s).expect("44-char base64 key must be accepted");
        assert_eq!(k.expose_secret(), s, "accepted key must not be altered");
    }

    #[test]
    fn from_env_value_none_is_ok_none() {
        assert!(matches!(SessionKey::from_env_value(None), Ok(None)));
    }

    #[test]
    fn from_env_value_empty_is_err() {
        // Regression: `PRESEMBLE_SESSION_KEY=` used to select Hosted with an empty key.
        assert_eq!(
            SessionKey::from_env_value(Some(std::ffi::OsString::new())).unwrap_err(),
            SessionKeyError::Empty
        );
    }

    #[cfg(unix)]
    #[test]
    fn from_env_value_non_utf8_is_err() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(&[0xff; 40]).to_os_string();
        assert_eq!(
            SessionKey::from_env_value(Some(raw)).unwrap_err(),
            SessionKeyError::NotUnicode
        );
    }

    #[test]
    fn too_short_error_does_not_contain_key() {
        let secret = "s3cr3t-value";
        let msg = SessionKey::try_from(secret).unwrap_err().to_string();
        assert!(!msg.contains(secret), "error must not leak the key: {msg}");
        assert!(msg.contains("PRESEMBLE_SESSION_KEY"), "msg: {msg}");
        assert!(msg.contains("32"), "msg: {msg}");
    }

    #[test]
    fn session_key_debug_is_redacted() {
        let k = key("super-secret-material");
        let dbg = format!("{k:?}");
        assert_eq!(dbg, "SessionKey(<redacted>)");
        assert!(!dbg.contains("super-secret-material"));
    }

    #[test]
    fn generated_keys_are_valid_and_distinct() {
        let a = SessionKey::generate();
        let b = SessionKey::generate();
        assert_eq!(a.expose_secret().len(), 43);
        assert!(SessionKey::try_from(a.expose_secret()).is_ok());
        assert_ne!(a.expose_secret(), b.expose_secret());
    }

    #[test]
    fn two_random_dev_stores_do_not_verify_each_other() {
        let a = DevSessionStore::new_random();
        let b = DevSessionStore::new_random();
        let token = a.mint("alice");
        assert!(a.verify(&token).is_some());
        assert!(b.verify(&token).is_none());
    }

    // ── DevSessionStore unit tests ────────────────────────────────────────────

    #[test]
    fn verify_minted_token_is_some() {
        let store = DevSessionStore::with_key(key("test-key-for-unit-test"));
        let token = store.mint("alice");
        let session = store.verify(&token);
        assert!(session.is_some(), "minted token must verify");
    }

    #[test]
    fn verified_session_has_matching_principal() {
        let store = DevSessionStore::with_key(key("test-key-abc"));
        let token = store.mint("bob");
        let session = store.verify(&token).unwrap();
        assert!(
            matches!(session.principal(), Principal::Dev { label } if label == "bob"),
            "principal label should match mint label"
        );
    }

    #[test]
    fn tampered_payload_returns_none() {
        let store = DevSessionStore::with_key(key("test-key-tamper"));
        let token = store.mint("alice");
        // Tamper: change a character in the payload portion
        let raw = token.as_str();
        let dot = raw.find('.').unwrap();
        assert!(dot > 0, "the payload is not empty, so index 0 is inside it");
        let mut chars: Vec<char> = raw.chars().collect();
        // Flip first char of payload
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        let tampered = SessionToken(chars.iter().collect());
        assert!(store.verify(&tampered).is_none(), "tampered token must not verify");
    }

    #[test]
    fn garbage_token_returns_none() {
        let store = DevSessionStore::with_key(key("test-key-garbage"));
        let garbage = SessionToken("not-a-valid-token".to_string());
        assert!(store.verify(&garbage).is_none());
    }

    #[test]
    fn wrong_key_returns_none() {
        let store_a = DevSessionStore::with_key(key("key-a"));
        let store_b = DevSessionStore::with_key(key("key-b"));
        let token = store_a.mint("alice");
        assert!(
            store_b.verify(&token).is_none(),
            "token minted with key-a must not verify with key-b"
        );
    }

    // ── Cookie helpers tests ──────────────────────────────────────────────────

    #[test]
    fn set_session_cookie_format() {
        let store = DevSessionStore::with_key(key("test-key"));
        let token = store.mint("dev");
        let cookie = set_session_cookie(&token);
        assert!(cookie.starts_with("presemble_session="), "must start with cookie name");
        assert!(cookie.contains("HttpOnly"), "must be HttpOnly");
        assert!(cookie.contains("SameSite=Lax"), "must be SameSite=Lax");
        assert!(cookie.contains("Path=/"), "must be Path=/");
    }

    #[test]
    fn read_session_cookie_extracts_value() {
        let store = DevSessionStore::with_key(key("test-key-cookie"));
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

        let store = Arc::new(DevSessionStore::with_key(key("extractor-test-key"))) as Arc<dyn SessionStore>;
        let token = {
            let ds = DevSessionStore::with_key(key("extractor-test-key"));
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

        let store = Arc::new(DevSessionStore::with_key(key("extractor-test-key2"))) as Arc<dyn SessionStore>;
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

        let store = Arc::new(DevSessionStore::with_key(key("extractor-test-key3"))) as Arc<dyn SessionStore>;
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

    // ── ClosedSessionStore / Unauthorized reason tests ─────────────────────────

    #[test]
    fn closed_store_verifies_nothing() {
        let closed = ClosedSessionStore;
        let tokens = [key("a"), key("b")]
            .into_iter()
            .map(|k| DevSessionStore::with_key(k).mint("alice"))
            .chain(std::iter::once(DevSessionStore::new_random().mint("alice")));
        for token in tokens {
            assert!(closed.verify(&token).is_none(), "closed store must verify nothing");
            assert!(closed.verify_str(token.as_str()).is_none());
        }
    }

    #[test]
    fn closed_store_reports_closed() {
        assert_eq!(ClosedSessionStore.login_availability(), LoginAvailability::Closed);
    }

    #[test]
    fn dev_and_magic_link_stores_report_open() {
        assert_eq!(
            DevSessionStore::with_key(key("k")).login_availability(),
            LoginAvailability::Open
        );
        assert_eq!(
            MagicLinkSessionStore::with_key(key("k")).login_availability(),
            LoginAvailability::Open
        );
    }

    async fn extractor_response(
        store: Arc<dyn SessionStore>,
        cookie: Option<&str>,
    ) -> (StatusCode, Option<String>, String) {
        use tower::ServiceExt;

        async fn handler(session: Session, State(_s): State<TestState>) -> &'static str {
            let _ = session;
            "ok"
        }

        let app = Router::new()
            .route("/test", get(handler))
            .with_state(TestState { session_store: store });

        let mut builder = axum::http::Request::builder().uri("/test");
        if let Some(c) = cookie {
            builder = builder.header("cookie", c);
        }
        let req = builder.body(axum::body::Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn extractor_closed_store_explains_editing_disabled() {
        let store = Arc::new(ClosedSessionStore) as Arc<dyn SessionStore>;
        let (status, ct, body) = extractor_response(store, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(ct.as_deref(), Some("application/json"));
        assert!(body.contains("Editing is disabled"), "body: {body}");
        assert!(body.contains("--dev-session"), "body: {body}");
        assert!(body.contains("\"ok\":false"), "body: {body}");
    }

    #[tokio::test]
    async fn extractor_dev_store_no_cookie_says_not_logged_in() {
        let store = Arc::new(DevSessionStore::with_key(key("k-nl"))) as Arc<dyn SessionStore>;
        let (status, ct, body) = extractor_response(store, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(ct.as_deref(), Some("application/json"));
        assert!(body.contains("Not logged in"), "body: {body}");
        assert!(!body.contains("--dev-session"), "body: {body}");
    }

    #[tokio::test]
    async fn extractor_invalid_cookie_is_not_logged_in_and_does_not_echo_value() {
        let store = Arc::new(DevSessionStore::with_key(key("k-inv"))) as Arc<dyn SessionStore>;
        let (status, _ct, body) =
            extractor_response(store, Some("presemble_session=secret-garbage-xyz")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("Not logged in"), "body: {body}");
        assert!(!body.contains("secret-garbage-xyz"), "body must not echo cookie: {body}");
    }

    #[test]
    fn authenticate_invalid_cookie_returns_not_logged_in() {
        let store = DevSessionStore::with_key(key("k-auth-inv"));
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "presemble_session=secret-garbage-xyz".parse().unwrap());
        assert_eq!(authenticate(&store, &headers).err(), Some(Unauthorized::NotLoggedIn));
    }

    #[test]
    fn authenticate_closed_store_returns_editing_disabled() {
        let mut headers = HeaderMap::new();
        headers.insert("cookie", "presemble_session=whatever".parse().unwrap());
        assert_eq!(
            authenticate(&ClosedSessionStore, &headers).err(),
            Some(Unauthorized::EditingDisabled)
        );
    }

    #[test]
    fn authenticate_valid_cookie_returns_ok() {
        let store = DevSessionStore::with_key(key("k-auth-ok"));
        let token = store.mint("carol");
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            format!("presemble_session={}", token.as_str()).parse().unwrap(),
        );
        let session = authenticate(&store, &headers).expect("valid cookie must authenticate");
        assert!(matches!(session.principal(), Principal::Dev { label } if label == "carol"));
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
        let store = MagicLinkSessionStore::with_key(key("v1-test-key"));
        let account_id = AccountId::generate();
        let token = store.mint(&account_id);
        let session = store.verify(&token);
        assert!(session.is_some(), "minted token must verify");
    }

    #[test]
    fn magic_link_store_yields_account_principal() {
        let store = MagicLinkSessionStore::with_key(key("v1-key-abc"));
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
        let store_a = MagicLinkSessionStore::with_key(key("key-x"));
        let store_b = MagicLinkSessionStore::with_key(key("key-y"));
        let account_id = AccountId::generate();
        let token = store_a.mint(&account_id);
        assert!(
            store_b.verify(&token).is_none(),
            "token minted with key-x must not verify under key-y"
        );
    }

    #[test]
    fn magic_link_tampered_token_rejected() {
        let store = MagicLinkSessionStore::with_key(key("tamper-key"));
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
        let shared = key("shared");
        let dev_store = DevSessionStore::with_key(shared.clone());
        let acct_store = MagicLinkSessionStore::with_key(shared);
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
        let store = LoginTokenStore::with_key(key("login-test-key"));
        let account_id = AccountId::generate();
        let token = store.issue_login_token(&account_id);
        let result = store.verify_login_token(&token);
        assert_eq!(result.as_ref(), Some(&account_id), "fresh login token must verify");
    }

    #[test]
    fn login_token_single_use() {
        let store = LoginTokenStore::with_key(key("login-single-use-key"));
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
        let store = LoginTokenStore::with_key_and_ttl(key("expire-key"), Duration::from_secs(0));
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
        let store_a = LoginTokenStore::with_key(key("login-key-a"));
        let store_b = LoginTokenStore::with_key(key("login-key-b"));
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
        let login_store = LoginTokenStore::with_key(key("roundtrip-key"));
        let session_store = MagicLinkSessionStore::with_key(key("roundtrip-session-key"));
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

        let login_store = LoginTokenStore::with_key(key("mail-test-key"));
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
