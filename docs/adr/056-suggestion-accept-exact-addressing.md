# ADR-056: Suggestions address exactly one node; selections are query-only; accept applies structured mutations in Rust

## Status

Accepted

## Decision

1. **Exact addressing.** Every NED suggestion's selection must resolve to
   exactly one node of the kind its mutation requires:
   - SetText, SearchReplace → one Text node that is the **only child** of a
     single-text block (heading, paragraph, link, blockquote, list,
     code-block). `raw-html` was listed here; ADR-062 renamed it `html-block`
     and D4 removed it from text editing.
   - Replace, Delete, InsertBefore, InsertAfter → one block (direct child of
     a slot or of body).
   - InsertChild → one container (a slot or body element).
   The target must belong to a live document (`Element("document")`);
   superseded copies are not addressable. At creation, zero matches, several
   matches or the wrong kind is an error; at accept they set Stale with cause
   `Unresolved`, `Ambiguous { matches }` or `WrongKind`. An ambiguous suggestion is never applied and never
   applied to every match — a human resolves it by editing, which makes the
   suggestion redundant.
2. **SearchReplace** is a substring replacement inside one text node; the
   search string must occur exactly once. It is kept, distinct from SetText,
   because it detects drift: if the text was rewritten, it goes Stale instead
   of overwriting. Zero occurrences is `SearchNotFound`; more than one is
   `SearchNotUnique`.
3. **Payload validity.** Text produced by SetText or SearchReplace must round-trip
   through the block's markdown serializer and parser as the same single
   block (no newlines in headings, no blank lines in paragraphs, no block
   syntax injection). Tree payloads are non-empty, block-kind roots, inserted
   as an ordered group.
4. **Query-only selections.** Selections are evaluated in an evaluator root
   with only the query builtins and prelude registered. Selection source is
   never spliced into a program.
5. **Rust applies mutations.** On accept the conductor resolves and validates
   the target, then applies the structured mutation in Rust: text and delete
   through `ned::mutation`, while tree payloads are first decoded into
   canonical blocks (`content_bridge`) and inserted as an ordered group,
   under a single write lock. Success requires a change count of at least 1.
   Affected documents are collected before mutating and are marked dirty and
   rebuilt. `compose_ned_program` is removed.
6. **Lifecycle.** Failure at accept sets `Stale { reason, cause }` with a typed
   `StaleCause`:
   - `Unresolved`
   - `Ambiguous { matches }`
   - `WrongKind { expected, found }`
   - `SearchNotFound`
   - `SearchNotUnique { occurrences }`
   - `InvalidValue { detail }`
   - `NoChange`
   - `EvalError { detail }`
   - `Unspecified` (the default for suggestions persisted before this ADR)

   Stale → Rejected is allowed. The same
   checks run at creation and return errors. "Ambiguous" always means "the
   address matches several nodes"; content modified since creation is called
   "Drifted" (future graduated staleness, Wave 2).
7. **Ambiguity carries through.** Positional query helpers (`ned/nth-text`,
   `ned/nth-after`, `ned/first-after`) work per scope node and return the union; they never
   choose one of several scopes.

## Why

Before this, `SetText` lowered to "set every text node under the selection",
so a slot with several blocks got its text duplicated. SearchReplace reported
success with zero matches. Selections ran with mutating builtins bound, so
creating a suggestion could change content. A suggestion that can't name its
target exactly can't be reviewed or safely applied later. Precise addresses
make "what will this change?" answerable, and make "it no longer applies"
detectable instead of silently mis-applied.

## Alternatives considered

- **Auto-narrow block or slot selections to their single text node** — rejected:
  it hides addressing errors and makes the meaning of an address depend on
  the content.
- **Apply to all matches** (the previous behaviour) — rejected: this is the bug.
- **Pick the first match** — rejected: it guesses.
- **Replace SearchReplace with "address the text containing x" plus SetText** —
  rejected: it loses drift detection against concurrent manual edits.
- **A top-level `Ambiguous` status now** — deferred: both cases currently end
  the same way (manual resolution, then reject); revisit with Wave 2's
  graduated staleness.
- **Keep Clojure program composition with escaping** — rejected: structured
  application is simpler, typed, and cannot be injected into.

## Consequences

- Suggestions persisted before this ADR that address slots, blocks or whole
  documents go Stale (WrongKind or Ambiguous) at accept; none is mis-applied.
- MCP `suggest` fails on multi-block or empty slots with a hint;
  `suggest_body_edit` is limited to the body and requires a unique search string.
- The browser always builds block addresses and wraps them for text mutations.
- Superseded document copies are retired at re-ingest (their root becomes
  `superseded-document`) and are invisible to every document query; full
  garbage collection comes in Wave 1.
- Grammar conformance of the resulting document (for example a paragraph
  inserted into a heading-only slot, or a required block deleted) is not
  checked until Wave 1's transactional validation.
- Positional helpers order by (parent, index in parent), not by NodeId.
- The direct-edit path (`/_presemble/apply`) follows the rule client-side now;
  server-side enforcement comes with the typed protocol (Wave 3).
- Table cells are not addressable as text yet.
- Builds toward ADR-033 pinning: Wave 2 adds fingerprints and the
  Valid/Compatible states on top of this resolution step.

## Related

ADR-033, ADR-040, ADR-045, ADR-046, ADR-047, ADR-054;
memory `project_ned_exact_addressing`, `project_ned_node_editor`.
