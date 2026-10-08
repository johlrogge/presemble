# ADR-059: Inline content is nodes in the NodeStore

## Status

Proposed (stays Proposed until stage 10).

Progress: stage 4 (headings) is done. Headings hold inline nodes in the
document model, the store and the renderer; heading edits are NED mutations
over nodes; `MarkdownText`, `capitalize_first` and the markdown-source adapter
are gone. See the stage 4 amendments below.

Stage 5b (one paragraph shape, §12) is done: there are no `link` or `image`
block kinds, a lone link or image is a paragraph whose shape decides its slot,
and an empty slot is filled by a block edit (§10 amendment). See the stage 5b
amendments below.

## Context

- The NodeStore models a document down to blocks
  (`document > preamble > slot | unassigned > block`, `body > block`). Below a
  block, content is one `Text` node holding a string whose meaning depends on
  the block kind:
  - paragraph, list and (since ADR-063) heading: markdown source;
  - blockquote: decoded text, with soft breaks joined;
  - link-block text and image alt: decoded text, with markup dropped;
  - table cell: the cell's source;
  - a paragraph that contains an image: decoded text with `<` and `{#`
    escaped again; its images are extracted and appended as image blocks.
- Every reader re-derives meaning from those strings:
  - rendering parses markdown again (ADR-064 V1–V4, `inline_children`,
    `table_cell_nodes`);
  - plain text parses again (`heading_inline_events`, `capitalize_first`);
  - references parse again (`paragraph_link_destinations`), and body links
    are not read at all (ADR-058 §8);
  - the browser re-derives stored text from markdown (`blockTextFromMd`) to
    diff it against `data-presemble-text`;
  - NED can only address "the single Text child of a block". Formatting
    cannot be targeted or changed structurally, and table cells cannot be
    addressed (ADR-056).
- Limits that follow from strings:
  - L6, L11, L13 and L17 (for blockquotes), L15, L16;
  - the leading-link loss: `[a](/x) trailing` drops ` trailing`;
  - images extracted from paragraphs;
  - merged multi-paragraph blockquotes;
  - the escaped-paragraph mismatch: `\# x` is stored with its backslash;
  - headings rendered as full markdown: `## 1. Intro` renders a list;
  - a link that edits into a paragraph changes block kind.
- Model (i), inline nodes, was chosen on 2026-10-01. ADR-063 introduced
  `MarkdownText` and `AsPlainText` as the seam towards it. Templates receive
  node trees (ADR-064). HTML in content is forbidden (ADR-062).
- Principle (user, 2026-10-06): the nodes are the content. Markdown on disk is
  one serialization of them, as HTML is for output. Writing markdown in
  canonical form is not a loss to be tolerated. It is the model working as
  intended: the file says what the nodes say, written one way.

## Decision

### 0. Nodes are primary; markdown is a serialization

- The NodeStore's block and inline nodes are the document.
- The parser reads markdown into nodes once, at the disk boundary.
- The serializer writes nodes as canonical markdown (§7). The first save of a
  file written in another style rewrites it canonically, by design.
- No information beyond the node model is kept, and none is needed: no
  source-style attributes, no source spans inside blocks, no "as written"
  text.

### 1. Target model directly, one block kind at a time

- There is no `MarkdownText` segment and no segment sequence.
- Each block kind moves from "string" to "inline nodes" in one stage, together
  with every reader of that kind. A block kind never holds a mix of the two.
- When the last stage lands, `MarkdownText` and every markdown parse outside
  the content parser are gone.

### 2. Inline vocabulary

Content domain (`content::inline`):

    pub enum Inline {
        Text(String),                         // literal text (§4)
        Emph(Inlines),
        Strong(Inlines),
        Code(String),                         // literal code-span content
        LineBreak,                            // hard break
        Link  { destination: LinkDestination, title: Option<String>, text: Inlines },
        Image { source: AssetRef, title: Option<String>, alt: Inlines },
        Html(InlineHtml),                     // verbatim, not comment-only; an Error (§14)
        Comment(Comment),                     // trivia (§14)
    }
    pub struct Inlines(Vec<Inline>);          // always in normal form (§4)

- Store element names are `emph`, `strong`, `code`, `line-break`,
  `inline-link`, `inline-image`, `inline-html` and `inline-comment`. Text is
  `Node::Text`.
- These are domain names, not HTML names. The store is the content model and
  HTML is one view of it (ADR-064 §2).
- An image's alt text is inline content (children of `inline-image`). The
  HTML `alt` attribute is its plain text (§8).

### 3. Inline containers and contexts

| Container | Context | Not allowed inside |
|---|---|---|
| `heading` | Heading | `line-break`, a newline in Text |
| `paragraph` | Paragraph | none |
| table `cell` | Cell | `line-break`, a newline in Text |
| `inline-link` (its text) | LinkText | `inline-link` |
| `inline-image` (its alt) | Alt | `inline-link`, `inline-image` |
| `emph`, `strong` | the parent's | as for the parent |

- `code` holds exactly one Text.
- `line-break`, `inline-html` and `inline-comment` have no children.

### 4. Text is literal; escaping is the serializer's job; normal form

- A Text node holds the characters the reader sees: escapes and entities are
  decoded. Only the serializer writes escapes.
