# ADR-001: Markdown-based schema format

## Status

Accepted

## Context

Presemble needs a schema format to describe content types. The schema serves three roles
simultaneously:

1. **Validator** — the publisher uses it as a hard gate at build time
2. **Author prompt** — the content editor shows it to guide authors on what to write
3. **Documentation** — a non-technical author can read it and understand what a content type expects

Standard schema formats (JSON Schema, TOML, YAML) serve role 1 well but poorly serve roles 2 and 3.
A format that reads like the content it describes would serve all three roles at once.

Additionally, Presemble's content model goes beyond frontmatter key-value pairs. A schema must
express:

- A **document grammar** — the expected sequence of structural elements (heading, image, paragraph,
  body), not just a bag of fields
- **Named slots** — each structural position has a name that becomes a queryable field in templates
  and content references
- **Constraints** on elements — heading level ranges, image orientation, text length, etc.
- **Generation hints** — instructions to the publisher to produce derived assets (thumbnails,
  format conversions)
- **Cross-content references** — a field that must resolve to an item in another collection
- **Collection invariants** — rules that apply across all items of a type

## Decision

Schemas are written in **annotated markdown**. The schema file looks like the content it describes,
with constraints expressed as definition lists and named slots marked with `{#name}` anchors.

A `----` separator (four dashes) marks the boundary between the structured preamble (named,
constrained slots) and the free body content.

### Example: blog article schema

```markdown
# Your blog post title {#title}
occurs
: exactly once
content
: capitalized

Your article summary. Tell the reader what they will learn. {#summary}
occurs
: 1..3

[<name>](/authors/<name>) {#author}
occurs
: exactly once

![cover image description](images/*.(jpg|jpeg|png|webp)) {#cover}
orientation
: landscape
thumbnail
: generate[(1024x768),(800x600)]
alt
: required

----

Body content. Headings H3–H6 only (H1 and H2 are reserved for the template).
headings
: h3..h6
```

### Key properties of the format

**Schema as document template**: reading the schema gives an author a clear picture of what their
article should look like. The placeholder text (`Your blog post title`, `Your article summary…`) is
shown in the content editor as authoring hints. Paragraph slots are written as plain text lines with
a `{#name}` anchor — visually indistinguishable from document prose. This preserves the core
property: the schema looks like the document it describes. Cardinality (`occurs: 1..3`) lives in
the definition-list constraints below the line, eliminating the old `paragraphs [min..max]`
bracket-count syntax.

**Named slots via anchors**: `{#title}`, `{#cover}`, `{#summary}` make structural positions
queryable. Templates and content documents reference them as `${article:title}`,
`${article:cover}`, `${article:summary}`.

**Definition list constraints**: attached to the element immediately above. `occurs: exactly once`,
`orientation: landscape`, `headings: h3..h6`. Unambiguously parsed as schema metadata, not content.

**Generation hints**: `thumbnail: generate[(1024x768),(800x600)]` instructs the publisher to
produce image derivatives. Generated and computed fields (including intrinsic properties like
`width`, `height`, `average_color`) join the data graph and are referenceable:
`${article:cover:average_color}`, `${article:cover:thumbnail:1024x768}`.

**Content documents are plain markdown**: `{#name}` anchors appear only in schema files. Content
editors write ordinary markdown with no schema annotations. The publisher infers which slot each
element occupies by matching document elements positionally against the schema's slot sequence.
This is a hard design constraint — schema syntax must never leak into the content editing
experience.

**Path-based schema binding**: schemas attach to path patterns (`blog/*`, `authors/*`). The
filesystem layout is the taxonomy. No separate routing or schema registry configuration.

**Format-agnostic references**: `${author(johlrogge):bio}` resolves an author by identity (the
filename) and projects a field. This syntax works identically in content documents and templates —
both query the same data graph.

**The `----` separator**: cleanly marks where named/constrained slots end and free body content
begins. The body section can have its own constraints (e.g. heading level range).

### What the publisher does with a schema

