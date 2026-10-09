# ADR-070: Auto-save: the conductor saves every edit after 2 s idle

## Status

Proposed

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
     sends one when its page is hidden or closed, or loses focus.
   - Auto-save goes through the same save path as an explicit save; there is
     no second writer.
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
9. **Configuration.** `.presemble/config.json` holds
   `autosave: { enabled, idle_ms }`. It is on by default locally. Builds never
   read or write it differently.
10. **Crash protection** is the idle window: at most about `idle_ms` of edits.
    There is no journal.

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
