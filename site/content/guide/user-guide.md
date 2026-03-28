# User Guide

The complete reference for Presemble — schemas, content, templates, the data graph, and the build pipeline.

----

## Schemas

A schema is a markdown file that defines the grammar of a content type. It lives in `schemas/<name>.md`. The file stem (`post`, `feature`, `note`) becomes the collection name in the data graph and determines which content directory and template are used.

### Slots

Each field in a schema is declared as a markdown element followed by a `{#field-name}` anchor. The element type determines what the content author must write.

#### Heading slots

```markdown
# Post title {#title}
## Section heading {#section}
```

The number of `#` symbols (1–6) sets the required heading level. Hint text between `#` and the anchor is shown to content authors.

#### Paragraph slots

```markdown
A one-sentence summary of the post. {#summary}
```

Any text line ending with `{#name}` is a paragraph slot. The text before the anchor is shown as a hint.

#### Link slots

```markdown
[<name>](/author/<name>) {#author}
```

The URL pattern may use `<variable>` placeholders. The link text is the hint.

#### Image slots

```markdown
![cover image](images/*.(jpg|jpeg|png|webp)) {#cover}
```

The URL is a glob pattern. Alt text is the hint.

### Constraints

Constraints are written as definition lists immediately below the slot they constrain. Multiple constraints can follow the same slot.

```markdown
# Post title {#title}
occurs
: exactly once
content
: capitalized
```

#### `occurs` — occurrence count

| Value | Meaning |
|---|---|
| `exactly once` | Exactly one occurrence required |
| `N` | Exactly N occurrences (e.g. `2`) |
| `1..3` | Between 1 and 3 occurrences (inclusive) |
| `1..` | At least 1 occurrence |
| `..3` | At most 3 occurrences |
| `at least once` | One or more occurrences |
| `at least N` | N or more occurrences |
| `at most N` | Up to N occurrences |

#### `content` — text content constraint

| Value | Meaning |
|---|---|
| `capitalized` | Text must begin with an uppercase letter |

#### `headings` — heading level range (body section only)

| Value | Meaning |
|---|---|
| `h2..h6` | Headings H2 through H6 allowed |
| `h3..h6` | Headings H3 through H6 allowed |
| `h1..h6` | All heading levels allowed |

#### `orientation` — image aspect ratio

| Value | Meaning |
|---|---|
| `landscape` | Width must exceed height |
| `portrait` | Height must exceed width |

#### `alt` — image alt text

| Value | Meaning |
|---|---|
| `required` | Alt text must be present |
| `optional` | Alt text is optional |

### The body separator

`----` divides the preamble (structured named slots) from the body (free-form markdown). Everything after `----` is validated as a block but not individually named.

```markdown
# Post title {#title}
occurs
: exactly once

A summary paragraph. {#summary}
occurs
: 1..3

----
Body content. Headings H3–H6 only.
headings
: h3..h6
```

The body section is optional. Schemas without `----` have no free-form body.

### Full schema example

```markdown
# Post title {#title}
occurs
: exactly once
content
: capitalized

A one-sentence summary. {#summary}
occurs
: 1..3

[<name>](/author/<name>) {#author}
occurs
: exactly once

![cover image](images/*.(jpg|jpeg|png|webp)) {#cover}
orientation
: landscape
alt
: required

----
Body content. H3–H6 only (H1 and H2 are reserved for the template).
headings
: h3..h6
```

## Content

Content files are plain markdown in `content/<schema-stem>/<slug>.md`. Authors do not write schema annotations — the publisher infers field assignment by position, matching the document structure against the schema in order.

```markdown
# My Post Title

A one-sentence summary of what you will learn.

[Author Name](/author/author-name)

----

### First section

Body text here.
```

The `{#field-name}` anchor syntax is for schema authors only. Content files are annotation-free.

### Unused source warnings

After each build, the publisher reports source files that exist but contribute to no output:

```
warning: assets/logo-old.svg is not referenced by any template, consider deleting it
warning: templates/draft.html is not used by any schema or include, consider deleting it
warning: schemas/draft.md has no content files in content/draft/, consider deleting it
warning: content/orphan/ has no matching schema, consider deleting it
```

These are informational — the build still succeeds.

## Templates

Templates are HTML files in `templates/`. The file stem must match a schema name (`post.html` for `post.md`) or be `index.html` for the home page. Included partials can have any name.

### `presemble:insert`

Insert a named data path into the page:

```html
<presemble:insert data="post.title" as="h1" />
<presemble:insert data="post.summary" as="p" />
<presemble:insert data="post.body" />
```

| Attribute | Required | Description |
|---|---|---|
| `data` | yes | Dot-separated path into the data graph |
| `as` | no | Wrap the value in this element (e.g. `h1`, `p`, `span`) |

Omitting `as` inserts the value as raw HTML nodes. If the path is absent from the data graph, the element is removed silently.

### `data-each`

