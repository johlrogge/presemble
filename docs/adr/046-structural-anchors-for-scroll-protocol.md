# ADR-046: Structural Anchors Replace DOM-ID Anchors in Browser Reload/Scroll Messages

## Status
Accepted

## Decision

The conductor → serve → browser scroll-anchor wire uses
`editorial_types::StructuralAnchor` (file, slot, heading_text,
node_kind, offset) — the same shape `AnchorJson::Structural` already
uses for suggestions. String DOM-ID anchors (`presemble-body-N`) are
gone from this wire.

`AnchorJson::Structural` is refactored to wrap `StructuralAnchor`
rather than re-declare the fields, eliminating drift risk between the
two sides of the wire.

## Why

The browser already has a structural resolver
(`_suggestFindStructural`, hoisted and renamed
`_findByStructuralAnchor` in this slice) used for suggestion
indicators. The positional ID `presemble-body-N` is a
structural-editing smell: it encodes a global body index that is
unstable across edits, brittle across templates, and meaningless to
the document model.

Phase 4 of the structural-anchor migration eliminated positional IDs
from edit and suggest paths. Phase A of the readers-off-body-IDs plan
removed `AnchorJson::BodyNth`. Phase B (this ADR) closes the last
remaining wire that emits string DOM-IDs to the browser.

After this ADR:
- One wire format for "where this anchor lands": `StructuralAnchor`.
- One resolver in the browser: `_findByStructuralAnchor`.
- No positional indexing anywhere on the scroll/reload paths.

## Alternatives considered

- **Keep string DOM-IDs and rely on `data.rs` continuing to emit
  them.** Rejected: locks in the structural-editing smell. Every new
  rendering path (`transformer.rs` already omits these IDs) has to
  remember to emit a global index that has no schema meaning.
- **Place `StructuralAnchor` in `conductor::protocol`.** Rejected:
  conductor only carries the value through; `editorial_types` already
  hosts the suggestion-side use of the same shape, and the type has
  no protocol-specific behaviour. Putting it next to `Suggestion` is
  the natural home.
- **Keep `AnchorJson::Structural` as a separate definition with the
  same fields.** Rejected: two parallel definitions of the same wire
  shape. Inevitable drift.
- **Provide a one-release legacy string-anchor fallback in
  inject.js.** Rejected: conductor + serve + inject ship together;
  there is no cross-version interop to protect. A dead branch in JS
  rots silently; a `console.warn`-and-skip is enough if a desync
  ever appears.

## Consequences

- Wire-format breaking change between conductor and the browser.
  Acceptable: the three components ship as one release artefact.
- `body_element_anchor_at_line` returns
  `Option<StructuralAnchor>`.
- `EditBodyElement` handler computes the structural anchor from the
  post-mutation document; this requires re-parsing the document text
  and loading the grammar (microsecond cost).
- `inject.js`'s `_suggestFindStructural` is hoisted out of the
  mode-management IIFE and renamed `_findByStructuralAnchor` so the
  scroll handler and the post-reload IIFE share one resolver.
- `data.rs:500-582` continues to emit `id="presemble-body-N"` for
  now; Phase C of the readers-off-body-IDs plan removes that
  emission once no reader remains.
- Establishes structural anchors as the single anchor representation
  across the conductor wire — no more parallel "string for scroll,
  struct for suggestions" model.

## Related

- ADR-045: Drop legacy `body-at` anchors (suggestion side).
- Phase 4 of the structural-anchor stack (commits `73ecbb1..ee247e8`).
- Phase A of `migrate-readers-off-body-ids` (commits `c4535f6`,
  `733494e`).
