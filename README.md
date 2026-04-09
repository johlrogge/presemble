# Presemble

A site publisher focused on editorial collaboration and semantic content safety.

Content is data. Templates are data. Schemas are the contracts between them.

## What it does

- **Schema-validated content** — define document grammars in markdown; the publisher enforces them at build time with hard failures and clear error messages
- **Data-bound templates** — templates are parsed DOM trees, not text with holes; written in Hiccup (EDN) and composed with `juxt` and pipe combinators
- **Live serve** — `presemble serve` rebuilds only affected pages on each save and reloads the browser automatically; click body elements to edit inline
- **LSP support** — `presemble lsp` provides completions, diagnostics, hover, and go-to-definition for content, template, and schema files in any LSP-capable editor
- **Editorial suggestions** — Claude and human editors push suggestions via the MCP server or conductor; each appears as an LSP diagnostic with accept/reject code actions
- **Presemble Lisp** — content files include link expressions that assemble collections at build time: `(->> :post (sort-by :published :desc) (take 5))`. Reverse references (`(refs-to self)`) populate a slot with all pages that link to the current page.
- **nREPL** — Calva and CIDER can jack in and evaluate expressions against the live content graph
- **Site wizard** — point `presemble serve` at an empty directory for a browser-based starter scaffold
- **Deployment URL rewriting** — authors write root-relative paths; the publisher rewrites them to relative, root-relative with base path, or absolute at build time

## Quickstart

```
cargo install --path projects/publisher
presemble serve my-site/   # opens browser wizard for empty dirs
```

Open `http://localhost:3000`. Pick a starter template in the browser wizard, or edit files under `my-site/content/`, `my-site/schemas/`, or `my-site/templates/` — the browser reloads automatically.

To scaffold without the wizard:

```
presemble init my-site/
presemble serve my-site/
```

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
| `templates/*.hiccup` | Data-path completions | Non-existent schema fields | Field hint text | Template include targets |
| `schemas/*.md` | Field type keywords, occurrence values | Parse errors | — | — |

## Claude integration

```
presemble mcp my-site/
```

Starts an MCP server for Claude Code. Claude reads schemas, reads content, and pushes suggestions to specific slots with rationale. Each suggestion appears as an LSP diagnostic. Accept or reject with a code action. Suggestions also appear as inline diffs in the browser preview.

## nREPL

```
presemble nrepl my-site/
```

Starts an nREPL server. Connect with Calva, CIDER, or `rep` and evaluate Presemble Lisp expressions against the live site graph.

## Architecture

Rust monorepo using [polylith](https://polylith.gitbook.io/polylith/). Components are shared libraries; bases are runtime wiring; projects are deployable binaries.

```
components/
  schema           — schema grammar parser
  content          — document parser and schema validator
  template         — DOM template renderer
  dep_graph        — dependency tracking for incremental builds
  lsp_service      — LSP server implementation (tower-lsp)
  lsp_capabilities — completions, diagnostics, hover, go-to-definition logic
  conductor        — serve orchestrator, dirty buffer, suggestion routing
  content_editor   — business logic for browser editing and scaffold
  serve_ui         — JS/CSS for the browser editing overlay
  site_templates   — embedded starter sites for the wizard
  forms            — EDN form reader
  reader           — Presemble Lisp reader (EDN-based)
  macros           — macro expander (-> and ->> threading)
  evaluator        — Presemble Lisp evaluator
  expressions      — link expression evaluation
  edn              — EDN parser
  bencode          — bencode codec for nREPL wire protocol
  nrepl            — nREPL server
  editorial_types  — shared types for the suggestion protocol
  fs_site_repository  — filesystem-backed SiteRepository
  mem_site_repository — in-memory SiteRepository for tests
  site_index       — site graph construction
  stylesheet       — CSS parser and asset tracker
  validation       — content validation logic

bases/
  publisher_cli    — CLI wiring for build, serve, lsp, mcp, nrepl modes
  editor_server    — multiplayer editing service base
  mcp_server       — MCP server base

projects/
  publisher        — presemble CLI binary
  content_management — long-running editing service
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
tools/smoketest.sh                 # end-to-end smoke test (requires curl and rep)
```

Requires the Nix devenv shell. Do not install packages with `cargo install -g` or `apt`; add them to `devenv.nix` instead.

## Version

Current release: **0.31.0**

See [ROADMAP.md](ROADMAP.md) for the milestone plan and [RELEASING.md](RELEASING.md) for the release workflow.
