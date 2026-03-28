# ADR-006: Serve architecture

## Status

Proposed

## Context

M1 introduces `presemble serve` — a build-and-preview tool analogous to `hugo serve`. The goal is
to watch the site directory for changes, rebuild on each change, and serve the output over HTTP so
an author can preview their work in a browser without a separate deploy step.

Several structural questions arise:

**Where does serve live?** The repository already contains two bases: `publisher_cli` (the `build`
subcommand) and `editor_server` (reserved for the M2 multiplayer editing service). Serve could
belong to either.

**Which HTTP library?** The publisher is currently synchronous — it runs a build pipeline and
exits. No async runtime is present. Introducing one just to serve static files would be a
significant dependency footprint increase.

**Which file watcher?** The Rust ecosystem offers several crates at different abstraction levels.

**How should CLI subcommands be structured?** The publisher currently has a single implicit
command. M1 makes `build` and `serve` explicit. Future milestones will add flags like `--at` and
`--port`. A hand-rolled argument parser becomes fragile quickly.

## Decision

### `presemble serve` lives in `publisher_cli`

`presemble serve` is implemented as a subcommand of the existing `publisher_cli` base, alongside
`presemble build`. It is not placed in `editor_server`.

`editor_server` is reserved for the M2 multiplayer editing service — a long-running collaborative
backend with websocket connections and operational transform. Conflating static-file preview with
that service would entangle two distinct concerns before either is mature. `publisher_cli` is
already the entry point for site-building operations; serve is a natural extension of build.

### Static file serving uses `tiny_http`

The HTTP server for `presemble serve` uses `tiny_http` — a synchronous, minimal HTTP library with
no async runtime dependency. Serving static files from a local build output directory requires no
concurrency primitives beyond a simple request loop. `tiny_http`'s single-threaded model is
sufficient for a local preview tool.

### File watching uses `notify` v7 with ~100ms debounce

The `notify` crate (v7) provides cross-platform filesystem event notifications. A debounce window
of approximately 100ms collapses rapid sequences of write events (common with editors that write
temp files before renaming) into a single rebuild trigger. This avoids redundant rebuilds during a
save operation without introducing noticeable latency.

### CLI subcommands use `clap` with the derive feature

The `publisher_cli` base adopts `clap` (derive feature) for argument parsing. Subcommands are
declared as an enum with `#[derive(Subcommand)]`. This gives clean `--help` output, typed
arguments, and room to grow as the CLI gains flags in later milestones.

## Alternatives considered

**Serve in `editor_server`** — placing `presemble serve` in the `editor_server` base would
conflate static-file preview with the future multiplayer editing service. `editor_server` is
intended for M2; coupling it to M1 static serving creates premature dependency between the two
milestones and makes the eventual M2 implementation harder to reason about cleanly.

**`axum` + tokio instead of `tiny_http`** — `axum` with a tokio runtime is a capable and ergonomic
HTTP stack. However, an async runtime is not yet needed in the publisher; the build pipeline is
synchronous, and local static file serving does not require async I/O. Introducing tokio for M1
would be justified if the same binary were also running the M2 editing service, but that belongs in
`editor_server`. The right time to adopt an async runtime in `publisher_cli` is if and when serve
is migrated as part of M2 integration, which should be recorded in a new ADR.

**Hand-rolled argument parsing** — two subcommands with no flags can be handled with a `match` on
`std::env::args()`. This works for M1 but breaks down at M3, where `--at <timestamp>` and
`--port <n>` flags are planned. Starting with `clap` in M1 avoids a rewrite and gives `--help`
output for free.

## Consequences

**Positive:**

- The publisher binary stays synchronous and lightweight; no async runtime overhead for a local
  preview tool
- `editor_server` remains focused on its M2 role; no M1 concerns leak into it
- `clap` pays compound interest as the CLI grows — `--port`, `--at`, `--watch` flags in future
  milestones require no structural changes to argument parsing
- `notify` v7 is the current stable API; the debounce window is a single constant, easy to tune

**Negative / open questions:**

- If M2 editing features are eventually integrated into the same running process as serve (rather
  than as a separate binary), `presemble serve` in `publisher_cli` will need to migrate. A new ADR
  should record that transition and its rationale at that time.
- `tiny_http` is minimal by design; it does not support HTTP/2, TLS, or websockets. This is
  acceptable for a local preview tool. If serve ever needs any of those capabilities, the HTTP
  library choice must be revisited.
- The `notify` debounce value of ~100ms is a starting point chosen empirically; it may need
  adjustment based on editor save patterns observed in practice.

## M3 Update (2026-03-29)

The M1 decisions above have been revised for M3 to support live reload.

### Migration from `tiny_http` to `axum` + `tokio`

`tiny_http` does not support WebSocket connections, which are required for live reload: the browser
must receive a push notification when the site is rebuilt so it can refresh without polling.

`axum` is the standard async web framework in the Tokio ecosystem with first-class WebSocket
support via `axum::extract::WebSocketUpgrade`. The public `serve_site` function remains
synchronous at its boundary (it wraps a `tokio::runtime::Runtime`) so callers do not need to
change.

The router exposes:
- `GET /_presemble/ws` — WebSocket endpoint for live reload signals
- `GET /*` (fallback) — static file handler with HTML injection of the reload script

A `tokio::sync::broadcast` channel carries `()` reload signals. The file watcher thread (the
unchanged synchronous `notify` loop) sends a signal after each successful rebuild. Each connected
browser tab subscribes to the channel via `ws_handler` and reloads on receipt. If the WebSocket
connection drops (e.g. server restart), the browser retries after 1 second.

`axum` is the right foundation for M3–M4 features (editor WebSocket, SSE, collaborative editing API).

### Output directory moved to sibling `output/<site-name>/`

Previously output was written to `<site-dir>/output/`. The file watcher watched the source
subdirectories inside `<site-dir>`. With watch events using broad path matching, the watcher
could react to changes in the output directory when output files matched the watched extensions,
creating a potential feedback loop.

The new output location is `<parent-of-site-dir>/output/<site-dir-name>/`:

```
site/           <- source (watched)
output/
  site/         <- output (not watched)
```

The public helper `publisher_cli::output_dir(site_dir: &Path) -> PathBuf` computes this path
consistently across `lib.rs` and `serve.rs`. The root `.gitignore` was updated to add `/output/`.
