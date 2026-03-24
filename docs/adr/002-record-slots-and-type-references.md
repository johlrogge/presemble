# ADR-002: Record Slots and Type References

## Status

Under evaluation

## Context

ADR-001 established the annotated markdown schema format for document-shaped content — headings,
paragraphs, images, links. But content types also need data-shaped fields: publish dates, tags,
slugs, and other metadata that authors edit but that may or may not appear in the published output.
Standard frontmatter (YAML/TOML between `---` fences) would solve this but violates the core
principle: content documents must be plain markdown with no schema syntax.

The question is: can data-shaped metadata be expressed in plain markdown, validated by the schema
format, and edited naturally in an editorial context — without frontmatter?

## Decision

Two mechanisms are introduced:

### 1. Record slots

A named heading slot introduces a *record slot* — a table block in the content document containing
key/value pairs. The schema declares field shape using a table with `field / type / occurs` columns.
The content document uses a matching table with `field / value` columns.

Schema:

```markdown
## Metadata {#metadata}

| field        | type     | occurs |
|--------------|----------|--------|
| publish-date | iso-date | 0..1   |
| tags         | string   | 0..*   |
```

Content document (plain markdown, no annotations):

```markdown
## Metadata

| field        | value      |
|--------------|------------|
| publish-date | 2026-03-22 |
| tags         | rust       |
```

Note: multiple values for a field (e.g. multiple tags) can use multiple rows with the same field
name.

Templates decide whether to render the Metadata section in published output. In editorial view, it
is a normal editable section.

### 2. Slot-level constraints (table form)

Slot-level constraints are expressed as a table immediately following a named slot (separated by a
blank line). This is the same positional association principle as ADR-001 — whatever follows a named
slot before the next slot or separator is that slot's constraints.

Example (paragraph slot with inline link cardinality constraints):

```markdown
This article introduces [<name>](/authors/<name>) and covers [<product>](/products/<product>). {#attribution}

| field   | occurs |
|---------|--------|
| name    | 1      |
| product | 0..1   |
```

The paragraph text acts as the "signature" — inline link patterns declare the named placeholders.
The constraint table acts as the "bounds" (inspired by Rust's `where` clause), specifying cardinality
for each named link pattern.

### 3. Inline links as graph edges

Inline links inside paragraph slots are semantic: they are edges in the data graph. A schema
declares inline link patterns within the paragraph text itself using `[<name>](/path/<name>)`
syntax, where `<name>` is a named placeholder. A constraint table specifies cardinality for each
named link pattern.

This allows the schema to express structural relationships — not just "this paragraph must appear,"
but "this paragraph must link to an author and optionally a product."

### 4. Generalisation

Constraint tables generalise to all slot types — not just paragraph slots. Image constraints,
heading constraints, and record field declarations can all be expressed as tables with consistent
`field / value` or `field / occurs` columns. This is a single mechanism for all constraint
expression, regardless of slot type.

### 5. Slot-level type references

A record slot can reference an external schema fragment instead of declaring fields inline:

```markdown
## Metadata {#metadata}

[[data/article_metadata.schema]]
```

The `[[path]]` reference is a *shared named type*: the referenced file defines the field shape, and
changes propagate to every schema that references it. The referenced file is itself annotated
markdown schema format — the same format used everywhere.

This is **slot-level composition only**. There is no schema-level inheritance ("this schema extends
that schema"). A reference pulls a named type into a specific slot position.

## Alternatives considered

**YAML/TOML frontmatter** — solves the metadata problem but violates "content is plain markdown."
Authors would need to edit YAML in a markdown file. Rejected.

**Fenced code block for constraints** (` ```presemble `) — avoids definition list ambiguity but
introduces non-markdown syntax into schema files. Rejected.

**Schema-level inheritance** — `[[reference]]` at the top of a schema file meaning "this schema
extends that one." Introduces "is-a" semantics, parent/child conflict resolution, and override
rules. Slot-level composition covers the known use cases. Rejected.

**Inline field declarations only (no references)** — simpler, but forces repetition when multiple
content types share a metadata shape. The `[[reference]]` mechanism adds reuse without complexity.

**Definition lists for constraints** — the original approach in ADR-001 and the initial proposal in
this ADR. Replaced by tables because: (1) definition lists are not supported by pulldown-cmark
without extensions, (2) tables have wider markdown parser support (GFM), (3) tables make the column
structure of constraints explicit and scannable, (4) tables naturally express multi-column
constraints (field + type + occurs) without syntax ambiguity.

## Consequences

**Positive:**
- Content documents remain plain markdown — authors write tables naturally
- Metadata lives in the document, editable and viewable in editorial context
- Templates control what is rendered; the schema has no "editorial-only" concept
- Shared types via `[[reference]]` enable reuse without repetition
- The schema format remains uniform — referenced files use the same annotated markdown format
- Tables have broad parser support (GFM) and make constraint structure immediately scannable

**Negative / open questions:**
- The `[[path]]` reference resolver adds a build-time dependency graph: the publisher must track
  which schemas reference which type files and re-validate when types change
- Field type vocabulary (`iso-date`, `string`, etc.) needs formal specification
- Cardinality syntax for fields (`0..1`, `0..*`) should align with the `occurs` constraint syntax
  from ADR-001

## Experiment scope

Before implementing, validate with a concrete example:

1. Write an article schema using table-based constraints, inline link patterns, and a metadata
   record slot with table field declarations
2. Write a content document with inline links and a metadata table
3. Validate the format reads naturally for both schema authors and content authors
4. Assess whether `comrak` (GFM tables + definition lists) or `pulldown-cmark` with table extension
   is the right parser
