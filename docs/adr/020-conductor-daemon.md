# ADR-020: Conductor daemon with nng IPC

## Status

Proposed

## Context

The LSP server (`presemble lsp`) and the development server (`presemble serve`) run as
separate processes. Each independently parses schemas, loads content, and tracks state.
Changes made in one process do not reach the other without a filesystem round-trip: the
editor must save, the file watcher must detect the change, and the server must rebuild.

This latency prevents the key editorial experience: typing in the editor and seeing the
browser update in real time, before saving.

The LSP process is spawned by the editor (Helix, VSCode) over stdio. It cannot share
memory with the serve process. A shared-state solution requires inter-process communication.

## Decision

Introduce a conductor daemon as a standalone process that owns all mutable site state.
The LSP and serve processes connect as thin clients via nng (nanomsg-next-gen) IPC over
Unix domain sockets.

### Conductor owns

- Dependency graph (single source of truth for incremental rebuilds)
- Schema cache (parsed grammars, loaded once)
- In-memory document sources (editor working copies from LSP `did_change`)
- File watcher (filesystem change detection)
- Build and rebuild pipeline

### nng topology

- **REP socket** accepts commands: `DocumentChanged`, `EditSlot`, `GetGrammar`, etc.
- **PUB socket** broadcasts events: `PagesRebuilt`, `BuildFailed`

Separate socket URLs for REQ/REP and PUB/SUB, both under
`$XDG_RUNTIME_DIR/presemble/<site-dir-hash>`.

### Client architecture

- **LSP client** (`presemble lsp`): translates LSP JSON-RPC to conductor commands.
  `did_change` sends `DocumentChanged`; grammar loading sends `GetGrammar`.
  Validation and diagnostics remain LSP-local.
- **Serve client** (`presemble serve`): subscribes to `PagesRebuilt` events and
  forwards them to the browser via WebSocket. HTTP edit requests forward to conductor
  via `EditSlot`. Static file serving reads from the output directory.

### Daemon lifecycle (Kakoune-inspired)

- First client to connect starts the conductor: checks for socket, spawns daemon if absent.
- Socket at `$XDG_RUNTIME_DIR/presemble/<site-dir-hash>`.
- Conductor shuts down after idle timeout when no clients remain.
- Stale socket detection: failed connect deletes socket, restarts daemon.
- Escape hatch: `presemble conductor stop`.

### Polylith placement

- `conductor` component: state management, command handling, protocol types, client wrapper.
- `editor_server` base: hosts the daemon process, nng socket binding.
- `publisher_cli` base: hosts thin clients (LSP adapter, serve adapter), `conductor` subcommand.

## Alternatives considered

**Embedded conductor (library inside serve)** — avoids daemon lifecycle complexity but
cannot share state with the LSP process, which is spawned separately by the editor.
Requiring `presemble serve` to run before the editor defeats the purpose.

**Unix domain sockets with custom JSON protocol** — no new dependency, but requires
manual implementation of PUB/SUB, REQ/REP, backpressure, and framing. Approximately
500-1000 lines of boilerplate that nng provides out of the box.

**gRPC over Unix sockets** — type-safe RPC but overkill for local IPC between Rust
binaries. HTTP/2 framing overhead, `.proto` build step, heavy dependency chain.

**Keep separate processes, faster file watcher** — does not solve the fundamental
problem: editor changes cannot reach the browser without a file save.

## Consequences

**Positive:**

- Editor changes reach the browser in real time, before saving.
- Schema loading and content parsing happen once (in the conductor), not independently
  in each process.
- The architecture naturally extends to future clients: REPL, remote editors, CI
  integrations.
- `presemble build` remains standalone — it does not need the conductor.

**Negative / open questions:**

- New system dependency: nng (C library) requires cmake at build time.
- Daemon lifecycle adds operational complexity: stale sockets, crash recovery, idle
  timeout tuning.
- The standalone `presemble lsp` must handle both modes: with-conductor (connected) and
  without-conductor (local state, current behavior). This dual mode is a code smell but
  necessary for lightweight editor integration without a full site build.
- nng sockets are blocking by default. The daemon command loop must be designed to avoid
  blocking the PUB socket while processing a slow rebuild. A dedicated rebuild thread
  or async bridge may be needed.
