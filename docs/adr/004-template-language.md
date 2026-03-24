# ADR-004: Template Language

## Status

Under evaluation

## Context

Presemble produces static HTML from validated content. The template system is the bridge between
the data graph (named slots, validated content, computed fields) and the rendered output.

Standard template engines (Tera, MiniJinja, Askama, Jinja2) solve this problem but carry Django
lineage: string templates with `{{ variable }}` holes and `{% for %}` / `{% if %}` block
directives. These are well-understood but treat templates as strings with control flow, not as
composable functions.

Presemble's data model is already graph-oriented — named slots are queryable as `${article:title}`,
cross-content references as `${author(johlrogge):bio}`, computed fields as
`${article:cover:average_color}`. The question is whether templates should speak the same language,
or introduce a separate template syntax.

## Decision

Templates are **HTML files with embedded expression slots**. The HTML provides the document
structure (designers read templates as pages). Expression slots handle all dynamic content: graph
lookups, transformations, iteration, and template composition — through a single uniform mechanism.

### Expression slots

A delimiter syntax (exact form TBD — `{{ }}`, `[[ ]]`, or similar; chosen for ergonomics after
writing real templates, not locked to `${ }` for familiarity) marks expression slots in HTML.
Everything inside is a Presemble expression.

### Graph lookups

```
{{ article:title }}
{{ author(johlrogge):bio }}
{{ article:cover:average_color }}
```

Same reference syntax as content documents — uniform across the whole system.

### Pipe-based transformations

Transformations are chained via `|`. Each transformation takes a value and produces a new value:

```
{{ article:title | uppercase }}
{{ article:cover | thumbnail(800x600) | url }}
{{ article:published_at | date_format("MMMM d, yyyy") }}
```

### Iteration via `each`

Iteration is not a block directive — it is a pipe transformation. `each` maps a template over a
collection:

```
{{ site:articles | each(template:article_card) }}
```

`template:article_card` references another template file. That file receives one article from the
collection and produces an HTML fragment. The fragments are concatenated at the call site.

### Optionality via `maybe`

`maybe` applies a template only if the value is present (non-null):

```
{{ article:cover | maybe(template:cover_block) }}
```

`each` and `maybe` are the same concept: "how many times to apply a template" — zero-or-many and
zero-or-one respectively.

### Template composition

Templates call other templates by referencing them as values in the graph. Composition is just
a pipe:

```
{{ site | template:header }}
{{ article | template:article_body }}
{{ site | template:footer }}
```

Templates are pure: they take a data graph value and return an HTML fragment. No side effects,
no global state.

### Conditionals and branching

Deferred. The pipe model will be pushed until it breaks. If predicate-based branching (e.g., "use
layout A if cover is landscape, layout B if portrait") cannot be expressed via `match` or `when`
as pipe transforms, a second mechanism will be introduced. The failure mode determines the form
of the solution.

### Delimiter syntax (TBD)

The exact delimiter syntax is not yet decided. Candidates: `{{ }}`, `[[ ]]`, `(( ))`.
Requirements: must not conflict with HTML syntax, must not conflict with JavaScript template
literals (`${ }`), must not conflict with the `[[reference]]` syntax from ADR-002, must read
cleanly in an HTML file. Decision deferred until real templates are written and ergonomics can be
assessed empirically.

## Alternatives considered

**Tera / MiniJinja** — Interpreted Jinja2-style. Well-supported, fast, familiar. Rejected because:
Django/Jinja lineage brings block directives (`{% for %}`, `{% if %}`) as a separate syntax from
variable interpolation — two mechanisms where one should suffice. Does not compose with the data
graph reference syntax.

**Askama** — Compile-time typed templates. Rejected because: interpreted at runtime is a hard
requirement. Templates must be loadable from disk without recompiling the publisher.

**Maud / Markup** — HTML as Rust macros. Rejected: compile-time, not interpreted.

**Embedded scripting language (Rhai, Lua, Starlark)** — Templates as programs in an embedded
language. More powerful than needed; template authors should not need to learn a programming
language. The pipe model covers the needed cases without general-purpose programming.

**Hiccup-style (HTML as data)** — Templates express HTML structure as nested data. Pure and
composable, but breaks the "template looks like the page it produces" property. Template authors
can no longer visually read a template as an approximation of its output.

## Consequences

**Positive:**
- Templates look like the pages they produce — HTML structure is readable as output
- One expression mechanism for everything: lookups, transforms, iteration, composition
- Same reference syntax as content documents — uniform across the whole system
- Pure functions: templates have no side effects, composable by piping values
- Interpreted at runtime — templates loadable from disk, no recompilation needed
- `each` and `maybe` unify iteration and optionality under one model

**Negative / open questions:**
- A custom expression language requires a parser and interpreter — non-trivial investment
- Delimiter syntax is unresolved — needs empirical validation with real templates
- Predicate-based conditionals are deferred — the pipe model may not stretch to cover all cases
- No existing library implements this model; this is a Presemble-native component

## Experiment scope

1. Write two or three real templates for blog.agical.se content using the proposed syntax (pick a
   delimiter, write the HTML, use `each` and `maybe` for a list page and a detail page)
2. Evaluate: does the pipe model cover all needed cases? Where does it strain?
3. Assess delimiter ergonomics empirically
4. Implement a minimal expression interpreter (graph lookups + pipes + `each` + `maybe`) and
   render one content type to HTML
