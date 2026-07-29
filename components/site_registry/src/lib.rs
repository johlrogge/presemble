//! `site_registry` component — persisted source of truth for hosted sites.
//!
//! Tracks which account owns which site, its subdomain, lifecycle status, and
//! placement (host + port + on-disk site path its per-account conductor runs at).
//! The registry is the source the control plane replays on startup to re-inject
//! Caddy routes, and the serialisation point for placement/port allocation.
//!
//! # Design
//!
//! - All types are newtypes over stringly-typed raw values.
//! - `Subdomain` is validated at construction time.
//! - Persistence is a single JSON file guarded by a `Mutex` (single-writer).
//! - The `SiteStore` trait is the persistence seam so a DB can replace the file
//!   backend later without changing callers.

use accounts::AccountId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("subdomain already registered: {0}")]
    DuplicateSubdomain(String),

    #[error("site not found: {0}")]
    NotFound(String),

    #[error("invalid status transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: SiteStatus,
        to: SiteStatus,
    },

    #[error("port range exhausted")]
    PortRangeExhausted,

    #[error("persistence error: {0}")]
    Persistence(String),
}

// ── SiteId ────────────────────────────────────────────────────────────────────

/// Opaque identifier for a hosted site. Not stringly-typed at call sites.
/// Internally a 32-char lowercase-hex string (16 random bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SiteId(String);

impl SiteId {
    /// Generate a fresh random `SiteId`.
    pub fn generate() -> Self {
        let bytes: [u8; 16] = rand::random();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Self(hex)
    }

    /// Expose the inner string (e.g. to embed in a URL or store in a DB).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse a `SiteId` from a previously serialised string.
    /// Returns `None` if the input is not 32 hex chars.
    pub fn parse(s: &str) -> Option<Self> {
        if s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }
}

impl std::fmt::Display for SiteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Subdomain ─────────────────────────────────────────────────────────────────

/// A validated DNS-label-safe subdomain.
///
/// Rules (RFC 952 / RFC 1123):
/// - Only lowercase ASCII alphanumeric characters and hyphens.
/// - Must not start or end with a hyphen.
/// - Length: 1–63 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Subdomain(String);

impl Subdomain {
    /// Attempt to construct a `Subdomain`, validating DNS-label safety.
    pub fn new(s: impl AsRef<str>) -> Result<Self, SubdomainError> {
        let s = s.as_ref();
        if s.is_empty() {
            return Err(SubdomainError::Empty);
        }
        if s.len() > 63 {
            return Err(SubdomainError::TooLong(s.len()));
        }
        if s.starts_with('-') || s.ends_with('-') {
            return Err(SubdomainError::LeadingOrTrailingHyphen);
        }
        if !s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-')) {
            return Err(SubdomainError::InvalidChars);
        }
        Ok(Self(s.to_string()))
    }

    /// Expose the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Subdomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors produced by `Subdomain::new`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubdomainError {
    #[error("subdomain must not be empty")]
    Empty,
    #[error("subdomain is too long ({0} chars; max 63)")]
    TooLong(usize),
    #[error("subdomain must not start or end with a hyphen")]
    LeadingOrTrailingHyphen,
    #[error("subdomain may only contain lowercase letters, digits, and hyphens")]
    InvalidChars,
}

// ── SiteStatus ────────────────────────────────────────────────────────────────

/// Lifecycle state of a hosted site.
///
/// State machine:
/// ```text
/// Provisioning → Ready
/// Provisioning → Failed
/// Ready        → Stopped
/// Ready        → Failed
/// Stopped      → Ready   (restart)
/// Stopped      → Failed
/// Failed       → Provisioning  (retry)
/// ```
/// All other transitions are rejected by `SiteStatus::transition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteStatus {
    Provisioning,
    Ready,
    Stopped,
    Failed,
}

