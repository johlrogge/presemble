# ADR-015: LSP server

## Status

Proposed

## Context

Presemble serves content authors via `presemble serve` (local dev). Authors working in editors such
as Helix, VSCode, or Neovim should receive diagnostics, completions, and hover hints for content
files without leaving their editor. A protocol-standard integration is preferable to per-editor
plugins, which carry high maintenance cost and fragment the feature surface.

M3 Phase 3 introduces an LSP server so any LSP-capable editor can:

- Validate content files against their schema at edit time (diagnostics)
- Receive schema-driven completions for slot names, required fields, and link targets
- See hover hint text for slot declarations
- Apply quick-fix code actions for common validation errors

The same protocol must also serve browser clients. M3 Phase 4 plans a structural in-browser
editor. Reusing the LSP session for browser communication avoids a second bespoke protocol and
keeps browser and IDE diagnostics consistent.

Two transport questions arise:

**How does a local editor connect?** Editors connect to LSP servers over stdio by convention. The
editor spawns the server as a child process and speaks JSON-RPC over stdin/stdout.

**How does the browser connect?** A browser cannot open a stdio process. The serve endpoint must
expose the LSP protocol over WebSocket so a browser tab can connect to the already-running
`presemble serve` process.

**Where does LSP logic live in the polylith layout?** LSP domain logic must not live in a base
(`publisher_cli` or `editor_server`); bases are entry points, not reusable libraries. Both bases
need access to the same validation and completion logic.

## Decision

### Two transports, one backend

A single LSP implementation serves both consumers via different transports:

- **stdio transport:** `presemble lsp <site-dir>` — the editor spawns this subcommand and speaks
  LSP over stdin/stdout. This is the standard mechanism for editors.
- **WebSocket transport:** `/_presemble/lsp` in `presemble serve` — the browser (or a browser-side
  LSP adapter) opens a WebSocket connection to the running serve process.

Both transports share the same LSP handler implementation. The transport layer is the only
difference.

### `tower-lsp` for the Rust LSP implementation

The `tower-lsp` crate (version 0.20, compatible with axum 0.8 + tokio 1) provides the
JSON-RPC/LSP layer. It handles the LSP lifecycle (initialize / initialized / shutdown), message
routing, and capability negotiation. Domain logic is implemented by supplying a struct that
implements the `LanguageServer` trait.

### LSP logic lives in polylith components

All LSP domain logic is placed in dedicated polylith components — `lsp_capabilities` and
`lsp_service` — not inside `publisher_cli` or `editor_server` bases. This allows both bases to
depend on the components without duplication. `publisher_cli` wires the stdio transport;
`editor_server` wires the WebSocket transport.

### Column positions use UTF-16 code units

The LSP specification requires that column positions in `Position` values use UTF-16 code unit
offsets, not byte offsets or Unicode scalar values. All position calculations in `lsp_service`
follow this convention.

### Declared capabilities

The server advertises the following capabilities during initialization:

- `textDocumentSync: FULL` — the client sends the full document text on each change; no
  incremental sync.
- `completionProvider` with trigger characters `#`, `[`, and `!` — matching the Presemble content
  syntax for slot references, links, and directives.
- `hoverProvider` — hover on a slot name returns its schema declaration.
- `codeActionProvider` — quick fixes for schema validation errors.

### Schema-to-content path convention

Content files under `content/{stem}/` are validated against the schema at `schemas/{stem}.md`.
For example, `content/posts/hello.md` is validated against `schemas/posts.md`. The `lsp_service`
component derives the schema path from the content file path using this convention.

## Alternatives considered

**Custom JSON-RPC protocol** — building a bespoke JSON-RPC server would require the same
implementation effort as `tower-lsp` but provide no editor ecosystem integration. Every editor
would require a custom plugin rather than reusing their generic LSP client. Rejected.

**REST API for browser only** — a REST endpoint could serve diagnostics and completions to the
browser without implementing LSP. However, this creates a second protocol: editors would still
need LSP, so two separate implementations would be required and maintained. Rejected.

**Per-editor plugins** — writing a Helix plugin, a VSCode extension, and a Neovim plugin
independently means reimplementing validation and completion logic in multiple languages with
separate release cadences. A single LSP server is maintained once and works everywhere.
Rejected.

## Consequences

**Positive:**

- Free integration with Helix, VSCode, Neovim, and any other LSP-capable editor — no per-editor
  plugins required.
- Browser and IDE share the same diagnostic engine; an error visible in the editor is the same
  error the browser reports.
- Phase 4 structural in-browser editor can build on the same LSP WebSocket session rather than
  inventing a new protocol.
- `lsp_capabilities` and `lsp_service` are polylith components: independently testable, reusable
  across bases, and isolated from transport concerns.

**Negative / open questions:**

- `tower-lsp` 0.20 must remain compatible with the axum 0.8 + tokio 1 versions used by the serve
  stack. This compatibility has been confirmed for the initial implementation; it must be
  re-verified on dependency upgrades.
- Diagnostic positions are slot-level initially — the whole slot is highlighted, not the specific
  character that triggered the error. Character-level precision within a slot is future work.
- WebSocket LSP is non-standard for editors. Editors connect exclusively via stdio. The WebSocket
  transport is for browser clients only and is not expected to work with editor LSP clients
  directly.