- **Normal form**, held by every `Inlines` value and checked by the gate:
  - N1: no empty Text.
  - N2: no adjacent Text siblings.
  - N3: `emph` and `strong` are non-empty.
  - N4: the content of `emph` and `strong` neither starts nor ends with
    whitespace (normalisation moves it outside). Whitespace is Unicode
    White_Space (CommonMark flanking treats U+00A0 as whitespace).
  - N5: no space or tab next to a `\n` in Text. Paragraph Text never holds
    `\n\n`. Heading and cell Text never hold `\n` (normalisation turns it into
    a space).
  - N6: `code` Text is non-empty and holds no `\n`.
  - N8: `emph` whose only child is `emph`, and `strong` whose only child is
    `strong`, are one node; `strong` whose only child is `emph` becomes `emph`
    of `strong`. These shapes render the same, and the rewrite makes the
    parsed tree writable (`**_x_**` is read as `strong(emph(x))` but written
    `***x***`, which reads back as `emph(strong(x))`).
  - N9: adjacent emphasis (or strong) siblings merge.
  - N7: in the Heading, Paragraph and Cell contexts, the top-level run has no
    leading or trailing space, tab, `\n` or `line-break`. U+00A0 is kept,
    because CommonMark strips only spaces and tabs at block edges.
- A soft break is a `\n` in Text. A hard break is `line-break`.

### 5. Markers and link expressions are structure, not text

- A heading or paragraph that ends in a slot marker stores it as a `marker`
  attribute, not as Text, and its inlines exclude it:
  - `Heading { level, content: Inlines, marker: Option<SlotMarker> }`
  - `Paragraph { content: Inlines, marker: Option<SlotMarker> }`
- Validation is unchanged: marker misuse is an Error (ADR-058 §9, ADR-060).
  A literal trailing `{#x}` in Text is written `\{#x}`, so the two can no
  longer be confused.
- Link expressions (ADR-058) are unchanged. They stay block-level
  `link-expression` elements, recognised from paragraph source by their closed
  grammar. PathRef and the legacy spellings stay link expressions, never
  paragraphs with one link.
- An expression-like destination in running text (ADR-058 §7) is an
  `inline-link` whose destination classifies as an expression. The diagnostic
  is computed from the node.

### 6. Store encoding

    paragraph
      "See "                                   Text
      inline-link  href="/post/a"  [Reference links-to -> document /post/a]
        emph
          "this"
      " and "
      inline-image src="i.png"
        "a cat"                                 (alt inlines)
      inline-comment source="<!-- c -->"
      "."

- `inline-link`: `href` (the destination as parsed, a `LinkDestination`) and
  an optional `title`.
- `inline-image`: `src` (an `AssetRef`) and an optional `title`.
- `inline-html` and `inline-comment`: a `source` attribute and no Text child.
  Text walkers and SetText never see them.
- Table: `table > head > cell*` and `table > rows > row > cell*`, with an
  `align` attribute per column.
- Reference edges are recomputed whenever the store is populated or an
  affected document is rebuilt. `href` and `src` remain the source of truth.

### 7. Canonical markdown

**Parsing.** Inlines are built from the same pulldown pass that finds blocks.
Reference-style links are resolved at parse time. `parse_inlines` fails on
input that is not inline content of its context; it never reinterprets block
syntax. Every tree the parser produces is writable (P4).

**Canonical forms.** The serializer always writes:

- C1: emphasis as `*emph*`.
- C2: strong as `**strong**`.
- C3: links inline as `[text](dest "title")`, using `<dest>` when needed.
  Link reference definitions are not written.
- C4: a link whose text is one Text equal to its absolute destination, or to
  `mailto:` followed by that text, as an autolink `<…>`.
- C5: code spans with a backtick fence one longer than the longest run
  inside, padded with a space where required.
- C6: hard breaks as a backslash before the newline.
- C7: escapes only where a character would otherwise change meaning.
- C8: characters, not entities, except U+00A0 as `&nbsp;` and other invisible
  characters as numeric entities.
- C9: ATX headings (setext is read and written as ATX, L19).
- C10: block-level escaping (ADR-057) applied to line starts after inline
  serialization, in one pass.

**Writability.** A tree is writable in its context iff
`parse(serialize(t)) == t`. Some normal-form trees cannot be written, because
of CommonMark's flanking rules (for example `a` + emph(`.b`) + `c`). Mutations
that would produce one are refused (§9), and the gate rejects them.

**Fixed point.** For text read from disk:
`serialize(parse(serialize(parse(s)))) == serialize(parse(s))`.

### 8. Plain text and edit text

**Plain text** (`AsPlainText for Inlines`) keeps the rule of ADR-063 §3, now
applied to nodes:

- Text, `code` and `inline-html` contribute their text.
- `line-break` becomes a space.
- `inline-image` and `inline-comment` contribute nothing.
- `emph`, `strong` and `inline-link` contribute their children.
- The result is normalised: split on Unicode White_Space, joined with U+0020.

Contract: `plain_text(t) == normalise(text_content(render(t)))`.

Readers that use plain text:

- structural anchors (`heading_text`);
- `ned/heading-with-text`;
- the `capitalized` constraint. Its fix is a SetText on the first Text node in
  document order, and there is no fix when a code span or inline HTML comes
  before the first letter; comments are skipped;
