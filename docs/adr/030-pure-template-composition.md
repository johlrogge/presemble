# ADR-030: Pure template composition and content-layer assembly

## Status

Proposed

## Context

Presemble has four core concepts: schemas, content, templates, and stylesheets. ADR-005
established that templates are data (DOM trees). ADR-013 added template composition via
`presemble:include`. Over time, templates have accumulated responsibilities beyond
presentation: they select which collections to iterate, conditionally include content based
on data presence, and resolve callable templates from a registry.

A brainstorming session (2026-04-05) explored whether content assembly should be a separate
5th concept ("documents" as EDN files). The exploration revealed that content + schema can
already express everything a document would — via link expressions in content files and
collection/cardinality constraints in schemas. The key insight: **the document concept
collapses back into content + schema** when links become expressions.

This ADR formalises the resulting model: templates are pure functions, content is the
assembly layer, and schemas are the contract layer.

### The principle: "The document promises, the template presents"

- **Schemas** define contracts: cardinality, type constraints, sort order, nesting depth
- **Content** fulfils contracts: link expressions select, sort, filter, and compose content
- **Builder** validates fulfilment against contract, then matches templates by name
- **Templates** receive a complete, validated sub-tree and produce DOM — nothing else

### Link expressions in content

Content files already use markdown links for references. Extended with Clojure-style
expressions, links become the assembly mechanism:

Simple reference:
```markdown
## Header {#header}
[](/fragments/header)
```

Collection with operations:
```markdown
## Featured posts {#featured}
[](-> /post (select :all) (sort .published :desc) (take 4))
```

Re-rooting (creating a lens into another content tree):
```markdown
[:internal (under :overview)](-> / (chroot :overview))
```

### Schema constraints (the contract)

```markdown
# Featured posts {#featured}
type: link(post)
occurs: 1..4
sort: publish-date desc
```

The builder validates that the link expression result satisfies the schema constraint.
This is the same architecture as schemas validating content, lifted one level up.

### Template purity: templates as function files

A template file contains:
1. **Local template definitions** (optional — like `let` bindings)
2. **A composition expression** (the return value)

```clojure
[
  [:template "body"
    [presemble/insert {:data "input.title" :as :h1}]
    [presemble/insert {:data "input.body"}]]

  (juxt
    /fragments/structure#header
    (apply self/body)
    /fragments/structure#footer)
]
```

Two combinators suffice:
- **pipe (`->`)** — output feeds next input (data transforms)
- **juxt** — same input to all, concatenate outputs (template composition)

Templates never select content, never resolve references, never reach outside their input.

## Decision

Adopt the pure template composition model:

1. **Content layer owns all assembly.** Link expressions in content files replace any content
   selection currently done by templates. Schemas constrain what link expressions may deliver.

2. **Templates are pure functions.** A template receives a validated sub-tree as `input` and
   produces DOM. It may delegate to other templates via `apply` or `juxt`, but may not select
   content or access global state.

3. **Builder validates contracts.** The builder checks that link expression results satisfy
   schema constraints before any template rendering occurs. The site does not build unless
   all layers are valid.

4. **Naming convention binds templates to content.** `content/post/item.md` matches
   `templates/post/item.html` by parallel path — no routing layer needed.

### Current violations and migration

The following mechanisms in the current codebase violate template purity:

| Mechanism | Location | Violation | Migration |
|-----------|----------|-----------|-----------|
| Root-level collection injection | `publisher_cli/src/lib.rs:573-599` | `build_render_context()` injects all collections at root — templates reach for global names | Collections should be assembled by the content file via link expressions, scoped under `input` |
| `data-each="collection"` | `transformer.rs:86-102` | Template selects which collection to iterate by name from the global namespace | Iteration target should be a path under `input` (e.g. `data-each="input.featured"`) — the content file decides what goes there |
| `presemble:include src="header"` | `transformer.rs:45-65` | Template pulls in fragments from filesystem | Replace with template composition (`juxt`, named template references) — composition is still the template's job, but via the algebra, not filesystem splicing |
| `presemble:apply` with callable resolution | `transformer.rs:523-569` | Template resolves callables from a registry and selects data context | Align with the `apply` combinator model — template delegates to named templates with its own input, no registry lookup |
| `data-slot="path"` | `transformer.rs:69-85` | Conditional rendering based on data presence | Low priority — arguably presentation logic. Could stay as-is or become metadata-driven |

### Migration path

**Phase 1: Content link expressions** — Extend the content parser to support link expressions
(`-> /post (select :all) (sort .published :desc) (take 4)`). Extend schemas with collection
constraints (`type: link(post)`, `occurs: 1..4`, `sort: publish-date desc`). Builder resolves
link expressions and validates against schema constraints.

**Phase 2: Scoped template input** — Change `build_render_context()` to pass the content
file's assembled tree as `input` instead of injecting collections at root. Update templates
to reference `input.featured` instead of `features`. This is the largest migration step.

**Phase 3: Template algebra** — Implement `juxt` and `apply` as first-class template
composition mechanisms. Migrate `presemble:include` usages to the new model. Local template
definitions replace the `<template name="...">` library pattern.

**Phase 4: Deprecate global namespace** — Remove root-level collection injection. All data
flows through `input`. Templates that reference bare collection names become build errors.

## Alternatives considered

- **Documents as a 5th concept (EDN files)** — Explored in detail. EDN document files would
  assemble content into named trees with metadata contracts and expression fulfillment.
  Rejected: creates collision risk with content files (`content/fish/hello.md` vs
  `docs/fish/hello.edn`), and content + schema can express everything documents would.
  The EDN exploration was valuable — it revealed the contract/fulfilment separation that
  now maps to schema/content.

- **Keep template-driven selection** — Templates continue to select collections and resolve
  includes. Rejected: violates the single-responsibility principle, makes templates
  untestable in isolation, prevents the builder from validating completeness before rendering.

- **Separate routing/pages layer** — A `pages/` directory mapping documents to templates to
  URLs. Rejected: the naming convention already handles this. Content files that need
  multiple renderings (HTML + RSS) can use separate content files or declare aliases.

## Consequences

**Positive:**
- Templates are testable in isolation — pass a data tree, assert DOM output
- Builder can validate completeness before rendering — no runtime surprises
- Content files are self-describing — editorial prose and assembled data in one place
- Link expressions are natural — the most fundamental thing in content becomes the
  composition mechanism
- Schema constraints and link expressions mirror the type/value relationship — same
  validation architecture at every level
- Enables REPL exploration of assembled content trees
- Enables parallel rendering (pure templates have no shared mutable state)
- Content assembly is visible in content files, not hidden in template directives

**Negative / open questions:**
- Link expression syntax in markdown needs design (Clojure-in-markdown ergonomics)
- Filter/where syntax for conditions ("only articles tagged dinosaur") not yet specified
- Migration is multi-phase — existing templates and the build pipeline need gradual updating
- `data-slot` (conditional rendering) sits on the boundary between selection and presentation
- How aliases/multiple-template-binding works without a document manifest
- The `chroot` mechanism for re-rooting needs formal specification
