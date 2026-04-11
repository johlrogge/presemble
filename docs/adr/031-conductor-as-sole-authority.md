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
- Require environment variables or hardcoded paths to find the conductor

**Per-call site targeting:**
- Clients that serve multiple sites (e.g. the MCP server) accept the site directory as a parameter on each request, not as a startup argument
- The conductor socket URL is derived from the canonical site directory path — clients connect to the right conductor by passing the right site dir
- No environment variables, no configuration files, no discovery protocols — the site dir is the only input needed to connect

**Conductor daemon lifecycle:**
- First client to need the conductor starts it (via `ensure_conductor`)
- The conductor stays alive while any client is connected
- All clients for the same site connect to the same daemon (socket URL derived from canonical site_dir)

## Why

1. **Single edit server.** One process holds the canonical state. No race conditions between clients reading stale disk state while the conductor has uncommitted edits in memory.

2. **No duplication.** `list_content`, `get_content`, link resolution, and schema lookup are implemented once in the conductor, not reimplemented in each client.

3. **No configuration.** Clients don't need environment variables or config files to find the conductor. The site directory is the only input. This eliminates a class of misconfiguration errors.

4. **Remote editing (future).** The local conductor can proxy to a remote conductor. Clients don't change — they still talk to the local daemon. This is the foundation for multiplayer editing.

5. **Testability.** Testing a client means testing protocol translation, not business logic. Business logic tests live in the conductor.

## Remaining violations

None. All clients operate through conductor commands.

`publisher_cli::serve` calls `build_for_serve` once at startup to bootstrap the output directory (HTML rendering, asset discovery, stylesheet/asset copying). This is not a violation — it is a one-time bootstrap, not a parallel pipeline. All subsequent rebuilds go through the conductor's `FileChanged` command.

## Migration path

1. ~~Add `ListContent` conductor command~~ — done
2. ~~MCP per-call `site` parameter~~ — done
3. ~~Remove hardcoded `site/` from devenv.nix~~ — done
4. ~~Wire MCP `list_content` to conductor `ListContent` command~~ — done (was already wired)
5. ~~Serve delegates rebuild to conductor~~ — done. Serve is a thin HTTP frontend; `watch_and_rebuild` sends `FileChanged` to conductor; all handlers use conductor commands.
6. ~~Extract duplicated evaluation logic from conductor into shared component~~ — done. Conductor calls `expressions::*`; `eval_repl` moved to `evaluator`.
7. **Future:** Conductor-to-conductor proxy for remote/multiplayer editing.

## Alternatives considered
- **Environment variable for site dir** — rejected. Adds a source of error with no benefit. The site dir is already a natural parameter of each operation.
- **Auto-discovery of running conductors** — rejected. Too much magic. The site dir deterministically maps to a socket URL.
- **Each client manages its own state** — the pre-ADR reality. Causes duplication, stale reads, and makes remote editing impossible.
- **Shared library instead of IPC** — clients link the conductor as a library. Simpler but prevents remote operation and multiplayer.

## Consequences
- All new MCP/REPL/serve features must go through conductor commands
- Adding a conductor command is the way to expose new capabilities to all clients simultaneously
- The conductor's command vocabulary becomes the project's API surface