Iterate over a collection on a `<template>` element:

```html
<template data-each="posts">
  <li>
    <presemble:insert data="post.title" as="h3" />
    <a data="post.url_path" href="">Read more</a>
  </li>
</template>
```

The `<template>` wrapper is not emitted. Inside the loop, data paths are relative to the current item.

### `presemble:include`

Include a named partial template:

```html
<presemble:include src="header" />
<presemble:include src="components/card" />
```

The `src` value is a file stem relative to `templates/`. Named template definitions inside a file use `::`:

```html
<presemble:include src="components::card" />
```

### `data-href`

Bind an `href` attribute from the data graph:

```html
<a data-href="post.url_path">Read more</a>
```

### `presemble:class`

Conditionally apply a CSS class:

```html
<li presemble:class="active:is-active">...</li>
```

Syntax: `data-path:class-name`. If the path resolves to a truthy value, the class is added.

### URL paths in templates

Always write root-relative paths in templates. The publisher rewrites them at build time:

```html
<link rel="stylesheet" href="/assets/style.css">
<a href="/feature/schemas-as-contracts">Schemas</a>
```

See [URL rewriting](#url-rewriting) for deployment configuration.

## The data graph

Schemas, content, and templates connect through named data paths. A schema named `post` with a field `{#title}` creates the path `post.title`. Templates traverse these paths to build output.

### Path structure

| Path form | Example | Description |
|---|---|---|
| `schema.field` | `post.title` | A named slot value |
| `schema.field.subfield` | `post.author.name` | A nested reference (linked content) |
| `schema.url_path` | `post.url_path` | The page's clean URL |
| `schema.body` | `post.body` | The free-form body as HTML |

### Collections

Collections are accessed by the plural schema name in `data-each`:

```html
<template data-each="posts">
  <presemble:insert data="post.title" as="h2" />
</template>
```

A schema named `post` produces a collection `posts`. The item variable inside the loop is `post` (singular).

Because the schema defines which paths exist, the template vocabulary is finite and verified at build time. A template that references a non-existent path fails the build.

See [the data graph](/feature/the-data-graph) for how cross-content references work.

## Building

```
presemble build <site-dir>
presemble build <site-dir> --config <config-file>
```

The publisher reads every content file, validates it against its schema, resolves the template, and writes output to `<site-dir>/output/`. Each content file becomes a clean URL:

```
content/post/my-post.md  →  output/post/my-post/index.html  →  /post/my-post
content/docs/index.md    →  output/docs/index.html           →  /docs/
```

Only assets referenced by templates are copied to `output/assets/`. Unreferenced files are reported as warnings.

### CLI flags

| Flag | Description |
|---|---|
| `--config <path>` | Load a named URL config file (e.g. `.presemble/github-pages.json`) |
| `--url-style <relative\|root\|absolute>` | Override the URL style |
| `--base-path <path>` | Base path prefix for root-relative style (e.g. `/presemble`) |
| `--base-url <url>` | Base URL for absolute style (e.g. `https://presemble.io`) |

### Initialising a new site

```
presemble init <dir>
```

Scaffolds a hello-world site with a `note` schema, one content file, matching templates, and a minimal stylesheet.

## Serving

```
presemble serve <site-dir>
```

Starts a local server on port 3000. The server watches `schemas/`, `content/`, and `templates/` for changes and rebuilds affected pages incrementally. Validation errors are reported to the terminal on every rebuild.

## URL rewriting

Authors always write root-relative URLs in content and templates. The publisher transforms them at serialization time.

### URL styles

| Style | Output example | When to use |
|---|---|---|
| `relative` (default) | `../../assets/style.css` | Works at any root, zero config |
| `root` | `/presemble/assets/style.css` | Known subdirectory (e.g. GitHub Pages) |
| `absolute` | `https://presemble.io/assets/style.css` | RSS, Open Graph, canonical links |

### Config file

Create `.presemble/config.json` (or a named file for each deployment target):

```json
{ "url-style": "root", "base-path": "/presemble" }
```

```json
{ "url-style": "absolute", "base-url": "https://presemble.io" }
```

Select with `presemble build site/ --config .presemble/github-pages.json`.

Default (no config file): relative URLs, works everywhere.

See [deployment URL rewriting](/feature/deployment-url-rewriting) for full details.

## Validated at every level

Presemble validates at every stage:

| Stage | What is checked |
|---|---|
| Schema parse | Schema file grammar — field names, constraint syntax, valid constraint values |
| Content validation | Each content file structure against its schema — field presence, occurrence counts, content constraints, heading levels |
| Template resolution | Data paths in templates exist in the schema |
| Asset resolution | Assets referenced by templates exist on disk |
| Link validation | Internal `href` values resolve to built pages |
| HTML output | Final HTML is well-formed XML |

Validation failures are build failures. There is no runtime state where invalid content reaches a browser.

See [validated at every level](/feature/validated-at-every-level) for the validation model.
