# ADR-058: Link expression surface syntax

## Status

Accepted

## Context

ADR-030 introduced link expressions in content and left their markdown syntax open. Documentation and the example blog use `[posts](->> :post (refs-to self))`, which CommonMark (pulldown-cmark) does not parse as a link: a bare destination cannot contain spaces. Only `%20`-encoded and escaped angle-bracket spellings ever parsed from content, and nobody writes those; the raw form only ever worked at the REPL. Link expressions are not general code: their purpose is to state, explicitly, which other pages a page refers to.

## Decision

1. Canonical form: `[text](->> :source op ...)`. No outer parentheses.
2. The destination is a closed grammar, not an s-expression:

       thread  := '->>' WS ':' keyword (WS op)*
       op      := '(' 'sort-by' WS ':' keyword [WS (':asc' | ':desc')] ')'
                | '(' 'take' WS digits ')'
                | '(' 'filter' WS ':' keyword WS string ')'
                | '(' 'refs-to' WS ('self' | string) ')'
       keyword := [A-Za-z0-9_-]+          (same charset as slot names)
       string  := '"' ( [^"\\] | '\\' ["\\] )* '"'
       WS      := [ \t]+

   Anything else is a content diagnostic. The expression is parsed into `LinkTarget` data and interpreted only by the link-expression resolver. It is never handed to the Presemble Lisp reader or evaluator.
3. `->` (thread-first) is not accepted; the diagnostic suggests `->>`.
4. A link expression must be a whole top-level paragraph: one line, nothing before or after it, not inside a list or blockquote. Presemble recognises that paragraph from its source; pulldown's link grammar is not used for it. A link expression must sit in a preamble slot; in the body it is ignored, with a diagnostic.
5. Legacy spellings are read and rewritten in canonical form: `[t]((->> ...))`, `[t](<(-\>\> ...)>)` and `%20`-encoded destinations. They map to the same closed grammar, so they add no expressive power.
6. PathRef (`[t](:path)`) is unchanged.
7. An expression-like destination inside running text (`a [x](:y) b`, `a [x](->> :p) b`) is not a link expression and gets a diagnostic.
8. `(refs-to self)` matches every page that references the current page. A reference is a reference wherever it appears: preamble link slots, PathRef expressions, and links in body text. Publishing relies on the complete set of references, so none may be ignored.
   - References from every parsed page count, publishable or not. Schema validity gates publishing, not referring (ADR-060 §5).
   - References in `preamble > unassigned` (ADR-060 P1) count: link hrefs, PathRef expressions, and the inline links in any paragraph there.
   - Images never count. Thread expressions never count, in a slot or in `unassigned`: they compute references, they do not declare them.
   - Body and inline references are pending ADR-059 (inline segments); until inline links are modelled as typed references, only the preamble is read.
   - Known gap: a page that fails to parse is not in the store, so its references are skipped.
9. **Explicit slot markers.** A link expression may end with a slot marker,
   `[label](->> ...) {#slot}`, the same `{#name}` syntax schemas use. A marked
   expression fills the preamble link slot `slot` wherever it appears in the
   preamble; the label is display text only and never names a slot. Marked
   expressions are invisible to positional assignment: they neither consume the
   positional cursor nor end another slot's run, and they are always placed
   (exceeding `occurs` is a validation error). Unmarked expressions, and all
   other preamble elements, are assigned by position. A marker that names no
   link slot is not slotted: the expression is kept and saved, never
   rendered, and reported as an error (ADR-060 §5). A label that equals a link slot name without a
   marker gets a hint to add the marker. Markers are written back on save;
   slots are written in schema order. Markers are recognised on link
   expressions only (thread, PathRef and legacy spellings); extending them to other preamble elements awaits inline
   segments (ADR-059).

## Evaluation and invalidation

- **One implementation of the operations.** `expressions::apply_link_ops` applies `sort-by`, `take`, `filter` and `refs-to` in order over items that expose a URL and field text (`ThreadItem`). The store pipeline (`node_store_bridge::store_pipeline`) and the REPL both use it. Items arrive in base order, URL ascending.
- **Declared references only.** `store_pipeline::links_to_index` is the only edge collector. It reads the declared references of decision 8; expression results and synthesised links are not edges. `refs-to` in link expressions and the REPL `refs-to` / `refs-from` primitives read the same index.
- **Query nodes.** Every thread expression in a preamble slot of page P has a `link-query` node Q holding its last result URLs. `Derived` edges record the dependency: `P_sem --query--> Q --over--> stem-items(S)`, where `stem-items(S)` stands for the item set of the source stem S. These edges are ephemeral: they are rebuilt with the store and never serialized. Thread expressions in `unassigned` get no query node.
- **Propagation.** A change to document X re-renders X, plus every page P whose query result changes, or whose old or new result contains X. The second half is the D3a rule: the goal is that a change to a referenced page triggers a more thorough analysis (which fields P's template reads) that may or may not result in a re-render; the D3a rule is the conservative interim.
- **Body-only edits.** Each document stores a preamble fingerprint: a hash of its canonical serialization with the body emptied, which covers slots, `unassigned`, hrefs and markers. When the fingerprint is unchanged, the edit is body-only and re-renders X alone.
- **Known limits.**
  - D3a: a page re-renders whenever X is in its result, even if it reads nothing that changed.
  - D7: a page that displays data from a page it links to (not through a link expression) is not re-rendered when that page changes.
  - D9: an external file change (`FileChanged`) is a full rebuild. Comparing the stored query results before and after the repopulate, as `CreateContent` already does, is the basis for a minimal one later.
  - A body-only edit of X does not re-render pages whose templates show X's body or an excerpt of it (the D3a family: what P's template reads is not analysed yet).
  - An unassigned paragraph that contains an image loses its link syntax when parsed (a save-path gap), so the links in it are not counted.
  - Stored fingerprints, like query nodes, exist in memory only; they are recomputed whenever the store is repopulated.
- **Destinations.** Only page references count: `expressions::page_reference` drops destinations with a scheme (`://`, `mailto:`), protocol-relative `//…` URLs, `#…` anchors and expressions, strips a leading `:` and normalises the rest.

## Alternatives considered

- `[t]((->> ...))` as canonical: looks like "paste any expression here" and invites arbitrary evaluation.
- Escaped angle-bracket or `%20` forms: valid CommonMark, but unreadable.
- Inline link expressions: they assemble slot data and have no inline meaning.
- Accepting `->`: two spellings with one meaning.

## Consequences

- The documented syntax becomes real; the example blog's author page works.
- Values may contain spaces and quotes (string escapes); sources and fields are validated keywords.
- Operations must actually be applied in the build pipeline. They had been ignored since the conductor became the sole build path (07c4865); see "Evaluation and invalidation".
- A paragraph whose text would read back as a link expression is escaped on write (`\[`).
- ADR-030's open question on syntax is resolved; its `(-> /post ...)` sketch is superseded.
- Open: a PathRef in a slot and a Link block to the same page both expose target data; revisit whether both are needed.

## Related

ADR-030, ADR-037, ADR-057.
