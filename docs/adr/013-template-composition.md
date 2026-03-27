# ADR-013: Template Composition via Includes

## Status

Proposed

## Context

As the number of page templates grows, shared structural fragments — navigation, headers,
footers, cards — get duplicated across every template file. Each duplication is a maintenance
liability: a change to the site header must be replicated in every template that contains it.

Presemble's template model (ADR-005) treats templates as data trees rather than strings. The
DOM transformation model annotates the tree with `presemble:` elements that describe
transformations declaratively. Template composition is a natural extension of this vocabulary:
a `presemble:include` element declares that another template subtree should be spliced in at
that position.

Two granularities of reuse are needed:

1. **Whole-file fragments** — a standalone template file that is entirely a reusable fragment
   (e.g. `header.html`, `nav.hiccup`).
2. **Named definitions within a file** — a template library file that bundles several related
   fragments, with each fragment identified by a `name` attribute on a `<template>` block
   (e.g. a `common.html` file containing `<template name="card">…</template>` and
   `<template name="hero">…</template>`). This avoids file-per-fragment explosion for closely
   related components.

## Decision

Templates compose via the `<presemble:include src="…" />` annotation element.

### Resolution modes

The `src` attribute value determines the resolution mode:

**Bare name** (`src="header"`) — resolves to a standalone template file. The registry searches
for `header.html` or `header.hiccup` in the configured template directory. The entire file is
the included fragment.

**File-qualified name** (`src="common::card"`) — the part before `::` identifies the template
file (`common.html` or `common.hiccup`); the part after `::` is the name of a definition block
within that file. Definition blocks are `<template name="…">…</template>` elements at the top
level of the file. Only the contents of the matching block are included — not the surrounding
`<template>` wrapper.

### TemplateRegistry trait

A `TemplateRegistry` trait abstracts the resolution mechanism. This keeps the template renderer
decoupled from filesystem concerns and enables test isolation.

```rust
pub trait TemplateRegistry {
    fn resolve(&self, src: &str) -> Result<TemplateNode, IncludeError>;
}
```

Two implementations are provided:

**`FileTemplateRegistry`** (in `publisher_cli`) — filesystem-backed resolution with in-memory
caching. Locates template files relative to a configured root directory, parses them on first
access, and caches the parsed tree for subsequent includes. Uses `RefCell` for interior
mutability; single-threaded only (the publisher build pipeline is single-threaded).

**`NullRegistry`** — always returns an error for any include. Used in unit tests that do not
exercise composition, and in contexts where template composition is not configured. Makes
the absence of a registry explicit rather than silently dropping includes.

### RenderContext

A `RenderContext` is threaded through the template rendering pipeline. It carries:

- A reference to the active `TemplateRegistry`
- The current recursion depth (starts at 0, incremented on each include)

The maximum recursion depth is **32**. Exceeding this limit produces a hard error:
`IncludeError::MaxDepthExceeded`. Circular includes (a template that includes itself directly or
transitively) always hit this limit and are detected this way. There is no separate cycle
detection — the depth limit is the guard.

### Example usage

Standalone fragment include:

```html
<html>
  <body>
    <presemble:include src="header" />
    <main>
      <presemble:insert data="article.title" as="h1" />
    </main>
    <presemble:include src="footer" />
  </body>
</html>
```

Named definition from a library file:

```html
<presemble:include src="common::card" />
```

Library file (`common.html`):

```html
<template name="card">
  <article class="card">
    <presemble:insert data="article.title" as="h2" />
    <presemble:insert data="article.summary" as="p" />
  </article>
</template>

<template name="hero">
  <section class="hero">
    <presemble:insert data="article.cover" />
  </section>
</template>
```

## Alternatives considered

**Pipe-based composition (ADR-004 model)** — ADR-004 proposed a pipe-based template language
where includes could be expressed as pipeline stages. Rejected when ADR-004 was retired in
favour of the DOM transformation model (ADR-005). The `presemble:include` element follows the
same declarative annotation pattern as `presemble:insert`.

**Implicit layout wrapping (Hugo's `baseof.html`)** — a convention where a base layout
automatically wraps every page template unless overridden. Rejected: implicit wrapping requires
understanding the convention before the relationship between files is visible. Explicit
`presemble:include` makes the composition structure readable directly in the template file.

**Import directives in frontmatter or a config file** — listing includes in a metadata block
separate from the template tree. Rejected: separating declarations from their usage site
creates indirection. An include annotation placed in the tree expresses both what is included
and where it appears.

## Consequences

**Positive:**
- Template duplication is eliminated for shared structural fragments
- The `presemble:include` element is consistent with the existing `presemble:` annotation
  vocabulary (ADR-005) — no new conceptual model required
- Named definition blocks allow template libraries without file-per-fragment proliferation
- `TemplateRegistry` trait keeps the renderer testable in isolation
- Circular includes produce a clear, early error rather than infinite recursion or silent
  truncation

**Negative / open questions:**
- Fragment templates must be valid parseable XML or hiccup. A fragment that is not a
  well-formed document on its own (e.g. a bare text node) cannot be used as an include target
- Nested template directories are not yet supported. The registry resolves names against a
  single flat directory. Sub-directory organisation (e.g. `components/card`) is deferred
- `FileTemplateRegistry` uses `RefCell` for caching and is therefore single-threaded only.
  If the publisher build pipeline becomes multi-threaded in future, the registry will need
  to be replaced with a `Mutex`-backed or lock-free implementation
- The `<template name="…">` extraction mechanism reuses the HTML `<template>` element.
  A `<template>` element without a `name` attribute in a library file is ambiguous — it
  could be a Presemble iteration block (ADR-005) or an unnamed definition. The publisher
  treats unnamed top-level `<template>` elements as non-definition nodes; this distinction
  needs to be made explicit in the template parser