- slugs, search, and `field_text` (sort and filter);
- the image alt attribute and the image-slot alt constraints;
- every string context of a node value (`display_text`, ADR-064 §6).

**Edit text** is used only on the server, by SearchReplace and edit lowering:

- It is the exact concatenation of Text and `code` Text, in document order.
- Each atom (`inline-image`, `inline-html`, `line-break`) becomes U+FFFC.
- Comments contribute nothing.
- An offset map leads back to (text node, byte offset).

The browser never computes plain text from markdown. JavaScript keeps only
`normalizeAnchorText` over DOM text, checked against the `normalize` group of
`text-vectors.json`.

### 9. NED mutations

All addressing stays exact (ADR-056).

**SetText**

- Targets one Text node in an inline container, at any depth, or the Text of
  a `code-block`.
- The payload is literal text and is never parsed as markdown.

**SearchReplace**

- Container-scoped (the general form): the target is an inline container, and
  the search runs over its edit text.
- Node-scoped: the target is one Text node.
- `search` must occur exactly once.
- The replacement takes the formatting of the match start: it goes into the
  Text node that holds the first matched character.
- Text nodes the match covers entirely are removed and the last one is
  trimmed. Formatting elements left empty are removed.
- Atoms are never removed. A match that contains U+FFFC is refused with
  `StaleCause::SpansAtom`.

**SetInlines(Vec<NodeTree>)**

- Targets one inline container.
- The payload is the new content as inline trees (stage 4 amendment: it was
  specified as `{ base, markdown }`). Trees are the N-form, the same shape EDN
  uses and the same payload form as `Replace`; nothing is parsed from markdown
  at accept. They must decode as inline content for the container's context.
- The drift check is the suggestion preview's base: a block whose canonical
  markdown differs at accept makes the suggestion `StaleCause::Drifted`.

**Normalisation and refusals**

- Every inline mutation normalises the containers it touched (§4).
- A result that is not writable is refused with `InvalidValue`, and so is an
  empty paragraph (delete it instead).
- Splitting a paragraph is an explicit structural command (follow-up).
  Ingest and text mutations never split a block.

### 10. Browser editing and suggesting

- In editorial output only, the server renders two attributes on every
  editable element (each body block and each slot element):
  - `data-presemble-md`: the element's canonical markdown (§7), a `Block`
    projection for body blocks and a `Content` projection for slot elements;
  - `data-presemble-address`: the NED selection that resolves to exactly that
    block. The server builds it (`content::block_addresses`); the browser
    never builds a selection. An element whose address would be ambiguous
    (duplicate heading text) has none and is not editable.
- The browser edits the markdown and sends a typed request,
  `POST /_presemble/block-edit`:

      BlockEdit {
        file,
        selection,                  // the element's data-presemble-address
        base:     BlockText,        // the element's data-presemble-md
        proposed: BlockText,        // the edited markdown
        intent:   Apply | Suggest,
        reason?
      }
      BlockText = Markdown(String)  // {"markdown": "..."} on the wire

  `BlockText` is a surface of the block, not the block. Markdown is the only
  one until another surface has a codec.
- The response is `Done` or `Refused`:

      Done(Applied { pages } | SuggestionCreated { id } | NoChange)
      Refused { cause: Drifted | Unaddressable | NotEditable | Invalid,
                message }

  A refusal is a normal response, not an error: the browser shows `message`
  and keeps the editor open with the user's text. An element without both
  attributes is not editable, and the browser says so; there is no fallback.
- The server first resolves the selection. It is query-only (ADR-056 §4) and
  must resolve to exactly one block.
- If `base` differs from that block's canonical markdown, the request is
  refused as `Drifted`.
- `proposed` must parse as blocks the kind allows. A payload kind that may not
  be written through a block edit (html-block, separator, link-expression) is
  refused as `Invalid`; the message says which.
