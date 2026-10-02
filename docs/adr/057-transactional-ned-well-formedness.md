# ADR-057: NED edits are journaled transactions; well-formedness is enforced, schema validity is reported

## Status

Proposed

## Context

ADR-056 made suggestion accept apply structured mutations under one write lock, but deferred checking that the resulting document conforms to "Wave 1's transactional validation". The node store has no notion of a transaction, and the set of documents an edit affects is computed ad hoc. Raw `/_presemble/apply` programs can leave arbitrary nodes behind.

Principle (2026-10-01): schema-invalid content is legitimate state. It can be edited, held and saved; schema validity decides only whether it can be published. The only input that can fail to parse is data crossing a boundary (files on disk, suggestion payloads, wire programs). Nodes inside the store are already parsed, and this ADR makes that true by construction.

## Decision

1. **Transactions.** Every editorial mutation (suggestion accept, `/apply`, NED REPL edits) runs as a transaction:
   - take the write lock;
   - clone the store (O(1), `im`);
   - `start_journal` on the clone;
   - apply the mutation;
   - `take_journal`;
   - run the commit checks (§3–§4);
   - swap the clone in.

   On rejection the live store is untouched. An empty `TouchLog` means "no change".
2. **Affected documents** = `owning_document` of every node in `created ∪ touched`. Removed nodes need no lookup, because their former owners are recorded as touched. Affected documents are marked dirty and rebuilt.
3. **Scope (a capability boundary, not validity).** An editorial transaction may only create or touch nodes owned by a live `document`.
   - It may not create, remove or rename a document *frame* element (`document`, `preamble`, `body`). Creating or deleting a page is a lifecycle operation with file-system effects and has its own command. Emptying content is the editorial equivalent.
   - Slots are content containers. They may be created (with a `name`) or removed; a removed slot serializes the same as an empty one.

   Violations produce `TxnError::OutOfScope { node }` or `TxnError::FrameChange { node, kind }`.
4. **Well-formedness gate.** Every element child of a `body` or `slot` that the transaction touched (the `owning_block` of each created or touched node in an affected document) must decode with `block_from_node`.
   - Violations produce `TxnError::Malformed { node, error: BlockDecodeError }`.
   - This is the only content check that can reject a transaction. It asks "can this be written to disk?", never "does this match the schema?".
5. **Validity is reported, never enforced.** After commit, each affected document is validated against its grammar.
   - Diagnostics are stored per document in `SiteState`, reusing `build_errors` keyed by document.
   - The document is `Unpublishable` while any diagnostics are present.
   - Edits, suggestion accepts and saves are never refused on grammar grounds.
   - Gating builds and publishing is Wave 4.
   - Amended by ADR-061 (publish gate).
6. **Saving is total for well-formed documents.** `serialize_from_store` succeeds for any document that passes §4. A missing `preamble` or `body` serializes as empty. Getting the text right is the serializer's job (canonical escaping, ADR-056 §3 as amended), not a reason to refuse an edit.
7. **Errors.** `TxnError = OutOfScope { node } | FrameChange { node, kind } | Malformed { node, error }`. Suggestion accept cannot produce these: addressing targets blocks or containers, and payloads are decoded first. So no new `StaleCause` is needed.

## Why

Rejecting schema-invalid edits makes intermediate states impossible to reach, such as half-filled required slots or reordered sections. Editors would then be forced to work in schema-valid jumps. Publishing is the real safety boundary.

Well-formedness is different. A node the serializer cannot write would turn a save into data loss, so it is enforced at the point where the node is created.

## Alternatives considered

- **Grammar gate on transactions:** rejected; it contradicts the principle.
- **No well-formedness gate; serializer writes a best-effort form of unknown blocks:** rejected. There is no faithful form, so it would silently drop or mangle data on save.
- **Undo or rollback via an inverse journal:** unnecessary. A copy-on-write clone makes rollback free.

## Consequences

- Invalid documents round-trip through disk. Markdown does not record slot names, so a document with an empty required slot may re-ingest with its blocks assigned to different slots. Block order and content are preserved, and diagnostics reflect the result.
- `DocumentDecodeError` shrinks to malformed blocks only, which §4 makes unreachable from editorial paths.
- Undo scope is not decided here.

## Related

ADR-040, ADR-054, ADR-056; memory `feedback_invalid_is_saveable`.

## Open questions (to be resolved before Accepted)

1. **Undo scope.** Record a bounded history (e.g. last 64 states) now with no undo command, or ship per-document undo in Wave 1.
2. **Page-body leak.** `serve`'s public file handler serves unsaved edits, working-tree-only pages and build errors to anonymous readers. Options: document it as a known gap now and gate HTML in Hosted mode later, or gate it now.
3. **Slot reshuffle on reload.** Should markdown record slot boundaries (e.g. an HTML comment marker), or is reassignment plus diagnostics acceptable?
4. **Well-formedness line.** Confirm: editorial transactions may not leave unknown block kinds or malformed blocks (e.g. a non-numeric heading level).
5. **Lossy canonicalization.** For text markdown cannot represent: a newline in a heading → space, a blank line in a paragraph → soft break, a multi-paragraph blockquote → one line. Is canonicalization acceptable?
6. **Inline markup in stored text.** The parser stores *literal* text (inline markup dropped: `*emph*` loses its emphasis on load). Keep literal text, with the serializer escaping inline characters, or store inline markdown source in the text nodes?
