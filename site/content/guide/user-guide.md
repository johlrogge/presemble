# User Guide

The complete reference for Presemble — schemas, content, templates, the data graph, and the build pipeline.

----

## Schemas

A schema is a markdown document that defines the grammar of a content type. Each paragraph, heading, or block in the schema becomes a named field in the data graph.

Fields are annotated with `{#field-name}` anchors, and constrained with `occurs`, `content`, and `headings` definition lists:

- `occurs: exactly once` — the field must appear exactly one time
- `occurs: 1..3` — the field may appear one to three times
- `content: capitalized` — the text content must start with an uppercase letter
- `headings: h3..h6` — body headings are restricted to H3 and below

The `----` separator divides header fields from the free-form body section. Body fields are validated structurally but not named individually.

See [schemas as contracts](/feature/schemas-as-contracts) for worked examples and the full constraint vocabulary.

## Content

Content files are plain markdown. Authors do not write schema annotations — the publisher infers field assignment by position, matching the document structure against the schema in order.

A content file for a `post` schema looks like:

```markdown
# My Post Title

A one-sentence summary.

[Author Name](/author/author-name)

----

### First section heading

Body text continues here.
```

The `{#field-name}` anchor syntax in schema files is for schema authors, not content authors. Content files are annotation-free.

## Templates

Templates are HTML files that reference named data paths using `presemble:insert`:

```html
<presemble:insert data="post.title" as="h1" />
<presemble:insert data="post.summary" as="p" />
```

The `as` attribute wraps the content in the specified element. Omit it to insert raw HTML.

To iterate over a collection, use `data-each` on a `<template>` element:

```html
<template data-each="posts">
  <li>
    <presemble:insert data="title" as="h3" />
  </li>
</template>
```

To reuse a partial, use `presemble:include`:

```html
<presemble:include src="header" />
```

See [templates are data](/feature/templates-are-data) for the full template surface.

## The data graph

Schemas, content, and templates connect through named data paths. A schema named `post` with a field `{#title}` creates the path `post.title`. Templates traverse these paths to build output.

Because the schema defines which paths exist, the template vocabulary is finite and verifiable at build time. A template that references a non-existent path fails the build immediately.

Nested paths follow dot notation: `post.author.name`, `post.author.bio`. Collections are accessed by name with items rendered via `data-each`.

See [the data graph](/feature/the-data-graph) for how paths are resolved and how collections are structured.

## Building

Run the build command from your workspace root:

```
presemble build <site-dir>
```

The publisher reads every content file, validates it against its schema, resolves the template, and writes output to `<site-dir>/output/`. Each content file becomes a clean URL directory:

```
output/post/my-post/index.html
```

Assets are copied verbatim. See [instant feedback](/feature/instant-feedback) for details on error reporting and build output structure.

## Serving

Start the development server:

```
presemble serve <site-dir>
```

The server watches `schemas/`, `content/`, and `templates/` for changes and rebuilds affected pages incrementally. Validation errors are reported to the terminal on every rebuild. A future release will push errors and live reload directly to the browser.

## URL rewriting

Authors always write root-relative URLs in content and templates. The publisher transforms them at serialization time according to the configured deployment style:

- **Relative** (default) — works at any root, zero config
- **Root-relative with base path** — for GitHub Pages or staging subdirectories, set `base-path` in `.presemble/config.json`
- **Absolute** — for RSS, Open Graph, and canonical links, set `base-url` in `.presemble/config.json`

Multiple deployment targets are supported via named config files selected with `--config`. See [deployment URL rewriting](/feature/deployment-url-rewriting) for full details.

## Validated at every level

Presemble validates content at every stage of the pipeline:

- Schema parse — the schema file itself is validated for grammar correctness
- Content validation — each content file is checked against its schema before rendering
- Template validation — data paths referenced in templates are checked against the schema
- Build output — the final HTML is well-formed

Validation failures are build failures. There is no runtime state where invalid content can reach a browser. See [validated at every level](/feature/validated-at-every-level) for the full validation model.