impl SiteStatus {
    /// Validate and perform a status transition.
    ///
    /// Returns `Ok(next)` if the transition is valid, or
    /// `Err(RegistryError::InvalidTransition)` otherwise.
    pub fn transition(self, next: SiteStatus) -> Result<SiteStatus, RegistryError> {
        let valid = matches!(
            (self, next),
            (SiteStatus::Provisioning, SiteStatus::Ready)
                | (SiteStatus::Provisioning, SiteStatus::Failed)
                | (SiteStatus::Ready, SiteStatus::Stopped)
                | (SiteStatus::Ready, SiteStatus::Failed)
                | (SiteStatus::Stopped, SiteStatus::Ready)
                | (SiteStatus::Stopped, SiteStatus::Failed)
                | (SiteStatus::Failed, SiteStatus::Provisioning)
        );
        if valid {
            Ok(next)
        } else {
            Err(RegistryError::InvalidTransition {
                from: self,
                to: next,
            })
        }
    }
}

impl std::fmt::Display for SiteStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Provisioning => "Provisioning",
            Self::Ready => "Ready",
            Self::Stopped => "Stopped",
            Self::Failed => "Failed",
        };
        f.write_str(s)
    }
}

// ── Placement ─────────────────────────────────────────────────────────────────

/// Where the per-account conductor process runs.
///
/// `site_path` must be an absolute path; the conductor derives its IPC socket
/// path from it, so distinct accounts must have distinct absolute paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub host: String,
    pub port: u16,
    pub site_path: PathBuf,
}

// ── Site ──────────────────────────────────────────────────────────────────────

/// A hosted site record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    /// The account that owns this site.
    pub account_id: AccountId,
    pub subdomain: Subdomain,
    pub status: SiteStatus,
    pub placement: Option<Placement>,
}

// ── SiteStore trait (persistence seam) ────────────────────────────────────────

/// Persistence backend trait. The `FileSiteStore` is the V1 implementation.
/// A database-backed implementation can be swapped in later without changing
/// `SiteRegistry`.
///
/// All methods operate under a `Mutex` guard supplied by `SiteRegistry`,
/// so implementations do NOT need to be independently thread-safe.
pub trait SiteStore: Send + 'static {
    /// Load all sites from the backing store.
    fn load(&self) -> Result<Vec<Site>, RegistryError>;
    /// Persist the full site list (overwrite).
    fn save(&self, sites: &[Site]) -> Result<(), RegistryError>;
}

// ── FileSiteStore ─────────────────────────────────────────────────────────────

/// JSON-file-backed persistence. Single writer; reads reconstruct from disk.
pub struct FileSiteStore {
    path: PathBuf,
}

impl FileSiteStore {
    /// Create (or open) a `FileSiteStore` at the given path.
    /// The file will be created on the first `save`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl SiteStore for FileSiteStore {
    fn load(&self) -> Result<Vec<Site>, RegistryError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let contents = std::fs::read_to_string(&self.path)
            .map_err(|e| RegistryError::Persistence(format!("read {}: {e}", self.path.display())))?;
        serde_json::from_str(&contents)
            .map_err(|e| RegistryError::Persistence(format!("parse {}: {e}", self.path.display())))
    }

    fn save(&self, sites: &[Site]) -> Result<(), RegistryError> {
        let json = serde_json::to_string_pretty(sites)
            .map_err(|e| RegistryError::Persistence(format!("serialise: {e}")))?;
        // Write to a sibling temp file then rename for atomicity.
        // NOTE: this atomicity relies on the `SiteRegistry` mutex serialising all
        // `save` calls; two `FileSiteStore` instances sharing the same path would
        // race the temp file and corrupt the registry.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &json)
            .map_err(|e| RegistryError::Persistence(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            RegistryError::Persistence(format!("rename to {}: {e}", self.path.display()))
        })
    }
}

// ── SiteRegistry ─────────────────────────────────────────────────────────────

/// Thread-safe site registry. All mutations go through a single `Mutex` so that
/// concurrent `register_site` calls are serialised and no SiteId is lost.
pub struct SiteRegistry {
    /// Single-writer mutex; guards both the in-memory index and the backing store.
    inner: Mutex<RegistryInner>,
}

struct RegistryInner {
    sites: HashMap<SiteId, Site>,
    store: Box<dyn SiteStore>,
}

