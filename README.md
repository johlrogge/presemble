# Presemble

A site publisher focused on editorial collaboration and semantic content safety.

Content is data. Templates are data. Schemas are the contracts between them.

## What it does

- **Schema-validated content** — define document grammars in markdown; the publisher enforces them at build time with hard failures and clear error messages
- **Data-bound templates** — templates are parsed DOM trees, not text with holes; structural validity is guaranteed by construction
- **Live serve** — `presemble serve` rebuilds only affected pages on each save and reloads the browser automatically
- **LSP support** — `presemble lsp` provides completions, diagnostics, hover, and go-to-definition for content, template, and schema files in any LSP-capable editor
- **Deployment URL rewriting** — authors write root-relative paths; the publisher rewrites them to relative, root-relative with base path, or absolute at build time

## Quickstart

```
cargo install --path projects/publisher
presemble init my-site/
presemble serve my-site/
```

Open `http://localhost:3000`. Edit any file under `my-site/content/`, `my-site/schemas/`, or `my-site/templates/` — the browser reloads automatically.

To build for deployment:

```
presemble build my-site/
presemble build my-site/ --config .presemble/github-pages.json
```

## Editor support

```
presemble lsp my-site/
```

Point your editor at the `presemble lsp` binary. It handles content, template, and schema files in the same server process, dispatching by file path.

| File type | Completions | Diagnostics | Hover | Go-to-definition |
|---|---|---|---|---|
| `content/*.md` | Slot names, cross-content references | Schema violations | Slot hint text | Content reference targets |
| `templates/*.html` | Data-path completions | Non-existent schema fields | Field hint text | Template `presemble:include` targets |
| `schemas/*.md` | Field type keywords, occurrence values | Parse errors | — | — |

## Architecture

Rust monorepo using [polylith](https://polylith.gitbook.io/polylith/). Components are shared libraries; bases are runtime wiring; projects are deployable binaries.

```
components/
  schema          — schema grammar parser
  content         — document parser and schema validator
  template        — DOM template renderer
  dep_graph       — dependency tracking for incremental builds
  lsp_service     — LSP server implementation (tower-lsp)
  lsp_capabilities — completions, diagnostics, hover, go-to-definition logic

bases/
  publisher_cli   — CLI wiring for build, serve, and lsp modes
  editor_server   — stub base for the multiplayer editing service

projects/
  publisher       — presemble CLI binary
  content_management — long-running editing service (in progress)
```

ADRs live in `docs/adr/`. The presemble.io site is in `site/` and is built with Presemble itself.

## Workspace modules

| Module | README |
|---|---|
| `components/schema` | [components/schema/README.md](components/schema/README.md) |
| `components/content` | [components/content/README.md](components/content/README.md) |
| `components/template` | [components/template/README.md](components/template/README.md) |
| `components/dep_graph` | [components/dep_graph/README.md](components/dep_graph/README.md) |
| `components/lsp_service` | [components/lsp_service/README.md](components/lsp_service/README.md) |
| `components/lsp_capabilities` | [components/lsp_capabilities/README.md](components/lsp_capabilities/README.md) |
| `bases/publisher_cli` | [bases/publisher_cli/README.md](bases/publisher_cli/README.md) |
| `bases/editor_server` | [bases/editor_server/README.md](bases/editor_server/README.md) |
| `projects/publisher` | [projects/publisher/README.md](projects/publisher/README.md) |

## Build and test

```
cargo check          # type-check the workspace
cargo test           # run all tests
cargo clippy         # lint
cargo run -p publisher -- site/    # run the publisher on the dogfood site
```

Requires the Nix devenv shell. Do not install packages with `cargo install -g` or `apt`; add them to `devenv.nix` instead.

## Version

Current release: **0.13.0**

See [ROADMAP.md](ROADMAP.md) for the milestone plan and [RELEASING.md](RELEASING.md) for the release workflow.
