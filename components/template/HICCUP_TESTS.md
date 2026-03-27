# Hiccup Parser — Test Plan

This document specifies the test cases for `components/template/src/hiccup.rs`.

The parser converts EDN/Hiccup source text into the internal `Vec<Node>` tree defined in
`dom.rs`. The same `Node` and `Element` types used by the XML parser are the output types here.
Errors are returned as `TemplateError::ParseError(String)`.

Once `hiccup.rs` compiles, the implementation minion should add a `#[cfg(test)]` block inside
`hiccup.rs` (or a sibling `hiccup_tests.rs` included with `#[cfg(test)] mod hiccup_tests;`)
containing the cases below.

---

## Happy-path cases

### 1. Simple nested element

Input:

```edn
[:div [:p "Hello"]]
```

Expected output: one top-level `Node::Element` with:
- `name` = `"div"`
- `attrs` = `[]`
- `children` = one `Node::Element` with:
  - `name` = `"p"`
  - `children` = `[Node::Text("Hello")]`

```rust
#[test]
fn simple_nested_element() {
    let nodes = parse_hiccup(r#"[:div [:p "Hello"]]"#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Element(ref div) = nodes[0] else { panic!("expected element") };
    assert_eq!(div.name, "div");
    assert_eq!(div.attrs, vec![]);
    assert_eq!(div.children.len(), 1);
    let Node::Element(ref p) = div.children[0] else { panic!("expected element") };
    assert_eq!(p.name, "p");
    assert_eq!(p.children.len(), 1);
    let Node::Text(ref t) = p.children[0] else { panic!("expected text") };
    assert_eq!(t, "Hello");
}
```

---

### 2. Element with attribute map

Input:

```edn
[:meta {:charset "utf-8"}]
```

Expected output: one `Node::Element` with:
- `name` = `"meta"`
- `attrs` = `[("charset", "utf-8")]`
- `children` = `[]`

```rust
#[test]
fn element_with_attrs() {
    let nodes = parse_hiccup(r#"[:meta {:charset "utf-8"}]"#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Element(ref el) = nodes[0] else { panic!() };
    assert_eq!(el.name, "meta");
    assert_eq!(el.attrs, vec![("charset".to_string(), "utf-8".to_string())]);
    assert!(el.children.is_empty());
}
```

---

### 3. Presemble namespace — slash maps to colon

Input:

```edn
[:presemble/insert {:data "article.title" :as "h1"}]
```

Expected output: `Node::Element` with:
- `name` = `"presemble:insert"` (the EDN namespace separator `/` maps to XML namespace `:`)
- `attrs` = `[("data", "article.title"), ("as", "h1")]`
- `children` = `[]`
- `el.is_presemble()` returns `true`

```rust
#[test]
fn presemble_namespace() {
    let nodes = parse_hiccup(r#"[:presemble/insert {:data "article.title" :as "h1"}]"#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Element(ref el) = nodes[0] else { panic!() };
    assert_eq!(el.name, "presemble:insert");
    assert!(el.is_presemble());
    assert_eq!(el.attr("data"), Some("article.title"));
    assert_eq!(el.attr("as"), Some("h1"));
}
```

---

### 4. data-each iteration

Input:

```edn
[:template {:data-each "features"} [:li "item"]]
```

Expected output: `Node::Element` with:
- `name` = `"template"`
- `attrs` = `[("data-each", "features")]`
- `children` = one `Node::Element` with `name` = `"li"` and text child `"item"`

```rust
#[test]
fn data_each_attribute() {
    let nodes = parse_hiccup(r#"[:template {:data-each "features"} [:li "item"]]"#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Element(ref tmpl) = nodes[0] else { panic!() };
    assert_eq!(tmpl.name, "template");
    assert_eq!(tmpl.attr("data-each"), Some("features"));
    assert_eq!(tmpl.children.len(), 1);
}
```

---

### 5. Multi-root document

Input:

```edn
[:h1 "Title"] [:p "Body"]
```

Expected output: two top-level nodes, both `Node::Element`.

```rust
#[test]
fn multi_root() {
    let nodes = parse_hiccup(r#"[:h1 "Title"] [:p "Body"]"#).unwrap();
    assert_eq!(nodes.len(), 2);
    let Node::Element(ref h1) = nodes[0] else { panic!() };
    let Node::Element(ref p) = nodes[1] else { panic!() };
    assert_eq!(h1.name, "h1");
    assert_eq!(p.name, "p");
}
```

---

### 6. Top-level text node

Input:

```edn
"Just text"
```

Expected output: `[Node::Text("Just text")]`

```rust
#[test]
fn top_level_text_node() {
    let nodes = parse_hiccup(r#""Just text""#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Text(ref t) = nodes[0] else { panic!() };
    assert_eq!(t, "Just text");
}
```

---

### 7. nil children are skipped

Input:

```edn
[:div nil "hello" nil]
```

Expected output: `Node::Element` with `name` = `"div"` and exactly one child: `Node::Text("hello")`.

```rust
#[test]
fn nil_children_skipped() {
    let nodes = parse_hiccup(r#"[:div nil "hello" nil]"#).unwrap();
    assert_eq!(nodes.len(), 1);
    let Node::Element(ref div) = nodes[0] else { panic!() };
    assert_eq!(div.children.len(), 1);
    let Node::Text(ref t) = div.children[0] else { panic!() };
    assert_eq!(t, "hello");
}
```

---

### 8. Full nested template — non-empty parse

Input: a representative multi-element template (e.g. an article layout):

```edn
[:article
  [:presemble/insert {:data "article.title" :as "h1"}]
  [:presemble/insert {:data "article.cover"}]
  [:section {:class "body"}
    [:presemble/insert {:data "article.body"}]]]
```

