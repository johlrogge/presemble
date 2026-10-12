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

   A third way, the **save-time mtime check**, covers a change the watcher has
   not reported yet (it debounces for 150 ms). The conductor keeps, next to the
   dirty documents, a stamp (modification time and length) of each content file
   as it last read it from disk (populate, reload) or wrote it (a save). Before
   a save writes a document with unsaved edits, it compares the file's current
   stamp with that one. A difference, or a file that was there and is gone,
   records the conflict right away (and announces it as above); the consent rule
   of decision 3 then applies, so nothing is written without consent that names
   the file. A path with no recorded stamp is not checked. The length is part
   of the stamp so that a write in the same mtime tick as the recorded one is
   still seen when it changes the size; a same-size rewrite within one tick of
   a coarse-timestamp file system can still go unseen. The stamps are not part
   of the self-write tracker (ADR-043), which only remembers the conductor's own
   writes for a second.
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

## Amendment (LSP-2, ADR-069)

A disk change whose text reads as what N currently holds for the document
(compared as documents, not as bytes) is not a conflict: it clears the dirty
mark and updates the stamp of decision 1. When an editor is attached and the
file holds exactly the editor's latest text, the write is the editor's own: held
regions are lowered as they stand, and then, if N reads as the file, it is a
save; otherwise N holds others' changes and N's text is written over the file.
Every other disk change follows decisions 1 to 5. The LSP is no longer a
carve-out for buffers (decision 4): an editor's text is lowered into N as it is
typed, so it is tracked like any other unsaved edit.

## Amendment (F2, 2026-10): a disk change is merged block by block

A disk change to a document with unsaved edits is no longer one conflict
for the whole file. It is merged as a three-way comparison, per container
and block, descending into quotes and list items:

- **Base:** the file as N last agreed with it. It is recorded when the
  document goes from saved to unsaved, if the file is still the version N
  was read from or last written as. It is cleared when the document is
  saved, reloaded or reverted.
- **Theirs:** the file now. **Ours:** N.
- A block changed **only on disk** is merged into N, as an edit from the
  disk, with the same exact addressing as any edit. The pages that show it
  are rebuilt.
- A block changed **only in N** is kept.
- A block changed **on both sides to the same content** is not a conflict
  (default Q5).
- A block changed **on both sides differently** is a conflict on that
  block. Its two resolutions sit on that block: "Take the file's version
  of this block" and "Keep the unsaved version".
- **Comments** merge like blocks; a comment changed on both sides is a
  conflict on the comment's host (default Q6).
- **Per-document exceptions,** still one conflict for the whole file:
  - there is no base (the file had already changed under N when the
    document became unsaved);
  - the file is gone;
  - the change alters the document's structure (slots appear or vanish,
    or `----` moves) while N has unsaved edits, because which block belongs
    to which slot is then not a block-by-block question.
- The merge is the engine an open editor's changes go through (ADR-069):
  the disk is one more channel. It has the same drift rule, the same
  "already holds it" rule, and the same base update. The base advances to
  the new file, except where a conflict remains.
- An open editor's base is not changed by a merge: its next keystroke
  leaves the merged block alone. LSP-1 will push merged blocks into open
  buffers.
- **Consent to overwrite (§3) is unchanged:** a save of a document with an
  unresolved conflict needs consent naming the file, and writes N, the
  merged blocks included. A document whose last conflict is resolved is
  an ordinary unsaved document again.
- Announcing a conflict (§2) now names the blocks. The browser and the
  editor show each conflict on its block.

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
