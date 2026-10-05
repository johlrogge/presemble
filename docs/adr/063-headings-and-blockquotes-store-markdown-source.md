# ADR-063: Headings and blockquotes store markdown source

## Status

Proposed

## Context

- ADR-057 OQ6 asks whether text nodes store literal (decoded) text or inline
  markdown source. Paragraphs and lists already store source (ADR-056).
  Headings and blockquotes still store decoded text: inline markup is dropped
  on load and escaped again on save.
- This causes known limits:
  - **ADR-062 L13:** HTML in headings and blockquotes is read as literal text.
    It is shown escaped, not reported, and written escaped on save.
  - **ADR-062 L17:** code spans in headings and blockquotes lose their
    backticks on save (``## The `<div>` element`` is written as
    `## The \<div> element`).
  - **ADR-060 RQ8 L18:** a heading cannot end in a literal `{#name}`, because
    the stored text has no escapes.
- ADR-057 OQ5 asks whether lossy canonicalization is acceptable. A
  multi-paragraph blockquote is joined into one line on save.
- ADR-062 D3 ("inline HTML is an Error") is narrowed by L13: it does not
  reach headings and blockquotes, because their decoded text cannot tell a
  bare tag from an escaped one.

## Decision

This resolves ADR-057 OQ6 for headings and blockquotes, and OQ5 for
multi-paragraph blockquotes.

### 1. Model

- `Heading { level: HeadingLevel, source: MarkdownText }`. `source` is the
  inline markdown of the heading line, without the `#` markers and without a
  closing `#` run.
- `Blockquote { source: String }`. `source` is the verbatim markdown of the
  blockquote, `>` markers included. This mirrors `List`, and keeps
  multi-paragraph blockquotes intact.
- A paragraph keeps `text: String`, which already holds source, until ADR-059
  S-A.
- Containers (package C) are deferred.

### 2. `MarkdownText` and `AsPlainText`

- `MarkdownText` (in `content::inline`) is a newtype over a markdown source
  string.
- `AsPlainText` (`fn plain_text(&self) -> String`) is the one way readers get
  the text of a source value.
- These are the seam that ADR-059 builds on: `Segment::MarkdownText(MarkdownText)`.

### 3. Plain-text rule

Walk the heading's inline events:

- `Text` and `Code` are appended.
- `InlineHtml` and `Html` are appended, unless they are comment-only.
- Soft and hard breaks become a space.
- Everything inside an image (`Start(Image)` to `End(Image)`) is skipped.

Finally, normalise: split on Unicode `White_Space` and join with U+0020. The
JavaScript equivalent is `split(/\p{White_Space}+/u).filter(Boolean).join(' ')`.

**Contract:** a heading's plain text equals the normalised text content of
its rendered node tree.

### 4. Readers

These readers use **plain text**:

- the structural anchor `heading_text`;
- `ned/heading-with-text`, through a new `:heading-text-equals` filter;
- the `capitalized` constraint and its capitalize fix;
- `input.title`, as `Value::Text` plain text until heading slots render
  inline nodes (ADR-064);
- link-expression `field_text`.

`Search` and `SearchReplace` match **source**, as they do for paragraphs under
ADR-056.

### 5. Rendering

Content reaches templates as node trees built from markdown events (ADR-064,
package N). Templates never receive HTML. Until heading slots render inline
nodes (ADR-064), a heading slot's template value is its plain text: text, not
HTML.

Browser editors edit the stored text carried in `data-presemble-text`, which
is only emitted in editorial output.

### 6. Store shape

The store shape is unchanged: one `Text` child, which now holds source. No
migration is needed, because the store is rebuilt from files. Persisted
suggestions that addressed decoded heading text may go Stale.

## Alternatives considered

- **Keep decoded text and escape on save.** This is today's model; it causes
  L13, L17 and L18. Rejected.
- **Inline node trees in the store now.** This is the long-term model
  (ADR-059), but it needs every reader and NED operation migrated at once.
  Deferred; `MarkdownText` is the step towards it.

## Consequences

- Inline markup in headings round-trips. Code spans keep their backticks.
- Bare HTML in headings and blockquotes becomes an Error, like in paragraphs.
  Writing about HTML stays possible through code spans and `\<`.
- A heading containing an inline image is now kept; it used to be lost on
  load.
- New known limit:
  - **L19:** setext (underlined) headings are canonicalised to ATX on the
    first save: an ATX heading is one line, so hard breaks become spaces, and
    a heading ending in a `#` run gets an escaped `\#`. Suggestions on a
    setext heading are refused until that first save.

## Amendments

- **ADR-062:** L13 and L17 are resolved for headings (package H) and
  blockquotes (package Q). The D3 narrowing is withdrawn: bare HTML in headings
  and blockquotes is an Error.
- **ADR-060 RQ8:** L18 is resolved.
- **ADR-056:** `SetText` and `SearchReplace` on headings and blockquotes match
  source; `heading-with-text` matches plain text.

## Related

ADR-056, ADR-057, ADR-059, ADR-060, ADR-062, ADR-064.