1. Parses the schema markdown into a document grammar (sequence of typed, named, constrained slots)
2. Parses the content document
3. Validates the content document against the grammar — structure, constraints, references
4. Resolves cross-content references and validates the referenced objects
5. Checks collection invariants across all items of this type
6. Executes generation hints (thumbnail creation, format conversion)
7. Makes all resolved and computed fields available in the data graph for templates

## Alternatives considered

**YAML/JSON Schema** — good tooling, widely understood, but describes fields not document
structure. Cannot express "the first image in the document is the cover." Poorly serves the
author-prompt and documentation roles. Also: the author actively dislikes TOML.

**Rust DSL compiled into the publisher** — fully type-safe, but inaccessible to non-developers
and requires recompiling to change a schema. Violates the "writers are not developers" principle.

**Custom YAML/TOML with a document grammar extension** — could work but creates a hybrid format
with no existing tooling and no readability advantage over JSON Schema.

## Consequences

**Positive:**
- Schema files are human-readable and serve as author documentation without extra effort
- The content editor can display schema files as authoring prompts natively
- Uniform reference syntax across content and templates simplifies the mental model
- Generation hints in the schema keep asset pipeline config close to content type definition
- Content documents are plain markdown — authors never see or touch schema syntax

**Negative / open questions:**
- Requires a custom schema parser — no existing library parses this format
- Definition list constraint syntax is valid CommonMark but unusual — parser must distinguish
  "constraint definition list" from "content definition list" (the `{#name}` anchor may be
  sufficient to establish schema context)
- The constraint vocabulary (`occurs`, `orientation`, `headings`, `thumbnail`, `alt`, etc.) needs
  to be fully specified
- Data-shaped content (JSON/YAML records like a product catalog) may need a different schema
  surface — the document grammar approach fits markdown content well but may not map cleanly to
  pure data records

## Experiment scope

Before committing to this format, validate it against a real case:

1. Write a schema for a blog article in this format
2. Write one real post from blog.agical.se as content
3. Implement a parser that reads the schema into an internal grammar representation
4. Implement a validator that checks the content document against the grammar
5. Evaluate: is the format expressive enough? Is the parser tractable? Does it read well?

Do not implement generation hints or cross-content references in the experiment — validate the
document grammar and named slots first.

## Evaluation

**Verdict: format accepted.**

The experiment (steps 1–5) is complete. Findings:

**Format readability:** The annotated markdown format reads naturally. A schema for a blog
article is immediately comprehensible to a non-technical author. The definition-list constraint
syntax is unobtrusive. The `----` separator is intuitive. The "schema looks like the document"
property holds for all four element types.

**Parser tractability:** A line-by-line state machine parser is straightforward to implement
and reason about. No library dependency is required for the schema parser. The grammar types
(`Grammar`, `Slot`, `Element`, `Constraint`, `BodyRules`) map cleanly to the format.

**Paragraph slots:** The initial `paragraphs [min..max] {#name}` keyword syntax was replaced
with plain text lines carrying a `{#name}` anchor. This restores format consistency —
paragraph slots are now visually indistinguishable from document prose, matching the approach
used for headings, links, and images. Cardinality is expressed via `occurs: 1..3` in the
definition-list constraints.

**Content documents are plain markdown:** Authors write ordinary markdown with no schema
syntax. The publisher infers slot assignment positionally. This is a hard design constraint
that survived the experiment intact.

**Open questions for future ADRs:**
- Positional matching handles the common case but needs work for optional slots (a missing
  optional slot currently cascades into false errors for subsequent slots).
- The content parser flattens inline vs. block context (an inline link and a standalone link
  both produce `ContentElement::Link`). The validator cannot currently distinguish them.
- The constraint vocabulary (`occurs`, `orientation`, `headings`, `alt`, `content`) is
  sufficient for the experiment scope but needs formal specification before general use.
- Pattern matching for link/image `pattern` fields is parsed but not yet validated.

These open questions are scoped to future work and do not block accepting the format.
