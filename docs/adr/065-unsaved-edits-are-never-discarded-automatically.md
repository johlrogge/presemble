# ADR-065: Unsaved edits are never discarded automatically; a disk change to a dirty document is a conflict

## Status

Proposed

## Context

- Presemble keeps unsaved edits (from the browser, the REPL or MCP) in the
  conductor's NodeStore. A document with such edits is *dirty*.
- The watcher reports files that change on disk as `Command::FileChanged`
  (ADR-043 filters the conductor's own writes). Before this ADR a
  `FileChanged` for a dirty document threw the unsaved edits away and reloaded
  from disk: the author lost work without being told.
- Since the repopulate fix, rebuilding the store from disk keeps every dirty
  document except those whose own file changed. That exception was the one
  case left over. Manual recovery is `presemble reload`; ADR-057's recovery
  section is being rewritten in stage 2e-2.
- The LSP side is similar: an editor save (`DocumentSaved`) over a dirty
  document silently replaced the unsaved edits' base.

## Decision

1. **A disk change to a dirty document is a conflict, not a reload.**
   `FileChanged` on a dirty path keeps the in-memory edits and marks the
   document `ConflictsWithDisk`. Only changed paths that are not dirty are
   taken from disk. `DocumentSaved` over a dirty document is a conflict in the
   same way. A conflicted document is dirty by construction, so everything that
   handles dirty documents (save-all, dirty-buffer lists) keeps working.
2. **The conflict is announced once**, when first recorded:
   `ConductorEvent::DiskConflict { file }`. It reaches the editorial socket as
   `{"type":"conflict","file":...}` and never the public socket. The current
   set is available from `GET /_presemble/conflicts` (`Command::GetConflicts`).
3. **Two resolutions, both explicit:**
   - **Save with consent.** `SaveAllBuffers { consent }` and
     `SaveBuffer { path, consent }` carry a typed
     `ConflictConsent::{None, Overwrite(BTreeSet<String>)}`. If any conflicted
     target is not named in the consent, nothing is written and the answer is
     `Response::ConflictsNeedConfirmation(files)`. The browser warns first
     (listing the files), then asks Overwrite or Cancel; Overwrite posts again
     with exactly the listed files. Consent names paths: a conflict that
     appears after the warning is not covered by consent already given.
   - **Reload.** `presemble reload <site-dir> <file>` discards the edits and
     takes the disk version. Reload is CLI-only; the browser notice has no
     buttons and names the command.
4. **The LSP is a carve-out for now.** The language server does not subscribe
   to conductor events, so it asks when it validates a content file and shows a
   warning at line 0 for a conflicted document. It offers no resolution of its
   own; the message points to the browser or the CLI. Buffers held only by the
   editor (the LSP mirror, `doc_sources`) are not tracked as conflicts: the
   editor owns them and resolves its own conflicts. The auto-save ADR, which
   closes channel gap 8, brings them into this model.
5. **Auto-save may never save a conflicted document.** Any future automatic
   save must treat a conflict as a refusal to write; only consent given by a
   person, naming the path, overwrites the disk version.

## Amends

- ADR-043: the `FileChanged` description ("clears in-memory NED state for the
  affected documents and reloads everything from disk") no longer holds for
  dirty documents; see the note added there.

## Related

- [ADR-043](043-filter-watcher-events-for-conductor-self-writes.md): self-write filtering.
- [ADR-057](057-transactional-ned-well-formedness.md): transactional NED; its recovery section is being rewritten in stage 2e-2.

## Consequences

- No edit is lost to an outside change; the author sees the conflict wherever
  they work (browser notice, LSP warning) and chooses.
- A person who edits the same file in an editor and in the browser must resolve
  the conflict before saving from the browser.
- Conflict state lives in memory in the conductor and is lost with it.
