# ADR-004: Template Language

## Status

Superseded by ADR-005

## Retirement note

This ADR described a `{{ }}` pipe expression string interpolation model for templates. It is superseded by ADR-005, which establishes a fundamentally different approach: templates are DOM trees, not text with holes. The DOM transformation model guarantees structural validity by construction, which string interpolation cannot.

The pipe expression vocabulary defined here (`each`, `maybe`, `match`, `default`, `first`, `rest`) is preserved in ADR-005 as the data graph query language for attribute-level bindings (`presemble:class`). The string interpolation delivery mechanism (`{{ }}` delimiters, the `template:` pipe, `FileTemplateLoader`) is removed.

## Context

Presemble produces static HTML from validated content. The template system is the bridge between
the data graph (named slots, validated content, computed fields) and the rendered output.

Standard template engines (Tera, MiniJinja, Askama, Jinja2) solve this problem but carry Django
lineage: string templates with `{{ variable }}` holes and `{% for %}` / `{% if %}` block
directives. These are well-understood but treat templates as strings with control flow, not as
composable functions.

Presemble's data model is already graph-oriented — named slots are queryable as `${article.title}`,
cross-content references as `${author(johlrogge).bio}`, computed fields as
`${article.cover.average_color}`. The question is whether templates should speak the same language,
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
{{ article.title }}
{{ author(johlrogge).bio }}
{{ article.cover.average_color }}
```

Same reference syntax as content documents — uniform across the whole system.

### Pipe-based transformations

Transformations are chained via `|`. Each transformation takes a value and produces a new value:

```
{{ article.title | uppercase }}
{{ article.cover | thumbnail(800x600) | url }}
{{ article.published_at | date_format("MMMM d, yyyy") }}
{{ article.cover.orientation | match(landscape => "cover--landscape", portrait => "cover--portrait") }}
{{ article.subtitle | default("Untitled") }}
{{ article.summary | rest }}
```

### Iteration via `each`

Iteration is not a block directive — it is a pipe transformation. `each` maps a template over a
collection:

```
{{ site.articles | each(template:article_card) }}
```

`template:article_card` references another template file. That file receives one article from the
collection and produces an HTML fragment. The fragments are concatenated at the call site.

### Collection accessors

Collection values (multi-occurrence slots) support positional access via pipe transforms:

```
{{ article.summary | first }}
{{ article.summary | rest | each(template:summary_continuation) }}
```

`first` returns the first element. `rest` returns all elements except the first. Together with
`each`, they enable split-and-remap patterns where different parts of a collection receive
different templates.

Additional transforms (`last`, `nth(N)`, `take(N)`, `skip(N)`) are reserved for future use.

### Optionality via `maybe`

`maybe` applies a template only if the value is present (non-null):

```
{{ article.cover | maybe(template:cover_block) }}
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

### Fragment context scoping

When a template is invoked via a pipe (`maybe`, `each`, or direct `template:` reference), the
piped value becomes the template's context root. The fragment sees only the value it receives,
not the caller's full context. Fields are accessed as bare names on the context root.

This makes fragments pure functions of their input — a cover fragment says `{{ path }}` and
`{{ alt }}`, not `{{ article.cover.path }}`. The same fragment works for any content type that
has a cover slot.

### Template context map

Each page render receives a context map: a set of named root values provided by the publisher.
Top-level names in expressions (`site`, `article`) resolve from this map. The publisher
establishes the context map based on content type and routing rules.

Fragment templates invoked via pipes receive a narrowed context: the piped value is the sole
root. Page templates are entry points with declared dependencies; fragments are pure functions
of their piped input.

### Conditionals and branching

Attribute-level conditionals are handled by `match` (maps enumerated values to output strings)
and `default` (provides a fallback for absent values). Both are pipe transforms.

Full block-level branching — different HTML structure based on a predicate — remains deferred.
If cases arise where `match` is insufficient (e.g., rendering entirely different element trees
based on a value), a `when` or block-level mechanism will be introduced. The failure mode
will determine the form.

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
- Attribute-level conditionals are resolved via `match` and `default`; full block-level branching remains deferred
- No existing library implements this model; this is a Presemble-native component

## Experiment scope

1. Write two or three real templates for blog.agical.se content using the proposed syntax (pick a
   delimiter, write the HTML, use `each` and `maybe` for a list page and a detail page)
2. Evaluate: does the pipe model cover all needed cases? Where does it strain?
3. Assess delimiter ergonomics empirically
4. Implement a minimal expression interpreter (graph lookups + pipes + `each` + `maybe`) and
   render one content type to HTML

## Evaluation

Real templates were written for the blog-site fixture (article detail page, article list
page, article card fragment, article cover fragment). These are the findings.

### What worked well

The basic expression model — graph lookups, pipe chaining, `each` for iteration, `maybe`
for optionality, and `template:` for composition — covered the common cases cleanly. Templates
read as approximations of their output. The pipe model eliminated the need for block
directives in all cases tested. Composition via pipes made data dependencies explicit at
every call site.

### Findings

Four gaps were identified. All are resolved within the pipe model.

**Fragment context scoping** — piped value becomes the fragment's context root; bare field
names access it. Makes fragments pure functions, enables reuse across content types. See
the Fragment context scoping section.

**Template context establishment** — top-level page templates receive a named context map
from the publisher. See the Template context map section.

**Attribute-level conditionals** — resolved by `match` and `default` pipe transforms, not
block directives.

**Multi-occurrence slot access** — resolved by `rest` (and future collection accessors)
combined with `each` for split-and-remap patterns.

### Delimiter ergonomics

`{{ }}` was used for the experiment. It reads well in HTML but causes editor confusion
(Jinja2/Handlebars association). `[[ ]]` avoids this but conflicts with the `[[reference]]`
syntax from ADR-002. If schema files use `[[reference]]` and template files use `[[ ]]`
expression slots, the contextual separation may be sufficient — but this must be stated
explicitly if `[[ ]]` is chosen. Decision remains open.

### Verdict

The pipe model is validated. The four gaps are resolved by specification additions and new
transforms — no block-level syntax or second mechanism is required. Proceed to implementation
of a minimal expression interpreter covering: graph lookups, pipes, `each`, `maybe`, `match`,
`default`, `first`, `rest`, and `template:` composition.

---

## Addendum — 2026-03-27

Collections moved to the root-level namespace. References of the form `site.articles` in this
ADR (e.g. `{{ site.articles | each(template:article_card) }}`) are superseded. The correct form
is `{{ articles | each(template:article_card) }}` — collection names are looked up directly from
the data graph root, not under a `site` prefix.
