# ADR-062: Raw HTML is forbidden in content; comments are trivia

## Status

Accepted

## Context

- **L12:** the content parser drops `Event::Html`, so HTML blocks are lost on
  save. ADR-060 P1 lists this as its one known exception.
- Three live bugs let HTML from content reach published pages:
  1. Paragraph and list text is rendered with `pulldown_cmark::html::push_html`,
     which passes inline HTML through unchanged (`template/src/data.rs` and
     `template/src/transformer.rs`). `a <script>…</script> b` is published as
     a live `<script>`.
  2. Heading and blockquote text is stored as literal text but rendered again
     as markdown, so `# x \<script>alert(1)\</script>` becomes a live
     `<script>`.
  3. Table cells are written unescaped (`<td>{c}</td>` in `data.rs`). The
     parser stores decoded `Text` in cells, so `\<script>` in a cell becomes
     `<script>`.
- ADR-061 makes every Error unpublishable: the whole site is published or
  nothing is.
- The author's rule: **save what the user typed; fail publish until the errors
  are solved.**

## Decision

### 1. Policy

Raw HTML is not allowed in content. Every HTML block is kept verbatim, saved,
never rendered, and reported as an Error, so the page is unpublishable under
ADR-061.

Content stays HTML-free even though CommonMark allows HTML, following the
direction of Hugo (goldmark's `unsafe = false` default) and other publishers:
presentation belongs in templates.

### 2. Comments are trivia (D1)

An HTML block that contains only comments (`<!-- … -->`, separated only by
whitespace) is not an element. It is trivia attached to the next block. Trivia
after the last block of a container is that container's end trivia.

### 3. Serve shows a placeholder (D2)

`serve` renders a body HTML block as a placeholder with the text
`Raw HTML is not published. Remove it to publish this page.` and the class
`presemble-html-removed`. The placeholder carries no `data-presemble-slot` and
is styled only in `inject.css`.

### 4. Inline HTML (D3)

Inline HTML is also an Error. It renders as escaped literal text. Inline
comments are stripped and are not an Error.

### 5. Editing HTML blocks (D4)

- An HTML block can be deleted, or replaced by a non-HTML block.
- An HTML block cannot be inserted through a suggestion payload and cannot be
  text-edited.

### 6. Trivia survives edits (D5)

- **Replace:** the replaced block's trivia moves to the replacement.
- **Delete:** the deleted block's trivia moves to the next block in the same
  container, or to the container's end trivia if it was the last block.

### 7. Element and store shape

- `ContentElement::RawHtml { html }` is renamed `HtmlBlock { source }`. Its
  store kind `raw-html` becomes `html-block`.
- HTML blocks count in the body index. Comments never do.
- In section 0, HTML blocks never fill a slot; they go to `unassigned`
  (ADR-060 P1).
- Trivia is stored in reserved attributes:
  - `_trivia-before` on a block;
  - `_trivia-end` on `slot`, `unassigned` and `body` nodes.

  Each holds an `Element("trivia")` whose children are `Text` nodes holding
  the comment source verbatim.

## Alternatives considered

- **A `Comment` element variant.** Every consumer (slot assignment, the body
  index, validation, rendering, NED addressing) would have to skip it, so
  "a comment never fills a slot" would hold only by convention. Rejected.
- **Sibling `comment` nodes in the store.** Every consumer of a child index
  would have to filter them out. Rejected for the same reason.

## Consequences

- No HTML from content is rendered, in `build` or `serve`. Inline HTML is
  escaped; block HTML is replaced by the placeholder in `serve` and blocks
  publishing.
- Body indices in existing files that contain HTML blocks shift, because those
  blocks now count.
- Known limits:
  - **L13:** HTML in headings and blockquotes is read as literal text: shown
    escaped, not reported, written escaped on save, until they store their
    source (ADR-057 OQ6). A heading is stable from the first save on; a
    multi-line blockquote from the second, when its lines are joined.
    Resolved for headings by ADR-063; blockquotes by ADR-059 (stage 8;
    package Q is dropped).
  - **L14:** an HTML block of CommonMark types 1–5 (`<script>`, `<pre>`,
    `<style>`, `<textarea>`, comments, processing instructions, declarations,
    CDATA) with no end condition runs to the end of the file, as CommonMark
    specifies. If an edit adds content after such a block, the next load
    folds that content into the HTML block. The bytes are kept and the Error
    is reported.
  - **L15:** inline HTML in link text, image alt text and image-bearing
    paragraphs is still dropped, until ADR-059 (inline nodes; stages 5 and
    5b; write side of L16 in stage 6).
    Image-bearing paragraphs escape a literal `<` as `\<`.
  - **L16:** GFM drops table cells beyond the header's column count. An
    unescaped `|` inside a code span splits the cell (`` `a|b` `` reads as
    `` `a `` and `` b` ``), so the cells pushed past the last column are lost
    on save. Write `` `a\|b` `` instead.
  - **L17:** code spans in headings and blockquotes lose their backticks on
    save (``## The `<div>` element`` is written as `## The \<div> element`).
    This is fixed when headings and blockquotes store their markdown source
    (ADR-057 OQ6); raw HTML in them then becomes an Error too.
    Resolved for headings by ADR-063; blockquotes by ADR-059 (stage 8;
    package Q is dropped).
- An HTML comment between two same-kind lists keeps them separate, because the
  comment is trivia of the second list. Follow-up: when a save would put two
  same-kind lists next to each other with no comment between them (L10), the
  serializer inserts `<!-- -->`.

## Amendments

- **ADR-057 §3 and §4:** `_trivia-before` and `_trivia-end` are reserved
  attributes, not content. The well-formedness gate decodes trivia along with
  the block or container it is attached to.
- **ADR-060 P1:** resolves the known exception L12. HTML blocks are elements
  and are no longer lost on save.

## Related

ADR-057, ADR-059, ADR-060, ADR-061.
