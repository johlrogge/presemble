//! Control plane binary entry point.
//!
//! Constructs `AppState` from real infrastructure seams and exits cleanly.
//! HTTP handlers and the dashboard are added in the next Task 3.1 step.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // ── Registry path ─────────────────────────────────────────────────────────

    let registry_path = std::env::var("PRESEMBLE_REGISTRY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/presemble/registry.json"));

    let registry = site_registry::SiteRegistry::new(
        site_registry::FileSiteStore::new(&registry_path),
    )
    .expect("failed to initialise site registry");

    // ── Session key ───────────────────────────────────────────────────────────
    // This single string is the ONLY source of the signing key.  It is fed to
    // MagicLinkSessionStore, LoginTokenStore, AND SharedVmProvisioner so they
    // can never drift (ADR-055).  No hardcoded fallback is permitted.

    let session_key_str = match std::env::var("PRESEMBLE_SESSION_KEY") {
        Ok(val) if !val.is_empty() => val,
        _ => {
            eprintln!(
                "error: PRESEMBLE_SESSION_KEY must be set \
                 (the shared session-signing key; see ADR-055)"
            );
            std::process::exit(1);
        }
    };

    // ── Real provisioning seams ───────────────────────────────────────────────

    let launcher: Arc<dyn provisioning::ConductorLauncher> =
        Arc::new(provisioning::TokioProcessLauncher::new());

    let health: Arc<dyn provisioning::HealthChecker> =
        Arc::new(provisioning::HttpHealthChecker::new());

    let base_domain = std::env::var("PRESEMBLE_BASE_DOMAIN")
        .unwrap_or_else(|_| "presemble.pub".to_string());

    let router: Arc<dyn provisioning::CaddyRouter> =
        Arc::new(provisioning::HttpCaddyRouter::new(base_domain));

    let accounts_root = std::env::var("PRESEMBLE_ACCOUNTS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/presemble/accounts"));

    let provisioner = provisioning::SharedVmProvisioner::new(provisioning::SharedVmConfig {
        registry: Arc::clone(&registry),
        launcher,
        health,
        router,
        session_key: provisioning::SessionKey::from(session_key_str.clone()),
        port_range: 3100..4000,
        accounts_root,
        host: "127.0.0.1".to_string(),
        health_timeout: Duration::from_secs(30),
    });

    // ── Accounts stores ───────────────────────────────────────────────────────

    let sessions = accounts::MagicLinkSessionStore::with_key(session_key_str.as_bytes().to_vec());
    // LoginTokenStore's `consumed` set is per-process memory; the control plane
    // must remain single-instance unless this is moved to a shared store (ADR-055).
    let login_tokens = accounts::LoginTokenStore::with_key(session_key_str.as_bytes().to_vec());
    // Alpha placeholders — replace with a persistent AccountStore and a real
    // MailSender before any production deployment.
    let account_store: Arc<dyn accounts::AccountStore> =
        Arc::new(accounts::InMemoryAccountStore::new());
    let mail: Arc<dyn accounts::MailSender> = Arc::new(accounts::ConsoleMailSender::new());

    // ── Assemble AppState ─────────────────────────────────────────────────────

    let _state = control_plane::AppState::new(
        provisioner,
        registry,
        sessions,
        login_tokens,
        account_store,
        mail,
    );

    println!("control_plane initialized");
}
