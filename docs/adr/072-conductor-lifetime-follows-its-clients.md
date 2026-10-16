# ADR-072: The conductor's lifetime follows its clients

## Status

Proposed (decided; implemented once the idle exit lands; accepted once the manual checks pass)

## Context

- One conductor serves a site, and every client joins it if it is running: serve, the language server, the MCP server and the nREPL (`ensure_conductor`). The first client to need it starts it, as a separate process.
- Nothing ends it. `just serve-stop` stops serve; the conductor serve started keeps running. In manual testing, a conductor survived serve, then held on to its site after the user expected everything to be stopped.
- The client that started it cannot know who joined later, so it cannot stop it without pulling it out from under the others. Helix's language server would lose its conductor.
- Clients connect and disconnect freely. MCP connects per call, and clients can crash, so a count kept by the clients would drift.
- A conductor told to stop flushes unsaved documents first (ADR-070). Its shutdown is bounded: if the writer does not answer within 5 s, it exits saying what the writer was busy with.

## Decision

1. **An explicit stop.** `presemble stop <site-dir>` asks the site's conductor to shut down: it sends `Shutdown` (the same flush and warnings as a signal), with no protocol check first, so it also stops a conductor of another build. It waits a bounded time for the answer. It reports one of: stopped (with any warnings about unsaved edits), no conductor running, or no answer (then SIGTERM stops it, within 5 s). `just stop` stops serve and then the conductor.
2. **serve stops only itself.** `just serve-stop` stops serve, never the conductor (D-b). A conductor shared with an editor survives serve restarts.
3. **The conductor exits when no client has been connected for a while** (D-a).
   - **Idle** means no connection on the command socket, the event socket or the nREPL, for the whole idle period.
   - **The period** is `conductor: { idle-ms }` in `.presemble/config.json`: 120000 (2 minutes) by default; `0` turns the idle exit off.
   - **Before exiting**, it flushes, as for a signal.
   - **If anything is still unsaved after the flush** (auto-save off, a conflict, a held or drifted region, a buffer not yet canonical), it stays up and warns, once per idle spell. It never exits on its own while that would lose an edit.
   - A client that needs it later starts a new one.

## Choices made by the user

- **D-a: the idle period is 2 minutes** (`conductor: { idle-ms: 120000 }` by default; `0` turns the idle exit off). Long enough that moving between Helix, the browser and MCP never restarts it; short enough that a forgotten conductor goes away.
- **D-b: `just serve-stop` stops serve only;** `just stop` stops everything.

## Alternatives considered

- **serve stops the conductor it started:** it can't know who joined later.
- **`serve-stop` kills conductors:** the same problem, by hand.
- **Clients count references:** crashed clients and per-call MCP connections make the count drift. The conductor counts what it can observe.

## Consequences

- No leftover conductors after the last client goes, once decision 3 lands; an explicit stop at any time.
- A client may occasionally start a conductor again after an idle exit, at the cost of a cold start.
- The advice for a conductor of another build becomes `presemble stop <site>` instead of `pkill`.

## Related

ADR-031, ADR-035, ADR-070. ADR-020 is superseded and gave no lifetime rule.
