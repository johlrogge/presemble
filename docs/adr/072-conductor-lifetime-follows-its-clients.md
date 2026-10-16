# ADR-072: The conductor's lifetime follows its clients

## Status

Proposed. Decided: `presemble stop`, and serve not stopping the conductor. Open: D-a and D-b below; the idle exit (decision 3) is implemented once D-a is decided.

## Context

- One conductor serves a site, and every client joins it if it is running: serve, the language server, the MCP server and the nREPL (`ensure_conductor`). The first client to need it starts it, as a separate process.
- Nothing ends it. `just serve-stop` stops serve; the conductor serve started keeps running. In manual testing, a conductor survived serve, then held on to its site after the user expected everything to be stopped.
- The client that started it cannot know who joined later, so it cannot stop it without pulling it out from under the others. Helix's language server would lose its conductor.
- Clients connect and disconnect freely. MCP connects per call, and clients can crash, so a count kept by the clients would drift.
- A conductor told to stop flushes unsaved documents first (ADR-070). Its shutdown is bounded: if the writer does not answer within 5 s, it exits saying what the writer was busy with.

## Decision

1. **An explicit stop.** `presemble stop <site-dir>` asks the site's conductor to shut down: it sends `Shutdown` (the same flush and warnings as a signal), with no protocol check first, so it also stops a conductor of another build. It waits a bounded time for the answer. It reports one of: stopped (with any warnings about unsaved edits), no conductor running, or no answer (then SIGTERM stops it, within 5 s). `just stop` stops serve and then the conductor.
2. **serve stops only itself.** `just serve-stop` stops serve, never the conductor (pending D-b). A conductor shared with an editor survives serve restarts.
3. **The conductor exits when no client has been connected for a while** (pending D-a). It counts the connections on its command and event sockets. When there have been none for the idle period, it shuts down as for a signal. With unsaved edits and auto-save off, it stays up and warns instead, since exiting would lose them. The period is `conductor: { idle-ms }` in `.presemble/config.json`. A client that needs it later starts a new one.

## Open decisions

- **D-a: the idle period.** Recommendation: 2 minutes. Long enough that moving between Helix, the browser and MCP never restarts it; short enough that a forgotten conductor goes away.
- **D-b: should `just serve-stop` also stop the conductor?** Recommendation: no. `just stop` is for stopping everything.

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
