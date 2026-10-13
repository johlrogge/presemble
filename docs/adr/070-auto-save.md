# ADR-070: Auto-save: the conductor saves every edit after 2 s idle

## Status

Proposed (implemented; accepted once the manual checks pass)

## Context

- Saving is an explicit act today: `SaveBuffer` and `SaveAllBuffers`, the
  ADR-065 conflict check and the S1 stamp check, the self-write tracker
  (ADR-043), the serial writer, and serve publishing after each save
  (ADR-068).
- An edit is lost if the author forgets to save, and an editor showing
  "unsaved" for hours is the normal state.
- Preconditions:
  - no known save path loses content (ADR-059 stage 8 closed the text losses);
  - L10, adjacent lists merging, is fixed by ADR-059 stage 9 before this
    ships;
  - ADR-069 gives one notion of dirty across every representation.

## Decision

1. **Trigger.**
   - The conductor owns the timer. A document is saved once it has been dirty
     and untouched for `idle_ms` (default 2000).
   - Every dirty document is flushed at once when the conductor shuts down,
     and when a client sends a flush hint (`Command::FlushDirty`). The browser
     sends one when its page is hidden or closed (see the amendment: not when
     it loses focus).
   - Auto-save goes through the same save path as an explicit save; there is
     no second writer.
   - A document with a held editor region (an unterminated code fence or HTML
     block, ADR-069) is not auto-saved until the region is lowered: N does not
     yet hold what the author is typing there, and a save would write the file
     without it. An explicit `:w` lowers the region as it stands.
2. **Scope.** Every edit to N: browser, MCP, REPL and nREPL, accepted
   suggestions, and editor buffers (through ADR-069). An explicit save (`:w`,
   SaveBuffer) is a flush of that document. For a document open in an editor,
   the save first brings the buffer to the canonical text (ADR-069 amendment)
   and writes once the editor holds it; while the user is typing it waits for
   the next idle.
3. **Conflicts.**
   - A document in conflict with disk (ADR-065) is never auto-saved. Its edits
     stay in memory and it shows the Conflict state until a person resolves
     it.
   - Every other document keeps auto-saving. ADR-065 §5 is now this rule.
4. **Canonicalisation.**
   - A save writes N's canonical markdown, also the first save of a
     hand-written file. No notice is shown.
   - `presemble fmt <site-dir>` canonicalises every content file once, so the
     style-only changes can be committed on their own.
5. **Invalid content is saved.** Schema validity gates publishing (ADR-061),
   not saving.
6. **No git history.** Auto-save never commits. Locally history is the user's
   git; hosted history is the ADR-048 archive.
7. **Publishing.**
   - Locally, serve keeps publishing after saves (ADR-068), coalesced: at most
     one publish running and one waiting.
   - Hosted, auto-save writes to the workbench only, and publishing is an
     explicit or scheduled act (ADR-048, ADR-049).
   - Until the workbench exists, a hosted conductor runs with auto-save off.
8. **What editors see.**
   - Each document has a save state: Saved, Saving, Unsaved, Conflict, or
     Save failed. The conductor announces it as
     `ConductorEvent::SaveState { file, state }` on the editorial channel.
   - A failed save shows a banner that stays until a Retry succeeds.
   - Hovering shows the last save time.
   - The LSP shows Save failed and Conflict as diagnostics.
9. **Configuration.** `<site>/.presemble/config.json` holds
   `"autosave": { "enabled": true, "idle-ms": 2000 }`. It is on by default
   locally. Only the conductor daemon reads it; builds never do.
10. **Crash protection** is the idle window: at most about `idle_ms` of edits.
    There is no journal.

### Done: `presemble fmt` (decision 4)

- `presemble fmt [--check] <site-dir>` writes every content file with the
  canonical serializer a save uses: `content::format_document` reads the file
  with its grammar (the item or collection schema, as the conductor picks it)
  and writes it with `serialize_document`. Invalid content is formatted too.
- That this is exactly what a save writes is proven by
  `formatting_matches_what_a_save_writes`: for 3060 documents (the
  conservation corpus under both of its grammars, link-expression lookalikes,
  and every content file of the site and the example sites), formatting gives
  the same text as putting the document in the store and serializing it from
  there. `formatting_is_a_fixed_point` shows formatting formatted text changes
  nothing. Building this showed that some parsed documents could not be saved
  at all; that was fixed first (every parsed document can now be saved).
- Only files whose text changes are written, each through a temporary dot file
  and a rename, so a running serve sees one change and takes it like any
  outside write (ADR-065): merged block by block, without a conflict.
- `--check` writes nothing and lists the files that would change. Exit codes:
  0 nothing to do, 1 `--check` found changes, 2 a file could not be formatted
  (2 wins over 1).