- The server then lowers the request:
  - **No change:** `NoChange`.
  - **Only Text contents differ** (same element skeleton, same atoms): a
    container-scoped SearchReplace with a minimal unique diff on the edit
    text.
  - **Same block kind, anything else:** SetInlines.
  - **A structural change** (block kind, heading level, several blocks):
    Replace.
  - **Filling an empty slot** (stage 5b.3): the selection is a slot
    container, not a block, and its `base` is empty: `InsertChild` of the
    proposed blocks. Every grammar slot is a container in the store, so an
    empty slot is addressed like any other (`(ned/slot (ned/doc-by-path F)
    "name")`). In editorial output its placeholder carries that address as
    `data-presemble-address` and empty `data-presemble-md`. The proposal is
    read through the `Content` projection as the kind the schema gives the
    slot (a heading slot's level comes from the schema). A slot that has
    blocks is refused as `Drifted`: the client's empty base is out of date.
- Apply and Suggest share one lowering and produce the same mutation; they
  differ only in when it is applied. Apply applies it now; Suggest stores it
  until someone approves it. A structural change (block kind, heading level,
  several blocks) lowers to Replace in both cases. Payload kinds are the same
  for both.
- Every lowered suggestion stores a preview (the base and proposed markdown).
  Replace, SetText and SetInlines suggestions go Stale as `Drifted` when their
  block no longer matches the base. Rebasing suggestions onto later edits is
  the subject of a follow-up ADR (see "Follow-ups").
- The review UI shows any suggestion that has a preview as a change from
  `preview.base` to `preview.proposed`.
- Block kinds not yet converted are lowered to their current behaviour:
  SetText or SearchReplace on the single Text node.
- Removed from the browser:
  - `data-presemble-text`, `blockTextFromMd`, `_atxHeadingContent` and
    `blockShapeError`;
  - the client-side `minimalDiff` and `expandToUnique`;
  - the address builders (`buildStructuralSelection` and the `ned*`
    composers);
  - the `ned/set-text`, `ned/replace` and `ned/parse-body` programs built on
    the client, and the grammar fetch that fed them.
- The protocol is defined in `editorial_types` (serde). JSON fixtures in
  `resources/serve/protocol/` are checked by Rust and by
  `tools/protocol-fixtures.ts`, which runs the browser's request builder
  (`buildBlockEditRequest`, the `protocol-builders` region of `inject.js`)
  against them. Wave 3 (TypeScript) types those same fixtures; it does not
  change the protocol.

### 11. References

- The **declared references** of a document (ADR-058 §8) are every
  `inline-link` in every inline container (preamble slots, unassigned, body,
  blockquotes, list items, table cells) and every PathRef. They are classified
  by one function (today `expressions::page_reference`).
- Images never count as page references.
- A resolvable page reference adds an edge
  `Reference { name: "links-to" }` from the link node to the target document.
- Inline images are asset dependencies of their page:
  - when assets are nodes (ADR-029/034), the edge is
    `Reference { name: "asset" }`;
  - variants and cache-busting key on `inline-image` `src`.
- **Propagation.** An edit is body-only (ADR-058) when the preamble
  fingerprint *and* the declared-reference set are both unchanged. Otherwise,
  every page whose `refs-to` result changes is re-rendered. Inline links
  render their own text, so they add no render dependency on their target.
- **Publish gate (ADR-061).** Each of these is an Error, for inline and slot
  forms alike:
  - a page link whose target page does not exist;
  - a local image whose asset file is missing.

  The content is kept and saved; the page is unpublishable.

### 12. One paragraph shape; link and image slots by shape

- There are no `link` or `image` block kinds. A paragraph that holds only a
  link is a paragraph.
- Its shape is computed, never stored:
  `BlockShape::of(&Inlines) -> SoleLink(&Link) | SoleImage(&Image) | Text`.
  Inline comments and whitespace-only Text are ignored when classifying.
- **Slot classification:**
  - a schema `Element::Link` slot takes `SoleLink` paragraphs;
  - `Element::Image` takes `SoleImage` paragraphs;
  - `Element::Paragraph` takes `Text` paragraphs;
  - headings and lists match by kind.

  This is the same partition as before, so slot assignment and serialization
  are unchanged.
- An edit that changes a paragraph's shape changes no stored kind and is not
  structural. A slot holding a paragraph of the wrong shape reports an Error
  (for example, "slot latest: expected a link"). The content is still
  saveable, and it is reassigned on reload as described in ADR-057.
- A link in a slot that points to its own page is still skipped, as plain
  link blocks were (ADR-058).
- **Template records** are built from the single inline element:
  - a link slot gives `href` and `text` (nodes);
  - an image slot gives `src` and `alt` (plain text);
  - string contexts use `display_text`.
- Suggestion payloads use `paragraph`. The `link` and `image` payload kinds
  are removed.
- Done in stage 5b (5b.0 to 5b.3). The image record's field is `src`: the
  rename landed in 5b.0, in the same commit as the record, and every reader
  uses `src`; no template or reader maps `path` to it.

### 13. Blockquotes and lists are containers

- `blockquote` holds child blocks. Package Q (blockquotes store source) is
  dropped.
- `list` has `ordered`, `start` and `tight` attributes and `item` children,
  each holding blocks.
  - Canonical markers: `-` bullets, and `N.` counting from `start`.
  - Indentation is derived from the markers.
- Comments inside containers are trivia of the nested blocks (ADR-062 §7).
- Replace, Delete and Insert can target a block directly under a slot, body,
  unassigned, `blockquote` or `item`.
- Body numbering and slot assignment count top-level blocks only.

### 14. Raw HTML and comments inline

- `inline-html` holds verbatim HTML that is not comment-only.
  - It is an Error in every inline container (ADR-062 D3).
  - It renders as escaped text and is written verbatim.
  - It cannot be edited as text. To remove it, use SetInlines or a
    SearchReplace around it.
- `inline-comment` holds one comment's source.
  - It is never rendered, never part of plain or edit text, and never an
    Error. It is kept on save.
  - It appears in `data-presemble-md`. SearchReplace never removes it.
  - ADR-062 rejected sibling comment nodes at block level because child
    indices there carry meaning. Inline positions carry none.

### 15. Migration

- The store is rebuilt from files on every run, so no data migration is
  needed.
- Files on disk keep markdown, written canonically from the first save on
  (§0).
- Persisted suggestions that address converted kinds may be discarded.
  Stage 4 does this for headings: pending suggestions made before headings
  stored inline nodes, that touch a heading (a heading in the payload, or a
  selection that resolves into one), are discarded once at conductor startup,
  without an event or a Stale state. The marker file
  `.presemble/suggestions/ned/MODEL` (content `heading-inlines`) records that
  this ran, so suggestions made later are never discarded. Accepted, Rejected
  and Stale records stay. A build never writes to `.presemble`: only the
  conductor's `new` and `with_repo` migrate.
- The migration is an ordered list of steps, and `MODEL` holds the name of the
  last step applied:
  1. `heading-inlines` (stage 4): discards pending suggestions that touch a
     heading.
  2. `paragraph-inlines` (stage 5): discards every pending suggestion that
     touches a paragraph, whether or not its text has markup. A running
     conductor applies every step after the recorded one, in order, then
     writes the last step's name. An absent `MODEL` means no step has run. A
     name the conductor does not know was written by a newer one: nothing
     runs. A discarded suggestion is deleted, not quarantined.
  3. `link-image-paragraphs` (stage 5b): discards every pending suggestion whose
     payload holds a `link` or `image` element (the kinds are gone, so it could
     never apply). Suggestions with other payloads, and accepted, rejected and
     stale records, stay.
  4. `table-cells` (stage 6): discards every pending suggestion whose payload
     holds a `table` element (the old `headers` shape, with cells as text, is
     refused on accept). Suggestions with other payloads, and accepted,
     rejected and stale records, stay.

## Implementation plan

Every stage leaves the tree green and shippable.

- Each bug fix shows its red test run first.
- When a pin is flipped, its new expectation is shown failing first.
- New functions get their tests written, and seen failing, before the
  implementation.

**0. ADR and pins.**
- Write this ADR.
- Pin `table_alignment_is_lost_on_save_l23` and
  `unused_reference_definition_behaviour_is_pinned_l24`.
- Note in ADR-062 and ADR-063 that package Q is dropped.

**1. Inline core** (`content::inline`; no reader changes).
- Delivers:
  - `Inline` and `Inlines`, contexts, `LinkDestination`, `AssetRef`;
  - parsing from events, the canonical serializer, `normalise`;
  - `AsPlainText`, `EditText`, and `Inlines::search_replace`;
  - a deterministic generator in the style of `conservation.rs`.
- Tests:
  - `inline_round_trip_on_generated_writable_trees`
  - `inline_source_is_fixed_point_after_one_save`
  - `plain_text_of_inlines_matches_plain_vectors`
  - `normalise_is_idempotent`
  - `canonical_forms_c1_to_c10`
  - `search_replace_takes_formatting_of_match_start`
  - `search_replace_refuses_match_spanning_atom`
  - `inline_html_and_comment_are_kept_verbatim`

**2. Store, gate, render adapters.**
- Delivers:
  - `inlines_to_store` and `inlines_from_store`;
  - new `BlockDecodeError` variants;
  - `ned::normalise_inline_container`, with the commit order mutate,
    normalise, gate;
  - `template::inline_nodes`;
  - a block decoder that `template` can call. Where it lives is decided here;
    it replaces N3's transformer-local adapter.
- Tests:
  - `inlines_store_round_trip_on_generated_trees`
  - `decode_rejects_link_inside_link`
  - `decode_rejects_code_with_element_child`
  - `gate_normalises_adjacent_text_then_accepts`
  - `gate_rejects_unwritable_formatting`
  - `inline_nodes_match_markdown_nodes_on_inline_corpus`
  - `plain_text_equals_rendered_text_content`

**3. Typed block edits.** Independent of stages 1 and 2.
- Delivers:
  - `BlockEdit` and its conductor endpoint;
  - server-side lowering on the current model;
  - canonical `data-presemble-md`;
  - the browser cutover and the removals listed in §10;
  - JSON protocol fixtures.
- Tests:
  - `block_edit_lowers_word_change_to_unique_search_replace`
  - `block_edit_with_stale_base_is_drifted`
  - `block_edit_changing_block_kind_is_refused`
  - `protocol_fixtures_round_trip`
  - a smoketest of edit and suggest

**4. Headings.**
- Delivers:
  - `Heading { level, content, marker }`;
  - `MarkdownText`, `capitalize_first` and `heading_inline_events` removed;
  - all heading readers migrated;
  - heading slots as `Value::Nodes`;
  - SetText at any depth, container-scoped SearchReplace, and SetInlines.
- Tests:
  - `heading_stores_inline_nodes`
  - `heading_plain_text_vectors_unchanged`
  - `capitalize_fix_sets_first_text_node`
  - `heading_slot_renders_inline_nodes_in_content_position`
  - `input_title_is_plain_text_in_string_context`
  - `heading_marker_is_structural`
  - `set_text_payload_is_literal`
  - `search_replace_on_heading_spans_formatting`
  - `formatting_change_in_heading_lowers_to_set_inlines`

**5. Paragraphs.** Run the performance harness at 1K and 10K pages.
- Delivers:
  - images stay inline;
  - the leading-link loss is fixed;
  - markers become structure;
  - references in unassigned paragraphs and inline-HTML validation are
    computed from nodes;
  - paragraph slots become nodes.
- Tests:
  - flip `a_leading_link_followed_by_text_without_a_marker_is_unchanged`
  - `image_inside_text_stays_inline`
  - flip `store_paragraph_ending_in_a_marker_moves_to_unassigned_l11`
  - `escaped_paragraph_reloads_without_backslash`
  - `inline_html_in_image_bearing_paragraph_is_error_l15`
  - `unassigned_paragraph_with_image_counts_its_links`
  - `paragraph_slot_renders_inline_nodes`
  - flip `unused_reference_definition_behaviour_is_pinned_l24`: the used
    definition now keeps its link
  - extend the conservation domain with inline pools

**5b. One paragraph shape** (§12).
- Delivers:
  - `link` and `image` blocks removed;
  - `BlockShape`;
  - slot classification by shape;
  - template records from the single inline element;
  - payload kinds updated.
- Tests:
  - `sole_link_paragraph_fills_link_slot`
  - `sole_image_paragraph_fills_image_slot`
  - `link_slot_paragraph_with_added_text_is_error_not_structural`
  - `block_edit_adding_text_to_a_link_is_accepted`
  - `link_block_text_keeps_markup_l6`
  - `inline_html_in_link_text_is_error_l15`
  - `image_alt_keeps_markup`
  - slot assignment byte-identical on the conservation corpus

**6. Table cells.** Done (6.1 to 6.4, see the stage 6 amendments).
- Delivers: `cell` elements, stored alignment, `\|` escaped in cells, and
  addressable cells.
- Tests:
  - `table_cell_is_inline_container`
  - `table_cell_text_is_addressable`
  - `pipe_in_cell_code_span_is_escaped_on_write_l16`
  - flip `table_alignment_is_lost_on_save_l23` (now
    `table_alignment_survives_a_save_l23`)

**7. References** (§11).
- Tests:
  - `body_inline_link_counts_in_refs_to`
  - `body_link_edit_rerenders_pages_whose_refs_to_changes`
  - `inline_link_has_links_to_edge`
  - `inline_image_change_rebuilds_page`
  - `expression_destination_in_running_text_diagnosed_from_nodes`
  - `broken_page_link_is_error`
  - `missing_local_image_is_error`

**8. Blockquote container.**
- Tests:
  - `multi_paragraph_blockquote_round_trips`
  - `blockquote_code_span_keeps_backticks_l17`
  - flip `html_inside_a_blockquote_is_literal_text_l13`
  - `block_inside_blockquote_is_replaceable`
  - `comment_inside_blockquote_is_trivia`
  - nested blockquotes in the conservation domain

**9. List container.**
- Tests:
  - `list_items_are_block_containers`
  - `list_item_text_is_addressable`
  - `inline_link_in_list_item_counts_as_reference`
  - `ordered_start_and_tightness_round_trip`
  - lists in the conservation domain
- Decide here whether L10 is fixed with a `<!-- -->` separator.

**10. Cleanup and accept.**
- Remove:
  - `inline_children`, `table_cell_nodes`, and every remaining markdown parse
    in `template`;
  - `pulldown-cmark` from `template`, if nothing else uses it;
  - `TEXT_BLOCK_KINDS`, replaced by `INLINE_CONTAINER_KINDS`.
- Change this ADR's status to Accepted and update the status lines of the
  amended ADRs.
- Test: `template_does_not_parse_markdown`.

**Follow-ups:**
- the split-paragraph command;
- an Error for excess table cells on read (L16);
- suggestions follow their target: a pending suggestion is amended (rebased)
  when its target changes by a manual edit or an accepted suggestion, not only
  marked Stale.

## Alternatives considered

- **A segment interim** (`MarkdownText | ImageRef | LinkRef`). Every reader
  handles two forms, and SearchReplace needs a source offset map that is later
  thrown away. Moving one block kind at a time gives the same incrementality.
  Rejected.
- **Markdown source in every block** ("thin (ii)"). It keeps every second
  parse, and markers and escapes stay ambiguous. Rejected.
- **Recording source style** (`_` vs `*`, reference vs inline links) as
  attributes. It contradicts §0. Rejected.
- **Keeping `link` and `image` blocks.** Two stored kinds with one meaning:
  edits flip the kind and reloads change it, and every reader needs both
  cases. Rejected in favour of `BlockShape` (§12).
- **HTML names in the store** (`em`, `a`, `img`). It ties the content model to
  one serialization. Rejected (ADR-064 §2).
- **Folding inline HTML into Text.** A bare tag and an escaped one can no
  longer be told apart (L13 again). Rejected.
- **Inline comments as reserved trivia attributes.** Text nodes carry no
  attributes, and inline positions carry no meaning. Rejected.
- **The browser addressing Text nodes, or diffing edit text on the client.**
  It duplicates parser knowledge in JavaScript. Rejected in favour of lowering
  on the server.

## Consequences

- Readers stop parsing strings. Formatting can be addressed, and so can table
  cells.
- Body references count, so `refs-to` and the publish gate see every
  reference. Broken internal links and missing local images block publishing.
- `template` stops parsing markdown.
- Files are rewritten canonically on their first save (§0). That is intended
  behaviour, not a limit.
- The store holds more nodes, typically 1–6 per paragraph. Stage 5 measured it
  with the `full_build` bench (`just perf`, `just perf-ref <rev>`; medians):
  moving paragraphs into the store (5.2, against 5.1) cost +12 to 13% at 1K
  pages (296 to 335 ms) and +8 to 9% at 10K (3.94 to 4.29 s); the rendering
  stage (5.4, against 5.2) cost +1.9% at 1K and +0.1% at 10K, within noise.

### Limits resolved

- L6, L11, L13 and L17 (blockquotes), L15 (also for link text, stage 5b),
  L16 (write side, stage 6: `|` is escaped in cells, code spans included), and
  L18 (now structural);
- the kind change when a link is edited (stage 5b): adding text to a lone link
  is a content edit, not a `Replace`;
- L23 (stage 6: column alignment is stored and written), L24 (used
  definitions);
- a link or image inside a table cell dropped the whole table on read (stage
  6.1);
- ADR-058 §8 (body references), and the ADR-058 limit on image-bearing
  unassigned paragraphs;
- the leading-link loss;
- image extraction from paragraphs;
- the multi-paragraph blockquote merge;
- the escaped-paragraph mismatch;
- headings rendered as full markdown;
- link and image kind flips;
- ADR-056 "table cells not addressable";
- inline markup stripped by edit-mode slot saves.

### Limits remaining or introduced

- L5 and L7 remain: link-expression labels stay plain, and an href starting
  with `:` reads as a link expression.
- L10 remains until stage 9 decides it.
- L16 remains on the read side: GFM drops excess cells on load.
- L19 remains.
- L20 (new): formatting that markdown cannot express is refused, never
  silently changed.
- L22 (new): emptying a paragraph is refused; delete it instead.
- L21 is not assigned. Canonicalisation is intended behaviour (§0).
- L25 (new): a paragraph whose text is a link-expression lookalike
  (`[x](->> :post)`, with the brackets escaped) is diagnosed like the
  unescaped one, because the checks read what an author would have typed.
  Write it as a code span to show it literally.
- Unused link reference definitions are not written (C3).

## Amendments

- **ADR-056 §1, §3, §6:**
  - SetText targets any Text in an inline container, and its payload is
    literal text.
  - SearchReplace is container-scoped, with node-scoped kept.
  - SetInlines is added.
  - New stale causes: `Drifted` and `SpansAtom`.
  - Block targets include blocks nested in `blockquote` and `item`.
  - The `link` and `image` payload kinds become `paragraph`.
- **ADR-057:**
  - §4: inline containers are normalised inside the transaction before the
    gate. The gate decodes inlines recursively and rejects trees that break
    normal form or cannot be written.
  - OQ5 is resolved by §0 and §7.
  - OQ6 is resolved: content is structure, with literal Text.
- **ADR-058:**
  - §8: inline references count everywhere.
  - A body-only edit also requires an unchanged reference set.
  - §7 is diagnosed from nodes.
  - "Link block" reads as "`SoleLink` paragraph".
- **ADR-060:** markers on headings and paragraphs are structural attributes.
  Link and image slots match by `BlockShape`.
- **ADR-061:** broken internal page links and missing local images are
  Errors.
- **ADR-062:**
  - L13 and L17 are resolved for blockquotes.
  - L15 is resolved, and L16 is resolved on the write side.
  - Inline HTML and inline comments get node kinds (§14).
- **ADR-063:**
  - §1, §2 and §6 are superseded: headings store inlines, and `MarkdownText`
    and `capitalize_first` are removed.
  - The §3 rule and its contract are kept, now over nodes.
  - Package Q is dropped.
- **ADR-064:**
  - §3: content reaches `dom::Node` through `inline_nodes`, without parsing
    markdown.
  - §7: content inlines are a domain model that is mapped to `dom::Node`; it
    does not reuse those shapes.

### Stage 4 amendments (headings)

- **§9, SetInlines:** the payload is inline trees, `NedMutation::SetInlines(Vec<NodeTree>)`,
  not markdown.
- **§15, migration:** pending heading suggestions from before stage 4 are
  discarded once at conductor startup, marked by
  `.presemble/suggestions/ned/MODEL`. A build never writes to `.presemble`.
- **§10, slot elements:** a block under a slot is edited under the `Content`
  projection, so a markdown block edit cannot change a slot heading's level:
  the level belongs to the schema, and the same text is `NoChange`. An EDN edit
  is the whole block and can change the level (a `Replace`); content that then
  breaks the slot's level rule is reported, never refused.
- **§6/§11, rendering and references:**
  - Heading slots are `Value::Inlines`, distinct from `Value::Nodes`, which is
    block content (the body). An insert wraps `Inlines` in the element the
    template asks for, even when empty; in a string context they read their
    plain text.
  - A structural edit (`Replace`, `Delete`, `Insert*`) refreshes the page's
    semantic content in place: the semantic node keeps its id, its derived,
    reference and consists-of edges are recomputed from the store, and the
    pages that show it render again. Thread slots are re-resolved by the
    propagation that follows.
- **§8, capitalize fix:** the fix for `capitalized` is a SetText on the first
  `Text` node in document order, in `slot_editor` and, through the same
  transform, in the LSP code action. There is no fix when a code span or
  inline HTML comes before the first letter; comments are skipped.

### Stage 5 amendments (paragraphs)

- **§5, markers:** a trailing `{#name}` of a paragraph is the structural
  `marker`, and the content excludes it. A literal `{#x}` in text is written
  escaped. Marker misuse is diagnosed from the model, and the paragraph still
  goes to `unassigned`. Link expressions are recognised before a paragraph is
  read as inlines.
- **§6, store encoding:** a paragraph is its optional `marker` attribute and its
  inline nodes. A payload `paragraph` takes the `marker` attribute and literal
  text.
- **§9, refusals:** emptying a paragraph is refused (L22), and so is a blank
  line in any text edit of a paragraph, because paragraph text never holds
  `\n\n`. Both are `InvalidValue`.
- **§10, block edits:** a paragraph lowers like a heading: with the same marker,
  a container search-replace on the block selection when the diff touches no
  atom and reproduces the proposal, otherwise `SetInlines`; a marker change, a
  change of kind or several blocks is a `Replace`. A sole link or image is still
  a `Link` or `Image` block until stage 5b, so adding text after a link is a
  `Replace`.
- **§11, references:** references of an unassigned paragraph are computed from
  its nodes: a link in emphasis counts, an image never does.
- **§12, rendering:** body paragraphs render through `inline_nodes`. Paragraph
  slots are `Value::Inlines`, so a template that inserts one shows its
  formatting (it used to show the raw markdown) and in a string context it
  reads its plain text. `ContentElement::paragraph_source()` is removed;
  `inline_children` remains for blockquotes and lists (stages 8 and 9).
- **§15, migration:** see the step list there.

### Stage 5b amendments (one paragraph shape)

- **§12, done:** `ContentElement::Link` and `Image` and the store kinds `link`
  and `image` are gone. `BlockShape` (`content::shape`) classifies a paragraph
  from its inline nodes, and `content_store::paragraph_shape` does the same off
  the stored children without decoding. The partition of slots is unchanged, so
  slot assignment and serialization are too.
- **Payload kinds:** the payload kinds are `heading`, `paragraph`,
  `blockquote`, `list`, `code-block` and `table`. A `link` or `image` root is
  refused (`NotPayloadKind`); a link or image is
  `[:paragraph [:inline-link {:href ...} ...]]`.
- **§10, filling an empty slot:** see the bullet there. This replaces
  `Command::EditSlot`, the `/_presemble/edit` route and the slot-edit escape
  hatch (ADR-041 is superseded, ADR-047, ADR-057 gap 7 is closed). The link
  picker sends a block edit with `[text](href)` as the proposal.
- **Stage 5 amendment, §10:** "a sole link or image is still a `Link` or
  `Image` block until stage 5b" no longer holds.
- **§15, migration:** the `link-image-paragraphs` step.

### Stage 6 amendments (table cells)

- **6.1, parser fix (`4e8ea6f`):** a link or image started a block whatever it
  was inside, so one in a table cell ended the table: the table vanished from
  the parsed document, a stray link paragraph took its place, and a save wrote
  the file without it. A link or image now starts a block only at the top
  level, where a block can start; anywhere else it is inline content of what is
  being read (paragraph, heading, table cell, list item, blockquote).
- **Model:** `ContentElement::Table { columns, rows }`. A `Column` is a header
  (`Inlines` in the `Cell` context) and an `Align` (`Default`, `Left`,
  `Center`, `Right`). Every row has exactly one cell per column; the parser pads
  short rows with empty cells.
- **§6, store encoding:** `table > head > cell*` and
  `table > rows > row > cell*`. Every `cell` is an inline container in the
  `Cell` context (it is in `INLINE_CONTAINER_KINDS`). A head cell carries
  `align` (`left`, `center` or `right`); the default has no attribute, and no
  other element takes `align`. Decoding is strict: a child that is not the
  expected element, the old `headers` shape or text directly under a row is
  `UnexpectedChild`; `align` on a body cell is `UnexpectedAttribute`; an
  invalid `align` value is `InvalidAttribute`; a row whose width differs from
  the head is `RowWidth { expected, found }`. A table payload takes the same
  shape.
- **Canonical markdown (§7):** the delimiter row is written `---`, `:---`,
  `:---:` or `---:`. Cells are canonical inlines, and `|` is escaped as `\|`
  everywhere in a cell, code spans included.
- **Rendering:** cells render through `inline_nodes`; no cell markdown is parsed
  (`table_cell_nodes` is removed). A column that declares an alignment renders
  `style="text-align: left|center|right"` on its `<th>` and `<td>` cells (a user
  decision); the default renders no attribute.
- **Addressing (§9, §10):** `(ned/header-cell table c)` and
  `(ned/cell table r c)`, zero-based, select a cell. A cell is an inline
  container, so `TextScope`, `SetText`, `SearchReplace` and `SetInlines` apply to
  it unchanged. A cell's text edit is scoped to the cell, and its document is the
  table's.
- **Validation:** inline HTML in a table cell is an Error, diagnosed from the
  nodes.
- **§15, migration:** the `table-cells` step.
- **Limits:** L23 is resolved. L16 is resolved on the write side; on the read
  side GFM still drops cells beyond the header's count.

## Related

ADR-029, ADR-033, ADR-034, ADR-045, ADR-046, ADR-056, ADR-057, ADR-058,
ADR-060, ADR-061, ADR-062, ADR-063, ADR-064.
