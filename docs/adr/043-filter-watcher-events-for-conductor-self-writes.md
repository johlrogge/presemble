# ADR-043: Filter watcher events for conductor self-writes

## Status

Proposed

## Context

Presemble's `serve` command uses a `notify`-based filesystem watcher to detect
changes in `schemas/`, `content/`, and `templates/`. The watcher debounces
events for 150 ms and then sends `Command::FileChanged` to the conductor, which
rebuilds the affected pages via `populate_node_store()` (a wholesale reload from
disk) followed by `build_all_pages()` (rewrite every output HTML).

This design was correct when the conductor and the user's editor were two
distinct write sources. The conductor now writes to those watched directories
itself — via scaffolding, save-buffer, save-all-buffers, create-content,
suggestion-acceptance, wizard scaffolding, and any future flow that mutates
source files.

When the conductor writes a file the watcher is observing, the watcher fires a
`Command::FileChanged` roughly 150 ms later. That command does the destructive
thing: clears in-memory NED state for the affected documents, reloads everything
from disk, and rewrites all output HTML. Any in-memory NED edit that had not yet
been saved gets clobbered.

The smoketest has been intermittently failing for this reason: a NED `apply`
followed quickly by a `GET` reads pre-edit content because a watcher-driven
`FileChanged` raced in between the write and the read.

## Decision

Filter filesystem events at the watcher's debounce boundary against a
"recent self-writes" tracker maintained by the conductor.

**Recording conductor writes.** When the conductor writes any file under a
watched source directory:

1. Complete the write (`fs::write`).
2. Stat the file to obtain the post-write `mtime`.
3. Record a `(path, mtime)` pair in a shared `SelfWriteTracker` with a
   retention window of 1 second.

Content hash is stored alongside `mtime` as a secondary discriminator for
filesystems with coarse timestamp resolution.

**Filtering watcher events.** When the watcher receives a `notify::Event` for a
path, before forwarding it as `Command::FileChanged`:

1. Stat the file to obtain its current `mtime`.
2. Consult the `SelfWriteTracker`: is there a recent record for
   `(path, current_mtime)`?
3. If yes — the event is a conductor-originated echo. Drop it silently.
4. If no — the file was modified externally (user's editor, git checkout,
   etc.). Forward it normally.

`mtime` is the discriminator (not just path) so that an external editor save
landing on the same path but with a newer `mtime` is never suppressed.

**Enforcement surface.** A single `conductor::write_source_file(path, content)`
helper wraps `fs::write` so every conductor write records into the tracker
automatically. Direct `fs::write` calls in the conductor crate for files under
the watched directories are forbidden; a Clippy lint is the recommended
enforcement mechanism once the helper is in place.

## Why

The actual flake mechanism is conductor self-trigger, not a scaffold/ingest
race. It was reproduced by tracing the file-modification timeline: conductor
writes, watcher fires 150 ms later, `FileChanged` clears NED state, subsequent
GET returns stale content.

`mtime`-based filtering preserves correctness for genuine external-editor
changes — they have a different `mtime` than the one the conductor recorded. The
shared tracker is a small, scoped contract: writers record, the watcher
consults. No new bidirectional coupling is introduced between the watcher and
the conductor's domain logic.

## Alternatives considered

1. **Pause the watcher during conductor writes (mutex).** The conductor does not
   know the debounce window's exact length, and a global pause also silently
   drops genuine external events that arrive during the pause. Rejected.

2. **Route conductor writes to a separate directory or in-memory store.**
   Over-engineered. Users must see edits on disk for git and external tools to
   work. Rejected.

3. **Coalesce conductor and watcher state into one component.** They have
   different responsibilities; fusing them at the wrong abstraction level
   creates inappropriate coupling. The tracker is the minimum interface needed.
   Rejected.

4. **Make `Command::FileChanged` idempotent and non-destructive.** This
   requires detecting "did this file actually change in a way that matters?"
   which collapses back to the same problem. Rejected.

## Consequences

**Positive:**
- The smoketest flake disappears. Observation is reliable across the rest of
  Phase D and beyond.
- Production users keep in-memory NED edits across rapid scaffold/save/edit
  sequences; the watcher no longer clobbers unsaved state.
- The conductor/watcher contract remains clear: writers record, the watcher
  consults.

**Trade-offs:**
- All site-source `fs::write` calls in the conductor crate must go through
  `write_source_file`. This is enforceable by code review; a Clippy lint is
  recommended once the helper is stable.
- The `SelfWriteTracker` retention window (1 second) is a tunable constant.
  Too long: small memory overhead and a risk of suppressing a legitimate
  subsequent external edit to the same file within the window. Too short: risk
  of missing slow `notify` deliveries. One second is a generous margin given
  the 150 ms debounce; adjust if production evidence warrants it.
- `mtime` resolution on some filesystems (FAT32: 2 seconds) is coarser than
  the retention window. On such filesystems the discriminator may incorrectly
  suppress external edits that happen to share the same coarse timestamp as a
  conductor write. Linux ext4 provides nanosecond resolution and is the primary
  target. The limitation should be noted in code comments.

## Related

- [ADR-006](006-serve-architecture.md) — serve architecture, defines the watcher
  and conductor responsibilities. This ADR refines that contract.
- [ADR-031](031-conductor-as-sole-authority.md) — conductor as sole authority
  over mutations. This ADR ensures conductor mutations are not clobbered by
  spurious watcher echoes.
