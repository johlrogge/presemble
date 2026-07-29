//! Control plane shared state; HTTP handlers + dashboard are added in the next Task 3.1 step.
//!
//! `AppState` holds all shared dependencies for the control plane service.
//! It is constructed once at startup via dependency injection and is intended to
//! be shared across request handlers (when they are added in the next step).

use std::sync::Arc;

/// Shared state for the control plane service.
///
/// All fields are either `Arc<dyn Trait>` for polymorphic dependencies or
/// concrete value types that are themselves internally thread-safe.
pub struct AppState {
    /// Lifecycle manager for per-account `presemble serve` conductor processes.
    pub provisioner: Arc<dyn provisioning::ConductorProvisioner>,
    /// Source of truth for site registrations, placements, and port allocation.
    pub registry: Arc<site_registry::SiteRegistry>,
    /// HMAC-signed session store for real account sessions (V1 magic-link flow).
    pub sessions: accounts::MagicLinkSessionStore,
    /// Single-use HMAC-signed login token store (magic-link email tokens).
    pub login_tokens: accounts::LoginTokenStore,
    /// Account lookup / create-on-first-login.
    pub accounts: Arc<dyn accounts::AccountStore>,
    /// Mail sender for dispatching magic-link emails.
    pub mail: Arc<dyn accounts::MailSender>,
}

impl AppState {
    /// Construct an `AppState` from explicit dependencies (dependency injection).
    ///
    /// All heavy construction (registry hydration, provisioner config) is done
    /// by the caller (`main`) before passing the fully-constructed values here.
    pub fn new(
        provisioner: Arc<dyn provisioning::ConductorProvisioner>,
        registry: Arc<site_registry::SiteRegistry>,
        sessions: accounts::MagicLinkSessionStore,
        login_tokens: accounts::LoginTokenStore,
        accounts: Arc<dyn accounts::AccountStore>,
        mail: Arc<dyn accounts::MailSender>,
    ) -> Self {
        Self {
            provisioner,
            registry,
            sessions,
            login_tokens,
            accounts,
            mail,
        }
    }
}