Expected: `parse_hiccup(src)` returns `Ok(nodes)` where `nodes` is non-empty and the first
element is named `"article"`.

```rust
#[test]
fn full_template_parses_non_empty() {
    let src = r#"
    [:article
      [:presemble/insert {:data "article.title" :as "h1"}]
      [:presemble/insert {:data "article.cover"}]
      [:section {:class "body"}
        [:presemble/insert {:data "article.body"}]]]
    "#;
    let nodes = parse_hiccup(src).unwrap();
    assert!(!nodes.is_empty());
    let Node::Element(ref article) = nodes[0] else { panic!() };
    assert_eq!(article.name, "article");
}
```

---

### 9. presemble:class attribute — DESIGN QUESTION (see below)

This test is **blocked** until the design question in section "Open design question" is resolved.
The placeholder below documents the intended test once the convention is decided.

Tentative input (preferred convention):

```edn
[:div {:presemble/class "feature.title | match(active => \"active\")"}]
```

Tentative expected output: `Node::Element` with:
- `name` = `"div"`
- `attrs` = `[("presemble:class", "feature.title | match(active => \"active\")")]`

That is, the EDN namespace separator `/` in an attribute keyword (`:presemble/class`) maps to `:`,
producing the HTML attribute name `"presemble:class"` — the same mapping used for element names.

```rust
// Blocked — see "Open design question" section below.
// #[test]
// fn presemble_class_attribute() { ... }
```

---

## Error cases

### 10. Non-keyword tag

Input:

```edn
["div"]
```

Expected: `Err(TemplateError::ParseError(_))` — the first element of a Hiccup vector must be an
EDN keyword (`:tag-name`), not a string.

```rust
#[test]
fn error_non_keyword_tag() {
    let result = parse_hiccup(r#"["div"]"#);
    assert!(result.is_err(), "expected parse error for string tag");
}
```

---

### 11. Unclosed bracket

Input:

```edn
[:div
```

Expected: `Err(TemplateError::ParseError(_))` — the input ends before the vector is closed.

```rust
#[test]
fn error_unclosed_bracket() {
    let result = parse_hiccup("[:div");
    assert!(result.is_err(), "expected parse error for unclosed bracket");
}
```

---

### 12. Attribute map with odd number of forms

Input:

```edn
[:div {:key}]
```

Expected: `Err(TemplateError::ParseError(_))` — a map literal with an odd number of forms is
invalid EDN.

```rust
#[test]
fn error_map_odd_elements() {
    let result = parse_hiccup(r#"[:div {:key}]"#);
    assert!(result.is_err(), "expected parse error for odd-element map");
}
```

---

## Open design question: how to write `presemble:class` in Hiccup

This is the most important unresolved question for the Hiccup surface syntax.

### The problem

In XML/HTML templates, the `presemble:class` attribute uses a colon as a namespace separator:

```html
<div presemble:class="article.cover.orientation | match(...)">
```

In EDN, the colon is the namespace separator for *namespaced keywords* — but it separates a
namespace prefix from a local name. EDN keywords have the form `:namespace/local-name`.
A literal colon inside the local-name part of a keyword is non-standard and not reliably
parseable by EDN libraries.

### Options

**Option A — preferred: `{:presemble/class "..."}`**

Use the EDN namespace separator `/`. The parser maps `/` to `:` when constructing attribute names,
exactly as it does for element tag names (`:presemble/insert` → `"presemble:insert"`).

```edn
[:div {:presemble/class "article.cover.orientation | match(...)"}]
```

Produces:

```
attrs: [("presemble:class", "article.cover.orientation | match(...)")]
```

This is valid EDN. It is consistent with how element namespaces are expressed. It requires the
same `/` → `:` mapping rule to apply uniformly to both tag keywords and attribute keywords.

**Option B — string keys: `{"presemble:class" "..."}`**

Use string map keys instead of keywords:

```edn
[:div {"presemble:class" "article.cover.orientation | match(...)"}]
```

This is valid EDN but unusual. EDN map keys are idiomatically keywords, not strings. Mixing
keyword keys (`:data`, `:class`) with string keys in the same map would be jarring:

```edn
[:div {:class "body" "presemble:class" "..."}]  ; awkward mixed key types
```

**Option C — flat convention: `:presemble-class`**

Use a hyphen instead of a colon within the attribute name, mapping `:presemble-class` to
`"presemble-class"` in HTML. This avoids the namespace question entirely but diverges from the
existing HTML attribute name `presemble:class`.

This would require renaming the attribute in the HTML syntax as well for consistency, which is a
larger change affecting both parsers and the transformer.

### Recommendation

**Option A** (`{:presemble/class "..."}` → attr `"presemble:class"`) is recommended because:

1. It is valid EDN.
2. It applies the same `/` → `:` mapping rule already needed for element names — no new rule.
3. Attribute and element name handling are symmetric.
4. The Hiccup author's mental model is simple: "namespace separators are `/` in Hiccup, `:` in HTML."

**Action for architect:** confirm Option A, then unblock test case 9.

---

## Parsing conventions to implement

These rules must be applied uniformly by `parse_hiccup`:

| EDN form | Maps to |
|---|---|
| `[:tag/name ...]` | `Element { name: "tag:name", ... }` |
| `[:tag/name {:ns/attr "v"} ...]` | `attrs: [("ns:attr", "v")]` |
| `[:tag/name {:plain-attr "v"} ...]` | `attrs: [("plain-attr", "v")]` |
| `[:tag/name "text"]` | `children: [Node::Text("text")]` |
| `[:tag/name nil ...]` | `nil` children dropped silently |
| `"text"` at top level | `Node::Text("text")` |
| Multiple forms at top level | Multiple entries in `Vec<Node>` |

The EDN namespace separator `/` maps to `:` in both tag names and attribute names.
