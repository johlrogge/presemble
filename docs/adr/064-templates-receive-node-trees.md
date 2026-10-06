# ADR-064: Templates receive node trees; HTML is serialization only

## Status

Proposed

## Context

- Presemble's model is data: content and templates are both node graphs
  (ADR-005), and the build runs as one pipeline over them (ADR-037).
- Formatted content still reaches templates as HTML strings in several places:
  - **V1:** `render_body_html` in `template/src/data.rs` produces
    `Value::Html("body")`, which the transformer parses back into nodes.
  - **V2:** `render_inline_markdown` goes through
    `markdown_html::safe_inline_html`, which returns an HTML string.
  - **V3:** the native path's body arms render markdown to HTML
    (`render_inline_md`) and then parse it (`parse_html_fragment`).
  - **V4:** table cells are rendered with `safe_table_cell_html`.
  - **V5:** the ADR-062 placeholder and the hidden separator are built as
    strings in `data.rs`.
  - **V6:** `Value::Html` in string contexts: `eval_expr_to_string` emits raw
    markup, and `display_text` leaves entities undecoded (`a &amp; b`).
  - **V7:** `Value::Html` crosses into `node_store_bridge/value_bridge.rs`,
    `edn` and `evaluator/primitives.rs`.
- HTML strings in the data path defeat structural editing, type checking and
  safety. The HTML-injection bugs that ADR-062 fixed were possible because of
  them.
- Stylesheets are expected to become data too (Garden-style EDN), so the node
  model has to fit more than HTML.

## Decision

### 1. Principle

Templates receive node trees. HTML and CSS are serializations only. Templates
and stylesheets never handle HTML or CSS strings.

### 2. One data model, typed views

- The generic layer is EDN data (`Form`).
- `dom::Node` is the typed hiccup view: what templates receive for markup.
- A future `css::Rule` / `Stylesheet` is the typed Garden view.
- These are not collapsed into an untyped `Value::Data(Form)`. Typed views keep
  illegal states out: a CSS rule cannot become an element child.

### 3. Markdown to nodes

- Markdown becomes `dom::Node` only through `template::markdown_nodes`, which
  builds nodes directly from pulldown-cmark events.
- ADR-062 neutralisation is an event filter applied before node building:
  - comment-only HTML is dropped;
  - any other HTML becomes a `Text` node.
- `events_to_nodes` neutralises HTML itself; there is no path that builds
  nodes from un-neutralised events.

### 4. Nodes carry no serialization concerns

`dom::Node` knows nothing about HTML text. Escaping, void elements (`<br>` vs
`<br />`) and raw strings belong to the HTML serializer.

URL-valued attributes built from markdown (`href`, `src`) hold
percent-encoded URLs; HTML escaping of attribute values belongs to the
serializer.

### 5. Attribute values stay `Form`

Attribute values are `Form`, not narrowed to strings.

### 6. `Value::Nodes` replaces `Value::Html`

- `Value::Html` is replaced by `Value::Nodes`.
- The `display_text` of a node value is its normalised text content.
- `Value::Nodes` maps losslessly to and from hiccup `Form`; EDN output of nodes
  is hiccup.

### 7. Inline segments

ADR-059 (inline segments) reuses the same node shapes.

## Alternatives considered

- **Keep HTML strings and sanitise harder.** Every new consumer would have to
  parse or escape correctly; the ADR-062 bugs show how that fails. Rejected.
- **One untyped `Value::Data(Form)`.** Generic, but lets a CSS rule appear
  where an element child is expected. Rejected in favour of typed views over
  `Form`.

## Consequences

- No HTML string reaches a template value. HTML is produced only by the final
  serializer.
- Text content of formatted values is exact: entities are characters, not
  `&amp;`.
- Package N delivers this in steps: N1 adds `markdown_nodes` alongside the
  current path; later steps switch readers over and remove `markdown_html`
  and `Value::Html`.

## Follow-ups (out of scope)

- Stylesheets as Garden data, with a CSS serializer.
- Attribute roles: editorial-only vs. possible style hook. Once stylesheets
  are data, strip only the attributes no stylesheet references. There is no
  strip-all.

## Amendments

- **ADR-005 (templates are data):** what templates receive is data too: node
  trees, never HTML strings.
- **ADR-062 §3–4:** the policy is unchanged. The mechanism changes: HTML is
  neutralised as an event filter before nodes are built, instead of by
  rendering escaped HTML with `push_html`.

## Related

ADR-005, ADR-037, ADR-059, ADR-062, ADR-063.
