# ADR-005: Templates Are Data

## Status

Accepted

## Context

ADR-004 proposed an HTML template language with `{{ }}` pipe expressions. The experiment validated
the pipe model but used HTML as string text with expression holes — the same fundamental model as
Jinja/Handlebars. The publisher treated templates as strings and substituted expression results as
text.

A deeper insight emerged from exploring HTML `<slot>` and `<template>` elements applied at build
time: **the right model is DOM transformation, not string interpolation.** Templates are data
structures representing HTML trees, not text files with holes.

Jinja/Handlebars templates cannot guarantee well-formed HTML output — a missing closing tag in a
loop produces broken HTML with no warning. A template that is parsed as an HTML tree first makes
structural invalidity impossible by construction.

Additionally, Presemble's overall architecture is already data-oriented: schemas are parsed into
Grammar data structures, content is parsed into Document data structures. The publisher is a
pipeline of pure data transformations. Templates should participate in this model — not remain as
text strings in an otherwise data-driven pipeline.

The observation that templates could be written in EDN, YAML, or HTML(ish) equally reveals what
matters: not the surface syntax, but the internal model — a tree of typed nodes with links to the
data graph.

## Decision

Templates in Presemble are data structures representing HTML DOM trees, not text files with
expression holes.

### The internal model

A template is a tree of nodes. Each node is either:

- A **literal element**: a standard HTML tag with attributes and children
- A **presemble annotation**: a Presemble-specific node describing a transformation — an insert
  point, an iteration, an attribute binding, etc.

The publisher parses a template into this tree, applies data graph transformations node by node,
and serializes the resulting tree to HTML. String manipulation occurs only at the final
serialization step.

### Structural validity guarantee

Because templates are parsed as HTML trees before any data is applied, the publisher can statically
verify that the template will produce well-formed output. Mismatched tags, invalid nesting, and
broken structure are caught at parse time — before any content is processed. This is impossible
with string-interpolation template engines.

### Presemble annotation vocabulary (to be specified in detail)

A working vocabulary emerging from experiment:

```html
<!-- Insert a slot value as a DOM node -->
<presemble:insert data="article.title" as="h1"></presemble:insert>

<!-- Attribute binding from the data graph -->
<div presemble:class="article.cover.orientation | match(landscape: cover--landscape, portrait: cover--portrait)">

<!-- Iteration -->
<template data-each="site.articles">
  <presemble:insert data="article.title" as="h3"></presemble:insert>
</template>

<!-- Conditional (insert only if value is present) -->
<template data-slot="article.cover">
  <presemble:insert data="article.cover"></presemble:insert>
</template>
```

**Slot resolution and element types:**
- Text-like slots (headings, paragraphs): publisher produces a text node or inline element.
  Template can override the wrapping element with `as="h1"`, `as="p"`, etc. Default is `<span>`
  with a schema-derived semantic class.
- Element-type slots (images, links): publisher produces the typed node (`<img>`, `<a>`) with its
  attributes from the data graph. The `as` override is not meaningful for these.

**Schema-derived semantic classes:**
The publisher automatically assigns a semantic class to each inserted element, derived from the
content type and slot name (e.g. `article-title`, `article-cover`, `article-author`). This gives
CSS authors a stable, schema-derived vocabulary to write styles against — describing what an
element *is*, not how it should look. Presentational and layout classes remain the template
author's responsibility.

### Multiple surface syntaxes

Because the internal model is a tree, the surface syntax used to write a template is a parser
choice, not a model choice. The following surface syntaxes all produce the same internal tree:

HTML(ish) — natural for designers and visual correspondence:

```html
<article>
  <presemble:insert data="article.title" as="h1"></presemble:insert>
  <presemble:insert data="article.cover"></presemble:insert>
</article>
```

EDN/Hiccup — natural for developers, concise, already data:

```clojure
[:article
  [presemble/insert {:data "article.title" :as :h1}]
  [presemble/insert {:data "article.cover"}]]
```

YAML — readable structured data:

```yaml
tag: article
children:
  - presemble: insert
    data: article.title
    as: h1
  - presemble: insert
    data: article.cover
```

**The choice of surface syntax is the developer's.** Projects may mix syntaxes — one developer
writes HTML templates, another writes EDN templates — because the shared contract is the internal
model and the data graph vocabulary, not the file format. Adding a new surface syntax requires only
a new parser that emits the internal tree; the publisher, transformer, and serializer are
untouched.

## Alternatives considered

**String interpolation (ADR-004 `{{ }}` model)** — pipe expressions embedded in HTML strings.
Validated by experiment as ergonomic and covering common cases. Rejected as the primary model
because: (1) cannot guarantee well-formed output, (2) treats templates as text rather than data,
inconsistent with the rest of the pipeline. The pipe expression vocabulary (`each`, `maybe`,
`match`, `default`, `first`, `rest`) is preserved as the data graph query language embedded within
the DOM transformation model.

**XSLT** — the canonical XML tree transformation language. Powerful but syntactically heavy and
unfamiliar to most developers. The Presemble model is inspired by the same idea (tree
transformation) but with a simpler, more targeted vocabulary.

**JSP tags** — imperative XML-encoded control flow (`<c:forEach>`, `<c:if>`). Rejected: Presemble
annotations are declarative structural descriptions, not imperative code expressed as tags.

## Consequences

**Positive:**
- Well-formed HTML output is guaranteed by construction
- Templates participate in the data-oriented pipeline — no string manipulation except final
  serialization
- Surface syntax is a parser concern, not a model concern — multiple syntaxes supported naturally
- Schema-derived semantic classes give CSS authors a stable vocabulary tied to content structure
- The publisher can validate templates statically before processing any content
- Familiar to developers who think in terms of DOM manipulation

**Negative / open questions:**
- Requires an HTML tree parser (not just an event stream) — `html5ever` / `markup5ever_rcdom` or
  similar
- The full presemble annotation vocabulary needs formal specification and experiment
- The `as` override on `<presemble:insert>` needs validation: which element type overrides are
  meaningful, which are not
- EDN/YAML surface syntaxes are not yet implemented — start with HTML, design the internal model
  for extensibility
- Semantic class generation from slot names needs a naming convention (e.g.
  `{content-type}-{slot-name}`)
- Templates are structured data, not HTML or XHTML. The current implementation uses XML as the surface syntax (via quick-xml) because it is familiar to designers and developers. XML supports self-closing syntax for any element (`<presemble:insert />`), so there is no "void element" restriction. The same internal DOM model could be expressed in EDN, YAML, JSON, or any format that can represent a tree — XML is an implementation choice, not a constraint.

## Experiment scope

1. Re-implement the template component using an HTML tree parser instead of the current
   string-splitting approach
2. Write the article template using the `<presemble:insert>` / `presemble:attr` vocabulary
3. Validate that the same content renders correctly
4. Confirm that the template parser rejects structurally invalid HTML at parse time
5. Assess whether the vocabulary covers the cases validated in ADR-004's experiment

---

## Addendum — 2026-03-27

Collections moved to the root-level namespace. The `data-each="site.articles"` example in the
annotation vocabulary section of this ADR is superseded. The correct form is
`data-each="articles"` — collection names are looked up directly from the data graph root,
not under a `site` prefix.
