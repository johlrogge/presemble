# ADR-002: Record Slots and Type References

## Status

Proposed

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

A named heading slot followed by a blank line introduces a *record slot* — a definition-list block
in the content document containing key/value pairs. The blank line separates slot-level constraints
(above) from field declarations (below).

Schema:

```markdown
## Metadata {#metadata}
occurs
: 0..1

publish-date
: iso-date
: 0..1
tags
: string
: 0..*
```

Content document (plain markdown, no annotations):

```markdown
## Metadata
publish-date
: 2026-03-22
tags
: rust
: schemas
```

The schema's field declarations use the same definition-list syntax as the content, but with type
names and cardinality as values (e.g. `iso-date`, `string`, `0..1`, `0..*`) rather than actual
data. The blank line after the slot-level constraints signals to the parser that what follows is
field declarations, not more slot constraints.

Templates decide whether to render the Metadata section in published output. In editorial view, it
is a normal editable section.

### 2. Slot-level type references

A record slot can reference an external schema fragment instead of declaring fields inline:

```markdown
## Metadata {#metadata}
occurs
: 0..1

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

## Consequences

**Positive:**
- Content documents remain plain markdown — authors write definition lists naturally
- Metadata lives in the document, editable and viewable in editorial context
- Templates control what is rendered; the schema has no "editorial-only" concept
- Shared types via `[[reference]]` enable reuse without repetition
- The schema format remains uniform — referenced files use the same annotated markdown format

**Negative / open questions:**
- The parser must distinguish slot-level constraints (definition list immediately after element)
  from field declarations (definition list after blank line inside a record slot) — the blank line
  is load-bearing
- The `[[path]]` reference resolver adds a build-time dependency graph: the publisher must track
  which schemas reference which type files and re-validate when types change
- Field type vocabulary (`iso-date`, `string`, etc.) needs formal specification
- Cardinality syntax for fields (`0..1`, `0..*`) should align with the `occurs` constraint syntax
  from ADR-001

## Experiment scope

Before implementing, validate with a concrete example:

1. Write a metadata schema fragment for a blog article (`data/article_metadata.schema`)
2. Reference it from the article schema
3. Write a content document with a Metadata section
4. Verify the format reads naturally for both schema authors and content authors
