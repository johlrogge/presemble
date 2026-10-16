use crate::error::CliError;
use crate::site_dir::SiteDir;
use lsp_service::PresembleLsp;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower_lsp::{LspService, Server};

/// Run the language server over stdio. With `arg`, it serves that site;
/// without, the workspace root the editor sends or the first file opened
/// chooses it, and it connects to that site's conductor then. A site
/// directory that is not one, or a conductor that cannot be started, does
/// not end it: the editor is told why, and every request is answered empty.
pub fn run_lsp_stdio(arg: Option<&Path>) -> Result<(), CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Render(format!("failed to create tokio runtime: {e}")))?
        .block_on(async {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            let started = start(arg);
            if let Err(reason) = &started {
                eprintln!("presemble-lsp: {reason}");
            }
            let (service, socket) = LspService::new(|client| match started {
                Ok(site) => {
                    PresembleLsp::waiting(client, site, Arc::new(conductor::ensure_conductor))
                }
                Err(reason) => PresembleLsp::unavailable(client, reason),
            });
            Server::new(stdin, stdout, socket).serve(service).await;
            Ok(())
        })
}

/// The site the server starts with: `Ok(Some(site))` for an argument that is
/// a site directory, `Ok(None)` without one (the editor's files choose it),
/// or why the argument is not one.
fn start(arg: Option<&Path>) -> Result<Option<PathBuf>, String> {
    arg.map(|path| {
        SiteDir::with_schemas(path)
            .map(|site| site.path().to_path_buf())
            .map_err(|error| error.to_string())
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_site_directory_reaches_the_editor_without_starting_a_conductor() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("demo");

        let reason = start(Some(&missing)).expect_err("a missing site is refused");

        assert!(reason.contains(&missing.display().to_string()), "{reason}");
        assert!(reason.contains("does not exist"), "{reason}");
    }

    #[test]
    fn without_an_argument_the_server_waits_for_a_file() {
        assert_eq!(start(None), Ok(None));
    }

    #[test]
    fn a_site_argument_is_the_site() {
        let tmp = tempfile::tempdir().unwrap();
        let site = tmp.path().join("site");
        std::fs::create_dir_all(site.join("schemas")).unwrap();
        assert_eq!(start(Some(&site)), Ok(Some(site)));
    }
}
