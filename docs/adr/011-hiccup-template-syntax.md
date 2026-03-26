# ADR-011: Hiccup Template Syntax

## Status

Proposed

## Context

ADR-005 established that templates are data structures — trees of typed nodes — and that the
surface syntax used to write a template is a parser choice, not a model choice. It noted EDN/Hiccup
as a first-class example syntax alongside HTML and YAML, but left all non-HTML syntaxes
unimplemented.

The current codebase parses only `.html` template files (via `quick-xml`). HTML is a good default
because it is familiar to designers, but if it remains the only option it risks drifting from "a
way" into "the way." ADR-005's core insight — that internal model and surface syntax are orthogonal
— is only as credible as the first second syntax.

Hiccup is a Clojure convention for representing HTML trees as nested vectors:

```clojure
[:article {:class "post"}
  [:h1 "Title"]
  [:p "Body text"]]
```

Tag names are EDN keywords. Attributes are an optional EDN map as the second element. Children are
strings, `nil`, or nested vectors. The format is compact, unambiguous, and already structured data —
no XML parser is needed, and there are no void-element rules to handle.

Presemble elements in HTML use a namespace prefix: `<presemble:insert>`. The natural Hiccup
equivalent is an EDN namespaced keyword: `[:presemble/insert {:data "article.title" :as "h1"}]`.
The `/` namespace separator in EDN maps directly to `:` in the HTML element name.

## Decision

Add `.hiccup` as a second template surface syntax.

A hand-written minimal EDN parser (~120 lines, no new dependency) converts Hiccup templates into
the existing `Node`/`Element` DOM tree used by all downstream pipeline stages. The transformer,
serializer, asset extractor, and all other pipeline components are unchanged — they operate on the
internal model and never see the surface syntax.

### Hiccup format

A Hiccup template is a single top-level EDN vector:

```clojure
[:article
  [:presemble/insert {:data "article.title" :as "h1"}]
  [:presemble/insert {:data "article.cover"}]
  [:section {:class "body"}
    [:presemble/insert {:data "article.body"}]]]
```

**Tag names** are EDN keywords. Simple keywords (`:div`, `:h1`, `:article`) map to HTML elements of
the same name. Namespaced keywords (`:presemble/insert`, `:presemble/each`) map to the
`presemble:`-prefixed element names used by the transformer vocabulary (`:` replaces `/`).

**Attributes** are an optional EDN map as the second vector element, immediately after the tag
keyword. Map keys are EDN keywords; values are strings. If the second element is not a map (or the
vector has only one element), there are no attributes.

**Children** follow the attribute map (or the tag keyword if there is no map). Each child is one
of:
- A string — becomes a text node
- A vector — becomes a child element (recursively parsed)
- The bare symbol `nil` — silently skipped

**Whitespace** between elements is not implied. If whitespace between sibling elements is needed,
an explicit string child (`" "`) must be included. This is an established Hiccup convention and
matches the "no invisible content" principle.

### Parser scope

The hand-written parser handles the subset of EDN needed for Hiccup templates:

- Vectors `[...]`
- Maps `{...}`
- Keywords `:name` and `:namespace/name`
- Double-quoted strings (with `\"` and `\\` escapes)
- The bare symbol `nil`
- Line comments starting with `;`

Full EDN (tagged literals, sets, lists, symbols other than `nil`, arbitrary numbers, characters,
instants, UUIDs) is outside scope and will produce a parse error with a descriptive message. This
scope boundary is intentional — templates have no need for the full EDN data model.

### CLI template resolution

The publisher CLI resolves templates by trying `.html` first, then `.hiccup`. If neither is found,
the existing "template not found" error is returned. Projects may use both syntaxes freely — the
shared contract is the internal `Node`/`Element` model and the data graph vocabulary.

### Mapping to the internal model

| Hiccup construct | Internal model |
|---|---|
| `[:div {:class "x"} ...]` | `Element { tag: "div", attrs: [("class","x")], children: [...] }` |
| `[:presemble/insert {:data "..."}]` | `Element { tag: "presemble:insert", attrs: [...], ... }` |
| `"text"` | `Node::Text("text")` |
| `nil` | (dropped) |

## Alternatives considered

**Full EDN crate** — a complete EDN parser library would handle the surface syntax correctly and
support the full data model. Rejected for two reasons: (1) the subset needed for Hiccup templates
is small enough to parse in ~120 lines without a dependency, and (2) existing Rust EDN crates use
their own namespace prefix representation that does not map cleanly to the `presemble:` element
names used by the internal model, requiring an additional translation layer with no net benefit.

**S-expressions / Lissp / other Lisp syntaxes** — similar expressive power to Hiccup but less
ergonomic for attribute maps (attributes become positional or use a special keyword convention).
Hiccup's `{:key "value"}` map is more readable than `(:key "value" :key "value")` for the
attribute case. No advantage over Hiccup.

**YAML** — considered in ADR-005. Requires explicit `text:` tagging for mixed content (elements
and text siblings in the same parent) because YAML sequences cannot hold both mappings and scalars
naturally without disambiguation. This makes YAML unnatural for DOM trees with inline text.

**JSON** — same mixed-content problem as YAML. Also more verbose than Hiccup for deeply nested
trees.

**KDL** — explicitly does not support mixed content (element and text children at the same level).
Unsuitable for a DOM-tree surface syntax where any element may contain both text and child
elements.

**TOML** — not a tree format. Not considered.

## Consequences

**Positive:**
- Proves ADR-005's "surface syntax is a parser choice" claim with a real second implementation
- Hiccup is compact and already data — no closing tags, no void-element rules, no XML escaping
- EDN namespaced keywords (`:presemble/insert`) map cleanly to the `presemble:` HTML namespace
  used throughout the internal model
- No new runtime dependency
- All downstream pipeline components (transformer, serializer, asset extractor) are unmodified
- Projects and individual developers may mix `.html` and `.hiccup` templates freely

**Negative / open questions:**
- Whitespace between sibling elements requires explicit string nodes — a deliberate Hiccup
  convention but potentially surprising to authors coming from HTML
- The hand-written parser covers only the Hiccup-relevant EDN subset; any EDN outside that subset
  produces a parse error. This is intentional but means `.hiccup` files that contain full EDN
  (e.g. tagged literals) are rejected
- Error messages from the minimal parser must be clear enough to guide template authors — the
  quality of parse error reporting needs validation
- Attribute value types: all attribute values are currently strings. EDN keyword values
  (`:h1` rather than `"h1"`) in attribute maps need a resolution: either coerce to string, or
  treat the bare keyword name as the string value. This must be decided before the parser is
  finalised
- The `.hiccup` extension is not registered with any MIME type or editor ecosystem — syntax
  highlighting in editors will require a manual Clojure/EDN association

## Experiment scope

1. Implement `hiccup.rs` — the minimal EDN parser and Hiccup-to-`Node` converter
2. Wire the publisher CLI to try `.hiccup` after `.html` during template resolution
3. Rewrite one existing test template in Hiccup and confirm the rendered output is identical to
   the HTML template output
4. Confirm the transformer, serializer, and asset extractor tests pass without modification
5. Evaluate: are parse error messages good enough to guide a template author to fix a mistake?
