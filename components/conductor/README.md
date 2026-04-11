# conductor

The conductor is the authoritative daemon for a running Presemble site. All clients — `presemble serve`, `presemble lsp`, the MCP server, and the nREPL server — are thin protocol adapters that delegate all business logic to the conductor via nng IPC.

See [ADR-031](../../docs/adr/031-conductor-as-sole-authority.md) for the full design rationale.

## What it owns

- **Site graph** — the built graph of all content, schemas, and templates
- **Schema cache** — parsed grammars keyed by stem
- **Dirty buffers** — in-memory editor working copies (not yet on disk)
- **Build errors** — most recent build error map keyed by file path
- **Suggestions** — pending editorial suggestions with accept/reject state
- **File watching and incremental rebuild** — `FileChanged` triggers a rebuild of affected pages and broadcasts results via PUB/SUB

## Protocol

Commands are sent over an nng REQ/REP socket. Events are broadcast over an nng PUB/SUB socket. Both use JSON serialization.

The command vocabulary lives in `protocol.rs`:

| Command | Description |
|---|---|
| `FileChanged { paths }` | File watcher detected changes; conductor rebuilds affected pages |
| `GetBuildErrors` | Returns the most recent build error map |
| `GetDocumentText { path }` | Returns editor working copy or disk fallback |
| `DocumentChanged { path, text }` | Editor updated a buffer (does not write to disk) |
| `DocumentSaved { path }` | Editor saved; conductor writes dirty buffer and rebuilds |
| `EditSlot { file, slot, value }` | Browser edit: modify a slot and write to disk |
| `EditBodyElement { file, body_idx, content }` | Browser edit: replace a body element |
| `CreateContent { stem, slug }` | Scaffold a new content file |
| `ScaffoldSite { ... }` | Browser wizard: scaffold a new site from a starter template |
| `Classify { path }` | Classify a file path by its role in the site |
| `ListSchemas` | All schema stems with source text |
| `ListLinkOptions { stem }` | Link completion candidates for a given schema stem |
| `ListContent` | All content file paths (site-relative) |
| `GetGrammar { stem }` | Cached grammar for a schema stem |
| `SuggestSlotValue { ... }` | Create a full-slot editorial suggestion |
| `SuggestSlotEdit { ... }` | Create a search/replace suggestion scoped to a slot |
| `SuggestBodyEdit { ... }` | Create a search/replace suggestion in the body |
| `GetSuggestions { file }` | All pending suggestions for a file |
| `AcceptSuggestion { id }` | Apply and mark a suggestion accepted |
| `RejectSuggestion { id }` | Dismiss a suggestion without applying |
| `SaveBuffer { path }` | Write a dirty buffer to disk |
| `SaveAllBuffers` | Write all dirty buffers to disk |
| `Ping` / `Shutdown` | Health check and graceful shutdown |

## Link resolution

Link expression evaluation uses the shared `expressions` component. The conductor does not contain its own copy of the link resolution logic.

## Used by

- `bases/publisher_cli` (serve, lsp, mcp, nrepl subcommands)
- `components/lsp_capabilities`
- `components/lsp_service`

---

[Back to root README](../../README.md)
