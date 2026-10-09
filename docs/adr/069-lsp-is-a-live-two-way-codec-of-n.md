# ADR-069: The LSP is a live two-way codec of N

## Status

Proposed

## Context

- N (the node store) is the model; the browser, MCP, the REPL, files and the
  editor are representations of it (ADR-032, ADR-059 §0).
- The LSP is not one yet:
  - N → editor: nothing. `lsp_service` sends commands and reads replies; it
    never subscribes to conductor events, and the workspace has no
    `workspace/applyEdit`. A browser, MCP or REPL edit never reaches an open
    buffer, which goes stale.
  - Editor → N: every `didChange` sends the whole buffer
    (`Command::DocumentChanged`). The conductor mirrors it in `doc_sources`
    and `update_document_in_store` re-parses it and **replaces the whole
    document** in N with a new root. This is the clone-and-swap the
    select-then-apply rule rejects (ADR-057): there is no base check, so a
    stale buffer's next keystroke overwrites edits made elsewhere.
  - There are two notions of "dirty": NED edits (`dirty_docs`, saved as N's
    canonical serialization) and editor text (`doc_sources`, saved as typed by
    `SaveBuffer`, bypassing N).
- ADR-065 §4 left the LSP as a carve-out until this ADR.
- `ConductorSubscriber` (nng SUB) already exists; serve uses it.

## Decision

**Scope:** content documents. Templates and schemas keep today's text path
until they are held in N.

1. **N pushes every change to every open buffer.**
   - After any commit to a document's N, from any origin other than this
     buffer, the conductor emits `ConductorEvent::DocumentModelChanged { file }`.
     Origins are: a NED plan, an accepted suggestion, a reload, a disk change
     taken from disk, or another editor.
   - The LSP subscribes to these events. For an open buffer it fetches the
     changed blocks and rewrites only their ranges in the buffer, with a
     versioned `workspace/applyEdit`. The new text of each changed block is
     its canonical markdown; unchanged blocks keep their text exactly as typed.
   - If the buffer's version has moved on, the edit is rejected; the next
     `didChange` goes through decision 2, whose base check sees the
     divergence.
2. **Buffer edits are lowered into N block by block.**
   - For each open buffer the LSP keeps `synced`: the buffer version and text
     last known to match N.
   - On `didChange` it compares the blocks of `synced` and of the new text,
     and turns each changed region into a step of one NED plan:
     - a changed block becomes `block_edit::lower` with that block's `synced`
       text as the base;
     - an added block becomes an insert;
     - a removed block becomes a delete.
   - The plan is atomic (select, then apply). Edits to different blocks from
     different places merge.
   - A step whose block changed in N since `synced` is refused (Drifted). The
     block gets an LSP diagnostic with two code actions: "take the model's
     version" (push it) and "keep mine" (overwrite, the ADR-065 consent rule
     in editor form). Nothing is applied silently.
   - Text that does not read as content keeps the buffer and gets a
     diagnostic; nothing is lowered.
   - The `didChange` that reports a pushed edit is an echo and is ignored.
3. **One notion of dirty.** `doc_sources` and the editor-text save path in
   `SaveBuffer` are removed. A document is dirty when N differs from its file;
   saving always writes N's canonical serialization.
4. **Formatting is canonicalisation.** `textDocument/formatting` returns the
   edits that make the buffer N's canonical markdown. With the editor's
   format-on-save (Helix `auto-format`), `:w` writes exactly what the
   conductor would. The conductor treats an editor save whose text reads as
   the current N as a save of N, not as a conflict (amends ADR-065 §1).
5. **ADR-065 §4 (the LSP carve-out) is withdrawn.** The LSP shows the
   ADR-065 conflict state for a conflicted document and offers its two
   resolutions as code actions.

## Open question (resolved by a spike in package LSP-2)

- How does Helix behave when the conductor writes a file under an open,
  unmodified buffer (auto-save, ADR-070)? Does it ignore it, warn, or refuse a
  later `:w`?
- If it warns or refuses, the fallback is to push the canonical formatting
  into the buffer right after each save. It only changes style, never
  content.

## Consequences

- Every representation of N, the browser and the editor alike, shows every
  change as it happens; no buffer is stale.
- A buffer's edits get the same exact-addressing and base checks as browser
  edits; two people editing different blocks never overwrite each other.
- An editor buffer shows canonical text for blocks changed elsewhere and the
  user's own style for blocks they typed, until formatting.

## Related

ADR-032 (NED), ADR-056 (exact addressing), ADR-057 (transactions), ADR-059
§10 (block edits), ADR-065 (conflicts), ADR-070 (auto-save).

## Done in LSP-2 (2.1 to 2.5)

Decisions 2 and 3 and the `:w` flush are built (the conductor commands
`EditorOpened`, `EditorChanged`, `EditorClosed`, `KeepMine`; `DocumentChanged`
and `doc_sources` are gone). Decision 1 (N pushes to the buffer) and the
canonical push before a save are not part of LSP-2.

- **Every keystroke is lowered live.** No debounce: each `didChange` is
  compared with the text N and the editor last agreed on (the base) and lowered
  at once.
- **Per-container regions.** A document is compared slot by slot, `unassigned`
  and body, by content, not by spelling. A region that is one blockquote on both
  sides, or one list of the same kind and spacing with the same number of
  items, is compared inside (descent), so a keystroke in a list item touches
  only that block. Anything else that differs is a region of the container it is
  in.
