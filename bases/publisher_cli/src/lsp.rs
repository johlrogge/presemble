use crate::error::CliError;
use lsp_service::PresembleLsp;
use std::path::Path;
use tower_lsp::{LspService, Server};

pub fn run_lsp_stdio(site_dir: &Path) -> Result<(), CliError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| CliError::Render(format!("failed to create tokio runtime: {e}")))?
        .block_on(async {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            let conductor_client = conductor::ensure_conductor(site_dir)
                .map_err(|e| CliError::Render(format!("failed to start conductor: {e}")))?;
            let (service, socket) = LspService::new(|client| {
                PresembleLsp::new(client, site_dir.to_path_buf(), conductor_client)
            });
            Server::new(stdin, stdout, socket).serve(service).await;
            Ok(())
        })
}
