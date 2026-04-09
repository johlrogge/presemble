# ADR-031: Conductor as sole authority — all clients are conductor clients
## Status
Accepted
## Decision

The conductor is the single source of truth for all site state. Every client — MCP server, nREPL, `presemble serve`, LSP, future browser editor — is a conductor client that does all its work through conductor commands. No client reads the filesystem directly.

**Architecture:**
- One local conductor daemon per site (first client to connect starts it, last to disconnect stops it)
- All clients connect to the same local conductor via nng IPC
- Clients are protocol adapters — they translate their wire protocol into conductor commands
- The conductor may itself connect to a remote conductor in the future (multiplayer editing) — but clients always talk to the local daemon

**Clients are protocol adapters:**
- MCP server: JSON-RPC (stdio) → conductor commands
- nREPL: bencode (TCP) → conductor commands
- Serve: HTTP/WebSocket → conductor commands + renders HTML from conductor responses
- LSP: LSP protocol (stdio) → conductor commands

**The conductor owns:**
- Content (read, write, dirty buffers)
- Schemas (parse, cache)
- Templates (resolve, render)
- The site graph (nodes, edges, references)
- Suggestions (create, accept, reject)
- Build pipeline (resolve, render, publish)
- File watching and incremental rebuild

**Clients never:**
- Read content files from disk (`fs::read_to_string`, `fs::read_dir`)
- Build absolute paths from a `site_dir`
- Maintain their own copy of resolution/evaluation logic

**Conductor daemon lifecycle:**
- First client to need the conductor starts it (via `ensure_conductor`)
- The conductor stays alive while any client is connected
- All clients for the same site connect to the same daemon (socket URL derived from canonical site_dir)

## Why

1. **Single edit server.** One process holds the canonical state. No race conditions between clients reading stale disk state while the conductor has uncommitted edits in memory.

2. **No duplication.** `list_content`, `get_content`, link resolution, and schema lookup are implemented once in the conductor, not reimplemented in each client.

3. **Remote editing (future).** The local conductor can proxy to a remote conductor. Clients don't change — they still talk to the local daemon. This is the foundation for multiplayer editing.

4. **Testability.** Testing a client means testing protocol translation, not business logic. Business logic tests live in the conductor.

## Current violations

- `mcp_server::handle_list_content` reads `site_dir.join("content")` directly via `fs::read_dir`
- `mcp_server::get_content` builds `site_dir.join(file)` to construct absolute paths
- `devenv.nix` hardcodes MCP to `site/` — should use the same site_dir as the running conductor
- `publisher_cli::serve` maintains its own build pipeline (`build_for_serve`, `rebuild_affected`) alongside the conductor
- `evaluator` has duplicated link resolution logic from `expressions`

## Migration path

1. **Immediate:** Add `ListContent` conductor command. MCP `list_content` becomes a conductor client call.
2. **Immediate:** MCP `get_content` sends content-relative paths to conductor, not absolute paths.
3. **Immediate:** Fix `devenv.nix` — MCP site_dir must match `presemble serve` site_dir.
4. **M4:** Serve delegates build/render to conductor. Serve becomes a pure HTTP frontend.
5. **M4:** Extract duplicated evaluation logic from conductor into shared component.
6. **Future:** Conductor-to-conductor proxy for remote/multiplayer editing.

## Alternatives considered
- **Each client manages its own state** — the current reality. Causes duplication, stale reads, and makes remote editing impossible.
- **Shared library instead of IPC** — clients link the conductor as a library. Simpler but prevents remote operation and multiplayer.

## Consequences
- All new MCP/REPL/serve features must go through conductor commands
- Adding a conductor command is the way to expose new capabilities to all clients simultaneously
- The conductor's command vocabulary becomes the project's API surface
- Clients need the same site_dir as the serve process to connect to the right conductor