**Future: templates and schemas.** `presemble fmt` is built to format other kinds of file: each kind needs only a canonical writer. Templates and schemas are intended to follow. Each needs its own decision first (a later ADR):
- **Templates:** the hiccup reader skips `;` comments, so writing a template back would drop them. Comments must be kept, as content's are (trivia), before templates can be formatted safely. Whether HTML templates keep their comments needs the same check, and the canonical form of each syntax (hiccup, HTML) must be fixed.
- **Schemas:** there is no schema writer at all yet, only a parser, and the parser is permissive (Wave 4). A canonical schema form needs both: a strict reading, and a writer that round-trips it.

## Amendment (auto-save implementation, 2026-10)

- **Drifted regions wait like held ones.** A document whose open editor has
  a held or drifted region (ADR-069) is not auto-saved: N does not hold what
  the editor shows there, so the buffer cannot be brought to the text a save
  would write. `:w` still saves at once.
- **An open editor's document waits for canonical text.** It is auto-saved
  only once the editor holds N's canonical text (ADR-069 amendment). Until
  then the conductor asks the editor for it (`EditorCanonicalWanted`), and
  asks again every five idle periods; it never saves on a timer of its own.
  `:w` and a flush save at once.
- **A flush saves at once.** Leaving the page (the browser's hint),
  `FlushDirty` and shutdown save every document that can be saved, without
  waiting for an editor's canonical text; an editor that had not taken it
  shows the file as changed on disk, which "Match the file on disk" settles.
- **Shutdown warns about conflicts.** A document still in conflict with its
  file when the conductor stops is named in a warning: its unsaved edits are
  lost with the conductor (ADR-065). Keeping them across a restart is later
  work.
- **Ticks.** The conductor's hosts put a tick on the serial writer every
  250 ms; each save runs there, through the ordinary save path.
- **The hint on leaving.** The browser sends the flush hint when its page is
  hidden or left (`visibilitychange` to hidden, `pagehide`), not when the page
  loses focus. This changes decision 1's "flush on leave". The page's own
  reload after a rebuild sends no hint. The hint and the save-state indicator
  are about the page's own document, which the page script names (on a
  collection page, its `index.md`, not the items it lists).
- **The configuration key.** `"autosave": { "enabled": true, "idle-ms": 2000 }`
  in `<site>/.presemble/config.json` (decision 9). Only the conductor daemon
  reads it, from the site's own file; builds never do.
- **Auto-save off.** A flush and shutdown save nothing; shutdown names each
  document with unsaved edits. Only explicit saves write: Save, save-all,
  `:w`, and Retry.
- **Retry.** The Save failed banner's Retry is an explicit save of that one
  document (`POST /_presemble/save`). When the document is in conflict with
  its file, it offers the conflict choices instead: overwrite the file with
  the edits, or cancel.
- **Ctrl-C.** SIGINT and SIGTERM shut the conductor daemon down through its
  writer, so dirty documents are saved first. A second signal exits at once.
- **No Saving state.** A save happens within one command, so no client could
  show it: the states are Saved (with the time, epoch ms), Unsaved, Conflict
  and Save failed (decision 8).
- **Publishing.** A published build starts at most 2 s after the first save
  of a burst, however steadily saves keep coming; saves during a build are
  coalesced into one build after it (decision 7).

### Done: auto-save

- 0b49c2c: the conductor saves every dirty document after it has been idle.
- bdff373: flush on request and on shutdown, and the save state of each
  document.
- 0925763: the conductor daemon drives auto-save, reads its configuration, and
  bounds publishing waits; Ctrl-C shuts down through the writer.
- 1374c31: the language server brings a buffer to canonical text before it is
  saved, and shows a failed save.
- e6a6832: the editorial page ignores message types it does not know (a
  save-state message no longer reloads it).
- 71c97e4: the page's save state, the Save failed banner with Retry, and the
  flush when the page is hidden or left.

## Out of scope

Undo belongs on N and gets its own ADR; auto-save only makes the file follow
N.

## Consequences

- An author never needs to remember to save, and "Unsaved" becomes a
  two-second state.
- Hand-written files are rewritten canonically on their first edit; run
  `presemble fmt` first to keep the history readable.
- A conflict is the only thing that keeps edits off the disk, and it is
  always visible.
- A text editor flags a file as changed on disk only when something outside
  Presemble wrote it (sed, git).

## Amends

- ADR-065 §5 is realised by decision 3; ADR-065 §4 is withdrawn by ADR-069.
- ADR-068: publishing after a save is coalesced (decision 7).

## Related

ADR-043, ADR-048, ADR-049, ADR-059 (stages 8 and 9), ADR-061, ADR-065,
ADR-068, ADR-069.
