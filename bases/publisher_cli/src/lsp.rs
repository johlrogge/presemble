use crate::error::CliError;
use crate::site_dir::SiteDir;
use lsp_service::{ConductorLink, PresembleLsp};
use std::path::Path;
use tower_lsp::{LspService, Server};

/// Run the language server over stdio for the site at `site_dir`. A site
/// directory that is not one, or a conductor that cannot be started, does
/// not end it: the editor is told why, and every request is answered empty.
pub fn run_lsp_stdio(site_dir: &Path) -> Result<(), CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Render(format!("failed to create tokio runtime: {e}")))?
        .block_on(async {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            let link = conductor_link(site_dir, conductor::ensure_conductor);
            if let ConductorLink::Unavailable(reason) = &link {
                eprintln!("presemble-lsp: {reason}");
            }
            let (service, socket) =
                LspService::new(|client| PresembleLsp::new(client, site_dir.to_path_buf(), link));
            Server::new(stdin, stdout, socket).serve(service).await;
            Ok(())
        })
}

/// The language server's line to the conductor of the site at `path`,
/// connected with `connect`. A path that is not a site directory is
/// unavailable at once, naming the path: no conductor is started for it.
fn conductor_link(
    path: &Path,
    connect: impl FnOnce(&Path) -> Result<conductor::ConductorClient, String>,
) -> ConductorLink {
    let site = match SiteDir::with_schemas(path) {
        Ok(site) => site,
        Err(error) => return ConductorLink::Unavailable(error.to_string()),
    };
    match connect(site.path()) {
        Ok(client) => ConductorLink::Connected(client),
        Err(e) => {
            ConductorLink::Unavailable(format!("no conductor for {}: {e}", site.path().display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_site_directory_reaches_the_editor_without_starting_a_conductor() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("demo");

        let link = conductor_link(&missing, |_| panic!("no conductor is started"));

        let ConductorLink::Unavailable(reason) = link else {
            panic!("the link is unavailable");
        };
        assert!(reason.contains(&missing.display().to_string()), "{reason}");
        assert!(reason.contains("does not exist"), "{reason}");
    }
}
