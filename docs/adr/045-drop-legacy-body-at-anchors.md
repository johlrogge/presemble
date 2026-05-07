# ADR-045: Drop Legacy body-at Anchor Migration; Accept Degraded Indicator

## Status
Accepted

## Decision

Remove `AnchorJson::BodyNth` from the suggestion wire format. Legacy
NED selections of shape `(ned/body-at (ned/doc-by-path "FILE") IDX)`
are no longer translated into a structural anchor for indicator
placement; they fall through to `AnchorJson::Doc { file: Some(...) }`
and the browser shows a file-level indicator only.

`ned/body-at` itself remains a valid NED selection primitive — only
the wire-projection layer's special-casing of it is removed.

## Why

The structural-anchor stack (Phases 1–4) replaced positional
selection with heading-anchored, kind-qualified, offset-relative
selectors. After Phase 4, no new browser edit or suggestion emits
`(ned/body-at ...)`; the remaining occurrences are persisted
suggestion JSON files written before the migration.

Translating those legacy selections into `AnchorJson::Structural` at
the projection boundary requires document context (to compute
`heading_text`/`node_kind`/`offset`), but `derive_anchor` is a pure
string function called from `From<&NedSuggestion> for
NedSuggestionJson`. Wiring conductor/document context through that
trait — or persisting the derived anchor at suggestion-creation time
and migrating existing files — is a non-trivial coupling change to
support a vanishing dataset.

Concrete inventory at decision time: one debug suggestion file
(`site/.presemble/suggestions/ned/sug-18acf79699bc5e1e.json`) carries
a `body-at` selection. No future suggestion will. The cost of full
translation is large; the cost of degradation is one indicator
showing the file rather than the paragraph until the user re-creates
or deletes the suggestion.

## Alternatives considered

- **A1: Pass conductor context into `derive_anchor`.** Threads the
  conductor handle through the `From` trait or replaces it with a
  fallible function. Touches every call site. Couples a pure
  projection layer to live state for a transient legacy concern.
  Rejected: invasive change for ephemeral data.

- **A2: Translate at suggestion-creation time and persist the
  derived anchor on the JSON.** Adds a new invariant ("persisted JSON
  contains pre-derived AnchorJson") that has to be defended forever,
  plus a one-shot migration over existing files. Rejected: durable
  schema change for transient data.

- **A3 (chosen): Drop `BodyNth`, let legacy `body-at` selections
  fall through to `Doc { file: Some(...) }`.** Smallest change.
  Honest about what we know (file is captured; structural triple
  isn't). Forward-compatible: new suggestions emit `Structural`
  directly via the parsers added in Phase 3.

## Consequences

- One persisted legacy suggestion (or any future hand-authored
  `body-at` suggestion) shows a file-level indicator in the browser
  instead of pinpointing the paragraph. Acceptable: user can delete
  or re-create.
- The wire-projection layer (`From<&NedSuggestion> for
  NedSuggestionJson`) stays pure — no conductor or document context
  threaded through.
- `try_parse_ned_body_at`, `AnchorJson::BodyNth`, the `body-nth`
  branch in `_suggestFindTarget`, and the corresponding tests are
  removed. Net code reduction.
- `ned/body-at` remains usable as a NED selection primitive;
  programmatic users (NED CLI, MCP, prelude composition) are
  unaffected.
- Establishes a precedent: deletion-with-degradation is preferred
  over context-threading for transient legacy formats. If a future
  enrichment is genuinely needed, the right move is a separate
  enrichment step (`enrich_with_doc_context(...) ->
  EnrichedSuggestionJson`) layered after the pure projection — not
  pushing context down.