impl SiteRegistry {
    /// Create a registry backed by the given store and hydrate from it.
    pub fn new(store: impl SiteStore + 'static) -> Result<Arc<Self>, RegistryError> {
        let sites_vec = store.load()?;
        let sites: HashMap<SiteId, Site> =
            sites_vec.into_iter().map(|s| (s.id.clone(), s)).collect();
        let inner = RegistryInner {
            sites,
            store: Box::new(store),
        };
        Ok(Arc::new(Self {
            inner: Mutex::new(inner),
        }))
    }

    // ── Write operations (all serialised through the mutex) ───────────────────

    /// Register a new site for the given account under `subdomain`.
    ///
    /// Mints a fresh `SiteId`, sets status to `Provisioning`, and persists.
    /// Returns `Err(DuplicateSubdomain)` if the subdomain is already taken.
    pub fn register_site(
        &self,
        account: &AccountId,
        subdomain: Subdomain,
    ) -> Result<Site, RegistryError> {
        let mut inner = self.inner.lock().expect("site registry lock poisoned");
        // Reject duplicate subdomains
        if inner
            .sites
            .values()
            .any(|s| s.subdomain == subdomain)
        {
            return Err(RegistryError::DuplicateSubdomain(
                subdomain.as_str().to_string(),
            ));
        }
        let site = Site {
            id: SiteId::generate(),
            account_id: account.clone(),
            subdomain,
            status: SiteStatus::Provisioning,
            placement: None,
        };
        inner.sites.insert(site.id.clone(), site.clone());
        let sites: Vec<Site> = inner.sites.values().cloned().collect();
        inner.store.save(&sites)?;
        Ok(site)
    }

    /// Transition a site to a new status.
    ///
    /// Enforces the state machine via `SiteStatus::transition`.
    pub fn set_status(
        &self,
        site_id: &SiteId,
        new_status: SiteStatus,
    ) -> Result<(), RegistryError> {
        let mut inner = self.inner.lock().expect("site registry lock poisoned");
        let site = inner
            .sites
            .get_mut(site_id)
            .ok_or_else(|| RegistryError::NotFound(site_id.to_string()))?;
        site.status = site.status.transition(new_status)?;
        let sites: Vec<Site> = inner.sites.values().cloned().collect();
        inner.store.save(&sites)
    }

    /// Attach a placement to a site (idempotent overwrite).
    pub fn set_placement(
        &self,
        site_id: &SiteId,
        placement: Placement,
    ) -> Result<(), RegistryError> {
        let mut inner = self.inner.lock().expect("site registry lock poisoned");
        let site = inner
            .sites
            .get_mut(site_id)
            .ok_or_else(|| RegistryError::NotFound(site_id.to_string()))?;
        site.placement = Some(placement);
        let sites: Vec<Site> = inner.sites.values().cloned().collect();
        inner.store.save(&sites)
    }

    // ── Read operations ───────────────────────────────────────────────────────

    /// Resolve a subdomain to its placement (for Caddy routing).
    pub fn placement_for(&self, subdomain: &Subdomain) -> Option<Placement> {
        let inner = self.inner.lock().expect("site registry lock poisoned");
        inner
            .sites
            .values()
            .find(|s| &s.subdomain == subdomain)
            .and_then(|s| s.placement.clone())
    }

    /// All sites belonging to an account.
    pub fn sites_for(&self, account: &AccountId) -> Vec<Site> {
        let inner = self.inner.lock().expect("site registry lock poisoned");
        inner
            .sites
            .values()
            .filter(|s| &s.account_id == account)
            .cloned()
            .collect()
    }

    /// All placements (for startup route re-injection). Only sites with a
    /// placement are returned.
    pub fn all_placements(&self) -> Vec<(Subdomain, Placement)> {
        let inner = self.inner.lock().expect("site registry lock poisoned");
        inner
            .sites
            .values()
            .filter_map(|s| s.placement.clone().map(|p| (s.subdomain.clone(), p)))
            .collect()
    }

