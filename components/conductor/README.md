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

The command vocabulary lives in `protocol.rs`. Client paths are validated types ([ADR-066](../../docs/adr/066-client-paths-are-validated-site-paths.md)): a `SitePath` is relative and made of plain names; a `ContentPath` is a `SitePath` under `content/` ending in `.md`. A message with any other path fails to decode.

| Command | Description |
|---|---|
| `Ping` / `Shutdown` | Health check and graceful shutdown |
| `FileChanged { paths: Vec<SitePath> }` | File watcher detected changes; conductor rebuilds affected pages. Paths that resolve outside the site are ignored |
| `DocumentChanged { path: SitePath, text }` | Editor updated a buffer (does not write to disk) |
| `DocumentSaved { path: SitePath }` | Editor saved the file itself; the conductor drops its copy of the buffer and records a conflict if the document has unsaved edits |
| `ReloadDocument { path: ContentPath }` | Discard the unsaved edits of one document and load it from disk (`presemble reload`) |
| `GetDocumentText { path: SitePath }` | Returns editor working copy or disk fallback |
| `CursorMoved { path: SitePath, line }` | Editor cursor moved; the browser scrolls to follow |
| `Classify { path: SitePath }` | Classify a file path by its role in the site |
| `GetBuildErrors` | Returns the most recent build error map |
| `GetDirtyBuffers` | All documents with unsaved edits |
| `GetConflicts` | Documents whose file changed on disk while they have unsaved edits (ADR-065) |
| `SaveBuffer { path: SitePath, consent }` | Write one dirty buffer to disk; a conflicted buffer is written only if `consent` names it |
| `SaveAllBuffers { consent }` | Write all dirty buffers; if any conflict is not named in `consent`, nothing is written |
| `ApplyNedProgram { program }` | Evaluate a NED program against the node store (all selections resolved, then applied) |
| `BlockEdit(request)` | Browser edit of one block, or the fill of an empty slot, through its markdown (ADR-059 section 10). `request.file` is a `ContentPath` |
| `CreateNedSuggestion { file: ContentPath, selection, mutation, reason, author }` | Store a NED suggestion without applying it |
| `GetNedSuggestions { file: ContentPath }` | All NED suggestions for a file, in every status |
| `GetNedSuggestionFiles` | Files that have a pending NED suggestion |
| `AcceptNedSuggestion { id }` / `RejectNedSuggestion { id }` | Apply a suggestion and mark it accepted, or dismiss it |
| `CreateContent { stem, slug }` | Scaffold a new content file (stem must be a known schema; slug alphanumeric and dashes) |
| `ScaffoldSite { ... }` | Browser wizard: scaffold a new site from a starter template |
| `ListSchemas` | All schema stems with source text |
| `GetGrammar { stem }` | Cached grammar for a schema stem |
| `ListLinkOptions { stem }` | Link completion candidates for a schema stem (an unknown stem is an error) |
| `ResolveLinkTargetStem { source_stem, slot }` | The collection stem a link slot points at |
| `ListDependents { stem }` | Files that depend on a schema stem |
| `ListContent` | All content file paths (site-relative) |
| `RenderPage { path, mode }` | Render a page (view mode reads the conductor's editorial output directory, never the published output ([ADR-068](../../docs/adr/068-serve-publishes-what-is-saved-and-valid.md)); schema mode renders a `/_schema/...` URL) |
| `SchemaUrlForPage { page_url }` / `PageUrlForSchema { schema_url }` | Map a page URL to its schema URL, and back |

## Link resolution

Link expression evaluation uses the shared `expressions` component. The conductor does not contain its own copy of the link resolution logic.

## Used by

- `bases/publisher_cli` (serve, lsp, mcp, nrepl subcommands)
- `components/lsp_capabilities`
- `components/lsp_service`

---

[Back to root README](../../README.md)