- **The lowering table.**
  - One block for one block: `block_edit::lower` with a typed target (the
    block, the text of a code block, or an empty container).
  - A run of blocks for a run: a Replace of the first aligned block and a
    delete of the rest.
  - Inserted blocks: `InsertAfter`, `InsertBefore` or `InsertChild`.
  - Removed blocks: Delete, under the base check.
  - Comments only: `SetComments` with place `Before` or `End`.
- **The re-read fallback.** Some changes are lowered by reading the whole
  document again from the editor's text. It happens only while N reads as the
  parsed base; otherwise the region drifts. The cases, each with its reason:
  - Separators, HTML blocks and link expressions: no payload may carry them
    (ADR-062 D4, ADR-058). Follow-up: an editor-only payload.
  - Comments changed together with blocks, and a paragraph typed directly after
    a comment: comments in payloads are not built (§0.8), and no single plan can
    move a comment onto a block that does not exist yet. Follow-up.
  - Changes to the slots of section 0: slot assignment is a function of the
    whole section. Inherent.
- **Held regions.** A region that opens a code fence, or an HTML block of
  CommonMark types 1 to 5, that is not closed is not lowered; the base keeps its
  old text and the editor shows an information diagnostic. Whether it is open
  is decided on the region's text with what the containers it is in take from
  its lines removed, as the parser sees it. `:w` lowers a held region as it
  stands (it is the editor's final word).
- **Drift.** A region whose aligned blocks in N no longer read as the base is
  reported, not applied. Its id (`DriftId`) hashes the path down to the region,
  its base text and the ids of the aligned N blocks, so it is stable while the
  editor types elsewhere. `KeepMine { drift_id }` replaces just those blocks
  with the editor's version, and is refused once the drift has changed (the
  editor then sees the new drift).
- **`editor_wrote`.** `:w` is a flush. A `DocumentSaved` or a watcher
  `FileChanged`, whichever comes first, for a file that holds the editor's
  latest text: held regions are lowered; if N then reads as the file, the dirty
  mark is cleared and the file stays as written; otherwise N holds others'
  changes and its canonical text is written. The file's stamp is taken before
  and after the read and the read is repeated if they differ, so a write that
  lands in between is a fresh disk change and is never recorded as N's version.
- **The sender in the LSP.** All traffic to the conductor goes through one
  queue and one worker thread, so a handler returns at once. A new text of a
  file replaces the one before it while that still waits; a save or a close
  queued in between stops the merge, so the order the editor acted in is kept.
- **Comments in payloads are left out (§0.8).** Blocks inserted or replaced
  carry no comments; comments move only through `SetComments`.
- **Flag errors as locally as possible.** Drift and held regions are reported
  and resolved at the depth where they arise, in the list item or the quote, not
  in the top-level block around it. Known exceptions:
  - FU-1: a tight list holds the whole list while a fence is open in one item
    (a code block after a paragraph makes the parser read the list as loose, so
    the list changes as a whole).
  - The re-read cases above.
  - Conflicts are per file until F2.
- **Cost.** Release build, a site of 1000 pages and one document of 500 blocks,
  40 rounds with the cases interleaved:

  | Case | p50 | p95 |
  |---|---|---|
  | Re-reading the whole document (the old way) | 12.5 ms | 13.4 ms |
  | `EditorChanged`, an edit in place | 11.2 ms | 11.6 ms |
  | `EditorChanged`, a new paragraph | 11.2 ms | 11.9 ms |

  Both lowered cases are within the cost of the old way, which paid the same
  page rebuild. That rebuild is what the time is made of.

## Amendment (2026-10-09): the open question, and what the decisions need

The user's answer: Helix does not flag a file as changed on disk when its
contents match the buffer; only a write Presemble did not make (sed, git)
is flagged. So §1 (push) and §2 (lowering) both stand, with auto-save.

- **The buffer matches the file before every write.** Presemble writes
  canonical markdown; a buffer holds the user's own spelling. Before saving a
  document open in an editor, the conductor sends the editor the edits that
  make the buffer the canonical text (a minimal line diff, versioned
  `workspace/applyEdit`), and writes the file only once the editor reports
  that text. If the version has moved (the user is typing), that save waits
  for the next idle. The canonical text lands in the buffer exactly as a
  refactor does.
- **Match the file on disk (code action).** When a buffer and its file read
  as the same content but differ as text (a format run, a checkout, an editor
  that flagged the file anyway), the language server offers "Match the file on
  disk": it applies the minimal edits that make the buffer the file's text.
  It is not offered when the contents differ; that is a change on disk or a
  conflict (ADR-065).
- **Lowering carries comments.** `NedMutation::SetComments(Vec<String>)` sets
  a block's leading comments or a container's end comments; each must be
  comment-only. Without it a comment typed in the editor would be removed from
  the buffer at the next save.
- **An echo needs no suppression.** The `didChange` that reports a pushed edit
  is lowered like any other, and lowers to nothing, because every block
  already reads as N.
- **`:w` is a flush.** An editor's write of the text it last sent is not a
  change on disk: anything pending is lowered, and N's canonical text is
  written if the file differs (it is pushed to the buffer first, as above).
  Any other write is a change on disk (ADR-065).
- **Closing a buffer changes nothing in N.** Its edits are already in N and,
  after an idle, on disk.
- **Conflict code actions** are worded "Take the file on disk (discard
  unsaved edits)" and "Keep the unsaved edits (overwrite the file)", since
  every channel edits the same N.
- **If the check shows that Helix compares modification times, not contents:**
  auto-save leaves documents open in an editor to the editor's `:w`, which is
  then the only writer of those files; everything else stands.