    /// Allocate the next free port in `range` (not already held by a placement).
    ///
    /// Returns `Err(PortRangeExhausted)` if every port in the range is taken.
    pub fn allocate_port(&self, mut range: Range<u16>) -> Result<u16, RegistryError> {
        let inner = self.inner.lock().expect("site registry lock poisoned");
        let used: std::collections::HashSet<u16> = inner
            .sites
            .values()
            .filter_map(|s| s.placement.as_ref().map(|p| p.port))
            .collect();
        range
            .find(|p| !used.contains(p))
            .ok_or(RegistryError::PortRangeExhausted)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn temp_registry() -> (Arc<SiteRegistry>, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");
        let reg = SiteRegistry::new(FileSiteStore::new(&path)).unwrap();
        (reg, dir)
    }

    fn make_account() -> AccountId {
        AccountId::generate()
    }

    fn sub(s: &str) -> Subdomain {
        Subdomain::new(s).unwrap()
    }

    // ── Subdomain validation ──────────────────────────────────────────────────

    #[test]
    fn subdomain_valid_labels_accepted() {
        assert!(Subdomain::new("hello").is_ok());
        assert!(Subdomain::new("hello-world").is_ok());
        assert!(Subdomain::new("my-site-123").is_ok());
        assert!(Subdomain::new("a").is_ok());
        assert!(Subdomain::new("a".repeat(63)).is_ok());
    }

    #[test]
    fn subdomain_rejects_empty() {
        assert_eq!(Subdomain::new(""), Err(SubdomainError::Empty));
    }

    #[test]
    fn subdomain_rejects_too_long() {
        let long = "a".repeat(64);
        assert!(matches!(Subdomain::new(&long), Err(SubdomainError::TooLong(_))));
    }

    #[test]
    fn subdomain_rejects_leading_hyphen() {
        assert_eq!(
            Subdomain::new("-bad"),
            Err(SubdomainError::LeadingOrTrailingHyphen)
        );
    }

    #[test]
    fn subdomain_rejects_trailing_hyphen() {
        assert_eq!(
            Subdomain::new("bad-"),
            Err(SubdomainError::LeadingOrTrailingHyphen)
        );
    }

    #[test]
    fn subdomain_rejects_uppercase() {
        assert_eq!(Subdomain::new("BadLabel"), Err(SubdomainError::InvalidChars));
    }

    #[test]
    fn subdomain_rejects_underscores() {
        assert_eq!(
            Subdomain::new("bad_label"),
            Err(SubdomainError::InvalidChars)
        );
    }

    #[test]
    fn subdomain_rejects_dots() {
        assert_eq!(Subdomain::new("bad.label"), Err(SubdomainError::InvalidChars));
    }

    // ── SiteId ───────────────────────────────────────────────────────────────

    #[test]
    fn site_id_generate_is_32_hex() {
        let id = SiteId::generate();
        assert_eq!(id.as_str().len(), 32);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn site_id_parse_roundtrip() {
        let id = SiteId::generate();
        let parsed = SiteId::parse(id.as_str());
        assert_eq!(parsed.as_ref(), Some(&id));
    }

    #[test]
    fn site_id_parse_rejects_invalid() {
        assert!(SiteId::parse("tooshort").is_none());
        assert!(SiteId::parse("not-hex-not-hex-not-hex-not-hex!").is_none());
        assert!(SiteId::parse("").is_none());
    }

    // ── SiteStatus transitions ────────────────────────────────────────────────

    #[test]
    fn status_valid_transitions_succeed() {
        use SiteStatus::*;
        assert_eq!(Provisioning.transition(Ready), Ok(Ready));
        assert_eq!(Provisioning.transition(Failed), Ok(Failed));
        assert_eq!(Ready.transition(Stopped), Ok(Stopped));
        assert_eq!(Ready.transition(Failed), Ok(Failed));
        assert_eq!(Stopped.transition(Ready), Ok(Ready));
        assert_eq!(Stopped.transition(Failed), Ok(Failed));
        assert_eq!(Failed.transition(Provisioning), Ok(Provisioning));
    }

    #[test]
    fn status_invalid_transitions_fail() {
        use SiteStatus::*;
        // Not an exhaustive list — spot-check nonsensical transitions.
        assert!(Provisioning.transition(Stopped).is_err());
        assert!(Ready.transition(Provisioning).is_err());
        assert!(Ready.transition(Ready).is_err());
        assert!(Stopped.transition(Provisioning).is_err());
        assert!(Failed.transition(Stopped).is_err());
        assert!(Failed.transition(Ready).is_err());
    }

    // ── register_site ─────────────────────────────────────────────────────────

    #[test]
    fn register_site_creates_provisioning_site() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("mysite")).unwrap();
        assert_eq!(site.status, SiteStatus::Provisioning);
        assert!(site.placement.is_none());
        assert_eq!(site.subdomain, sub("mysite"));
        assert_eq!(site.account_id, account);
    }

