# publisher_cli

CLI wiring for build, serve, and lsp modes.

Implements the `presemble` subcommand dispatch using `clap`. Ties together the `schema`, `content`, `template`, `dep_graph`, and `lsp_service` components into the three runtime modes.

## Commands

| Command | Description |
|---|---|
| `presemble build <site-dir>` | Full or incremental build; writes output to a sibling `output/` directory |
| `presemble build <site-dir> --config <file>` | Build with a named URL config (e.g. `.presemble/github-pages.json`) |
| `presemble serve <site-dir>` | Local HTTP server on port 3000 with file watching and live reload over WebSocket |
| `presemble lsp <site-dir>` | LSP server over stdio — handles content, template, and schema files |
| `presemble init <dir>` | Scaffold a hello-world site |

## Used by

`publisher` project

---

[Back to root README](../../README.md)
