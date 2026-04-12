# publisher_cli

CLI wiring for all `presemble` subcommands.

Implements the `presemble` subcommand dispatch using `clap`. Ties together the `schema`, `content`, `template`, `dep_graph`, `lsp_service`, `conductor`, and related components into the runtime modes.

## Commands

| Command | Description |
|---|---|
| `presemble build <site-dir>` | Full or incremental build; writes output to a sibling `output/` directory |
| `presemble build <site-dir> --config <file>` | Build with a named URL config (e.g. `.presemble/github-pages.json`) |
| `presemble serve <site-dir>` | Local HTTP server on port 3000 with file watching and live reload over WebSocket; starts the conductor daemon |
| `presemble lsp <site-dir>` | LSP server over stdio — handles content, template, and schema files; delegates to conductor |
| `presemble mcp <site-dir>` | MCP server for Claude Code integration; each tool call accepts an optional `site` parameter |
| `presemble nrepl <site-dir>` | nREPL server for Calva, CIDER, and `rep` |
| `presemble init <dir>` | Scaffold a hello-world site |

## Used by

`publisher` project

---

[Back to root README](../../README.md)