    #[test]
    fn register_site_rejects_duplicate_subdomain() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        reg.register_site(&account, sub("taken")).unwrap();
        let err = reg.register_site(&account, sub("taken")).unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateSubdomain(_)));
    }

    #[test]
    fn register_site_allows_different_subdomains() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        reg.register_site(&account, sub("one")).unwrap();
        reg.register_site(&account, sub("two")).unwrap(); // must not error
    }

    // ── set_status ────────────────────────────────────────────────────────────

    #[test]
    fn set_status_valid_transition() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("statussite")).unwrap();
        reg.set_status(&site.id, SiteStatus::Ready).unwrap();
        let sites = reg.sites_for(&account);
        assert_eq!(sites[0].status, SiteStatus::Ready);
    }

    #[test]
    fn set_status_invalid_transition_returns_error() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("badtrans")).unwrap();
        let err = reg.set_status(&site.id, SiteStatus::Stopped).unwrap_err();
        assert!(matches!(err, RegistryError::InvalidTransition { .. }));
    }

    #[test]
    fn set_status_unknown_site_returns_not_found() {
        let (reg, _dir) = temp_registry();
        let unknown = SiteId::generate();
        let err = reg.set_status(&unknown, SiteStatus::Ready).unwrap_err();
        assert!(matches!(err, RegistryError::NotFound(_)));
    }

    // ── set_placement ─────────────────────────────────────────────────────────

    #[test]
    fn set_placement_attaches_placement() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("placed")).unwrap();
        let placement = Placement {
            host: "127.0.0.1".to_string(),
            port: 3101,
            site_path: PathBuf::from("/data/accounts/abc/site"),
        };
        reg.set_placement(&site.id, placement.clone()).unwrap();
        let sites = reg.sites_for(&account);
        assert_eq!(sites[0].placement, Some(placement));
    }

    // ── placement_for ─────────────────────────────────────────────────────────

    #[test]
    fn placement_for_returns_correct_placement() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("caddy")).unwrap();
        let placement = Placement {
            host: "127.0.0.1".to_string(),
            port: 3200,
            site_path: PathBuf::from("/data/accounts/xyz/site"),
        };
        reg.set_placement(&site.id, placement.clone()).unwrap();
        let found = reg.placement_for(&sub("caddy"));
        assert_eq!(found, Some(placement));
    }

    #[test]
    fn placement_for_returns_none_when_no_placement() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        reg.register_site(&account, sub("noplace")).unwrap();
        assert!(reg.placement_for(&sub("noplace")).is_none());
    }

    #[test]
    fn placement_for_returns_none_for_unknown_subdomain() {
        let (reg, _dir) = temp_registry();
        assert!(reg.placement_for(&sub("ghost")).is_none());
    }

    // ── sites_for ─────────────────────────────────────────────────────────────

    #[test]
    fn sites_for_returns_only_matching_account_sites() {
        let (reg, _dir) = temp_registry();
        let a = make_account();
        let b = make_account();
        reg.register_site(&a, sub("asite1")).unwrap();
        reg.register_site(&a, sub("asite2")).unwrap();
        reg.register_site(&b, sub("bsite1")).unwrap();
        let sites_a = reg.sites_for(&a);
        let sites_b = reg.sites_for(&b);
        assert_eq!(sites_a.len(), 2);
        assert_eq!(sites_b.len(), 1);
    }

    // ── all_placements ────────────────────────────────────────────────────────

    #[test]
    fn all_placements_returns_only_placed_sites() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let s1 = reg.register_site(&account, sub("placed1")).unwrap();
        let _s2 = reg.register_site(&account, sub("unplaced")).unwrap();
        let placement = Placement {
            host: "127.0.0.1".to_string(),
            port: 3300,
            site_path: PathBuf::from("/data/accounts/p1/site"),
        };
        reg.set_placement(&s1.id, placement.clone()).unwrap();
        let all = reg.all_placements();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, sub("placed1"));
        assert_eq!(all[0].1, placement);
    }

    // ── allocate_port ─────────────────────────────────────────────────────────

    #[test]
    fn allocate_port_skips_used_ports() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let s1 = reg.register_site(&account, sub("p1")).unwrap();
        let s2 = reg.register_site(&account, sub("p2")).unwrap();
        reg.set_placement(
            &s1.id,
            Placement {
                host: "127.0.0.1".to_string(),
                port: 3100,
                site_path: PathBuf::from("/data/a1/site"),
            },
        )
        .unwrap();
        reg.set_placement(
            &s2.id,
            Placement {
                host: "127.0.0.1".to_string(),
                port: 3101,
                site_path: PathBuf::from("/data/a2/site"),
            },
        )
        .unwrap();
        // 3100 and 3101 are taken; first free is 3102
        let port = reg.allocate_port(3100..4000).unwrap();
        assert_eq!(port, 3102);
    }

    #[test]
    fn allocate_port_exhausted_range_errors() {
        let (reg, _dir) = temp_registry();
        let account = make_account();
        let site = reg.register_site(&account, sub("only")).unwrap();
        reg.set_placement(
            &site.id,
            Placement {
                host: "127.0.0.1".to_string(),
                port: 3100,
                site_path: PathBuf::from("/data/only/site"),
            },
        )
        .unwrap();
        // Range of exactly one port, already used
        let err = reg.allocate_port(3100..3101).unwrap_err();
        assert!(matches!(err, RegistryError::PortRangeExhausted));
    }

    // ── Round-trip persistence ────────────────────────────────────────────────

    #[test]
    fn round_trip_persist_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("registry.json");

        // Populate
        {
            let reg = SiteRegistry::new(FileSiteStore::new(&path)).unwrap();
            let account = AccountId::parse("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4").unwrap();
            let site = reg.register_site(&account, sub("persist-me")).unwrap();
            reg.set_status(&site.id, SiteStatus::Ready).unwrap();
            reg.set_placement(
                &site.id,
                Placement {
                    host: "127.0.0.1".to_string(),
                    port: 3999,
                    site_path: PathBuf::from("/data/accounts/persist/site"),
                },
            )
            .unwrap();
        }

        // Reload
        {
            let reg = SiteRegistry::new(FileSiteStore::new(&path)).unwrap();
            let account = AccountId::parse("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4").unwrap();
            let sites = reg.sites_for(&account);
            assert_eq!(sites.len(), 1);
            let site = &sites[0];
            assert_eq!(site.subdomain, sub("persist-me"));
            assert_eq!(site.status, SiteStatus::Ready);
            let placement = site.placement.as_ref().unwrap();
            assert_eq!(placement.port, 3999);
            assert_eq!(
                placement.site_path,
                PathBuf::from("/data/accounts/persist/site")
            );
        }
    }

    // ── Concurrent registration ───────────────────────────────────────────────

    #[test]
    fn concurrent_register_no_lost_writes_no_duplicate_ids() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("concurrent.json");
        let reg = SiteRegistry::new(FileSiteStore::new(&path)).unwrap();
        let reg = Arc::new(reg);

        let handles: Vec<_> = (0..20_u32)
            .map(|i| {
                let reg = Arc::clone(&reg);
                let account = AccountId::generate();
                std::thread::spawn(move || {
                    let label = format!("site-{i}");
                    reg.register_site(&account, Subdomain::new(&label).unwrap())
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        // All registrations must succeed
        for r in &results {
            assert!(r.is_ok(), "concurrent registration should not fail: {r:?}");
        }

        // All SiteIds must be unique
        let ids: Vec<SiteId> = results.into_iter().map(|r| r.unwrap().id).collect();
        let unique: std::collections::HashSet<&SiteId> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "all SiteIds must be unique");
    }
}
