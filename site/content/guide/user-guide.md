# User Guide

The complete reference for Presemble — schemas, content, templates, the data graph, and the build pipeline.

----

## Schemas

A schema is a markdown file that defines the grammar of a content type. It lives in `schemas/<type>/item.md`. The directory name (`post`, `feature`, `note`) becomes the collection name in the data graph and determines which content directory and template are used.

```
schemas/post/item.md       ← schema for the "post" type
content/post/my-post.md    ← a post content file
templates/post/item.html   ← template for individual posts
```

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

### Body content types

The body section supports the full range of markdown block and inline content:

**Block elements:**

| Element | Syntax |
|---|---|
| Paragraph | Plain text |
| Heading | `##`, `###`, … (within the schema's `headings` constraint) |
| Blockquote | `> quoted text` |
| Unordered list | `- item` |
| Ordered list | `1. item` |
| Code block | Triple-backtick fence |

**Inline formatting within paragraphs:**

| Format | Syntax |
|---|---|
| Bold | `**text**` or `__text__` |
| Italic | `*text*` or `_text_` |
| Inline code | `` `code` `` |

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

Templates live in `templates/`. Each content type uses a directory: `templates/<type>/item.html` (or `.hiccup`) matches `schemas/<type>/item.md`. The home page template is `templates/index.html`. Shared partials such as `templates/header.html` can have any name and any depth.

Use `presemble convert` to translate any template between HTML and Hiccup syntax: `presemble convert templates/post/item.html` produces `templates/post/item.hiccup` and vice versa. Both formats are first-class — the publisher accepts either without configuration.

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

### Hiccup syntax

Templates can be written in Hiccup (EDN) format instead of HTML. Use `presemble convert` to translate between the two. Hiccup templates support `;` line comments, which are stripped at parse time:

```clojure
; Render title as h1
[:presemble/insert {:data "post.title" :as "h1"}]
```

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

The publisher reads every content file, validates it against its schema, resolves the template, and writes output to a sibling `output/<site-dir-name>/` directory. Each content file becomes a clean URL:

```
my-site/content/post/my-post.md  →  output/my-site/post/my-post/index.html  →  /post/my-post
my-site/content/docs/index.md    →  output/my-site/docs/index.html           →  /docs/
```

Output lives outside the source tree so the file watcher never sees its own writes. Only assets referenced by templates are copied to `output/assets/`. Unreferenced files are reported as warnings.

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

### Live reload

When a file changes and the rebuild completes, the browser reloads automatically — no manual refresh needed. If the changed page is different from the one currently open, the browser navigates directly to the changed page. If multiple pages changed, the browser navigates to the first one.

### Suggestion nodes

In serve mode, missing or invalid content slots render as inline suggestion nodes instead of error pages. Each suggestion node is derived from the schema: it shows the slot's hint text and indicates what is expected. The page is always browsable — a content file with no fields renders as a fully scaffolded guide. Suggestion nodes never appear in a published build; `presemble build` still fails on missing required slots.

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

## Editor support (LSP)

```
presemble lsp <site-dir>
```

Starts an LSP server over stdio. A single server process handles content, template, and schema files, dispatching by path. Point any LSP-capable editor at the `presemble lsp` binary.

### Helix setup

Add to `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "markdown"
language-servers = ["presemble-lsp"]

[language-server.presemble-lsp]
command = "presemble"
args = ["lsp", "site/"]
```

Replace `site/` with the path to your site directory.

### Content file capabilities

| Capability | Description |
|---|---|
| Completions | Slot names from the schema; for link slots, actual content files formatted as `[Title](/type/slug)` |
| Diagnostics | Schema violations — missing slots, occurrence counts, capitalization, broken link references |
| Hover | Schema hint text for the slot at the cursor |
| Go-to-definition | Navigates to the linked content file |
| Code actions | Quickfix: capitalize first letter; insert missing slot snippet |

### Template file capabilities

| Capability | Description |
|---|---|
| Completions | Data-path completions for `data="…"` attributes, derived from the matching schema |
| Diagnostics | Data paths referencing fields not declared in the schema |
| Hover | Schema hint text for the field at the cursor |
| Go-to-definition | Jumps to `presemble:include` target or `presemble:define` block |

### Schema file capabilities

| Capability | Description |
|---|---|
| Completions | Element keyword syntax (heading, paragraph, link, image); constraint key/value pairs |
| Diagnostics | Parse errors at the failing line |

### File-type dispatch

The server classifies each file by its path within the site directory:

| Prefix | Kind | Capabilities |
|---|---|---|
| `content/` | Content | All |
| `templates/` | Template | Completions, diagnostics, hover, go-to-definition |
| `schemas/` | Schema | Completions, diagnostics |

Files outside these prefixes receive no diagnostics.

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
