# ADR-047: Retire legacy Suggestion type and HTTP surface; drop persisted legacy JSON

## Status
Accepted

## Decision

The legacy `editorial_types::Suggestion` shape — `SuggestionTarget::{Slot,
BodyText, SlotEdit}`, `SuggestionStatus`, the `Command::SuggestSlot* /
SuggestBodyEdit / GetSuggestions / AcceptSuggestion / RejectSuggestion /
GetSuggestionFiles` family, the `/_presemble/suggestions /
accept-suggestion / reject-suggestion / suggest-* / suggestion-files`
HTTP endpoints, and the corresponding browser branches in `inject.js` —
is removed. `NedSuggestion` is the only suggestion shape going forward.

Persisted legacy suggestion JSON in `site/.presemble/suggestions/sug-*.json`
(both Pending and historical entries) is **deleted** on first boot with
the new code. No migration. The 9 Pending legacy suggestions in the
working set are casualties; the user must re-create any they still
want.

`Command::EditSlot` and `Command::EditBodyElement` survive Phase C —
they are not suggestion paths, and the link picker still routes through
`EditSlot`. Their retirement is gated on NED gaining link/image
mutations and is tracked separately.

## Why

Phase A and Phase B of the larger NED-edit-substrate plan delivered
NED-on-NodeStore as the single browser-edit substrate. NED suggestions
(`NedSuggestion` with selection-as-Clojure + structured mutation) are
strictly more expressive than the legacy three-variant target type, and
Phase 4 of the structural-anchor work plus the suggest-mode UX commits
already routed all new browser-authored suggestions through them.

What remained was a duplicated surface: every legacy reader still
existed in browser, HTTP, conductor, REPL, MCP, and on-disk format.
Keeping both shapes alive split the suggestion model into "the new
canonical one" and "the legacy compatibility one," violating
`one-mutation-substrate` (project memory `project_next_priorities.md`)
and accumulating dead branches that drift.

Persisted legacy JSON: there are 9 Pending entries the user could in
principle want to keep. The user explicitly chose "drop everything"
when asked: faster, cleaner, no migration code surface that gets read
once and dies. The historical Accepted/Rejected entries are write-once
provenance that nothing reads at runtime; deleting them loses cold
data but no live workflow.

## Alternatives considered

- **Migrate Pending to NED at first boot.** Lower
  `SuggestionTarget::{Slot, BodyText, SlotEdit}` to `(selection,
  mutation)` pairs using the same logic the MCP wrapper uses. Keep
  Accepted/Rejected on disk. Rejected by the user: "drop everything."
  The 9 Pending entries are debug data, not load-bearing work.

- **Keep on disk, ignore at runtime.** Files survive but no code reads
  them. Rejected: orphaned data is a smell; the disk format becomes a
  graveyard the next contributor has to navigate.

- **Wait for NED link/image mutations and retire `EditSlot`/
  `EditBodyElement` together.** Rejected as a Phase C scope: NED link/
  image mutations are a non-trivial vocabulary extension that doesn't
  belong in this ADR. Phase C's scope shrinks to suggestions only.

## Consequences

- Five-wave implementation: ADR (this), browser, HTTP, conductor +
  REPL + MCP, types + first-boot deletion + docs.
- The `Suggestion`, `SuggestionTarget`, and (legacy) `SuggestionStatus`
  types are deleted. `NedSuggestionStatus` (which carries `Stale`)
  stays as the only suggestion-status enum.
- Every `SuggestionJson` projection, `From<&Suggestion> for
  SuggestionJson`, and the `target_type: "slot" | "body" | "slot_edit"`
  wire field are gone. Browser code paths `_suggestFindTarget` legacy
  branches, `_suggestAccept` text-replace-then-accept dance, the
  legacy `_fetchSuggestionCount` / `_fetchSuggestionFiles` / `_suggestEnter`
  fetch-and-merge logic are removed.
- Conductor commands `SuggestSlotValue`, `SuggestBodyEdit`,
  `SuggestSlotEdit`, `GetSuggestions`, `AcceptSuggestion`,
  `RejectSuggestion`, `GetSuggestionFiles` are deleted. The `suggestions:
  Arc<RwLock<HashMap<SuggestionId, Suggestion>>>` field on `Conductor`,
  `persist_suggestion`, `load_suggestions`, and the startup load go
  with them.
- REPL primitives `(suggest …)` and `(get-suggestions …)` in
  `editor_server` rebind to NED equivalents (or are dropped if the
  replacement shape diverges meaningfully).
- MCP server's legacy `get_suggestions` projection path that surfaces
  legacy `Suggestion` records is deleted. The `suggest` and
  `suggest_body_edit` MCP convenience wrappers stay (they already
  route through `Command::CreateNedSuggestion` and don't touch the
  legacy type).
- One-shot deletion at conductor startup: enumerate
  `site/.presemble/suggestions/sug-*.json` (top-level only — leave the
  `ned/` subdirectory alone) and remove each. Idempotent and
  best-effort: a missing directory is fine; a permission error logs
  and moves on. No backup. The user pre-approved.
- User-facing site copy that still references `SuggestSlotEdit` by
  name (`site/content/feature/editorial-collaboration.md:32`,
  `site/content/index.md:49`, `site/content/guide/user-guide.md:556`)
  is updated as part of Wave ε.
- LSP code-action plumbing is verified during Wave δ. If the LSP still
  consumes `Command::GetSuggestions` / `AcceptSuggestion` directly, it
  is rerouted through the NED commands before the legacy commands are
  deleted.

## Related

- ADR-045: Drop legacy `body-at` anchors (suggestion side, projection layer).
- ADR-046: Structural anchors replace DOM-ID anchors in browser reload/scroll.
- The bigger plan at `~/.claude/plans/let-s-plan-out-replacing-shimmying-gosling.md`,
  section "Phase C — Suggestions as NED programs (revised)".
- Project memory `project_next_priorities.md` — "one mutation substrate
  (NED on NodeStore)".
