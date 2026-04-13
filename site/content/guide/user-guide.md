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

#### List slots

```markdown
- hint text {#tags}
occurs
: *
```

A markdown list item ending with a `{#name}` anchor declares a list slot. The occurrence value `*` means unbounded — any number of items. In a content file, write a standard markdown list:

```markdown
- Rust
- WebAssembly
- Publishing
```

In templates, iterate the list with `data-each`; each item exposes a `text` field. To render all items joined with spaces, use `:apply text` directly on the insert:

```html
<presemble:insert data="input.tags" apply="text" />
```

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
| `*` | Unbounded — any number of items (used for list slots) |

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
<presemble:insert data="input.title" as="h1" />
<presemble:insert data="input.summary" as="p" />
<presemble:insert data="input.body" />
```

`input` always refers to the current page's own content. The name can be changed with the `:input` directive (see [Data context](#data-context)).

| Attribute | Required | Description |
|---|---|---|
| `data` | yes | Dot-separated path into the data graph |
| `as` | no | Wrap the value in this element (e.g. `h1`, `p`, `span`) |
| `apply` | no | Transform the value before rendering (see below) |

Omitting `as` inserts the value as raw HTML nodes. If the path is absent from the data graph, the element is removed silently.

#### `:apply` — value transforms

The `apply` attribute (`:apply` in Hiccup) transforms a value before it is inserted. Use `text` to render the Display (text) representation of any value:

```html
<presemble:insert data="input.title" as="h1" apply="text" />
```

In Hiccup:

```clojure
[:presemble/insert {:data "input.title" :as "h1" :apply "text"}]
```

To thread a value through multiple transforms, use a pipe expression:

```html
<presemble:insert data="input.title" apply="(-> text to_lower capitalize)" />
```

In Hiccup, the expression is a list (no quoting needed):

```clojure
[:presemble/insert {:data "input.title" :apply (-> text to_lower capitalize)}]
```

**Available functions:**

| Function | Effect |
|---|---|
| `text` | Render as plain text (Display representation) |
| `to_lower` | Convert to lowercase |
| `to_upper` | Convert to uppercase |
| `capitalize` | Uppercase the first character |
| `truncate` | Truncate to a default length |

`:apply text` on a list field joins all items with spaces.

#### Anchor wrapping for link records

When the `data` path resolves to a link record (a slot declared as a link in the schema), the `as` attribute wraps the linked text in the given element and the whole thing is wrapped in an `<a>` pointing to the linked page:

```html
<presemble:insert data="input.author" as="h3" />
```

Produces:

```html
<a href="/author/johlrogge"><h3>Joakim Ohlrogge</h3></a>
```

In Hiccup:

```clojure
[:presemble/insert {:data "input.author" :as "h3"}]
```

### `data-each`

Iterate over a collection on a `<template>` element. The value passed to `data-each` is the singular schema stem — the publisher automatically finds all items of that type:

```html
<template data-each="post">
  <li>
    <presemble:insert data="item.title" as="h3" />
    <a data-href="item.url_path" href="">Read more</a>
  </li>
</template>
```

The `<template>` wrapper is not emitted. Inside the loop, each item is bound to `item`. The parent context — `input`, other collections, and outer loops — remains accessible inside the loop body.

The item binding name can be customised with the `:item` directive:

```html
<template data-each="post" :item "p">
  <presemble:insert data="p.title" as="h3" />
</template>
```

Every page template has access to all collections regardless of content type, so a `post` item template can iterate all `guide` items and vice versa.

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
<a data-href="input.url_path">Read more</a>
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
[:presemble/insert {:data "input.title" :as "h1"}]
```

#### EDN attribute types

Hiccup attribute values are not limited to strings — the full EDN type system is available. Symbols, lists, sets, integers, and keywords are all valid attribute values and are interpreted by the directive that reads them:

```clojure
; Symbol — used as a bare function reference in :apply
[:presemble/insert {:data "input.title" :apply text}]

; List — used as a pipe expression in :apply
[:presemble/insert {:data "input.title" :apply (-> text to_lower capitalize)}]

; Keyword — used as an enum value
[:presemble/insert {:data "input.title" :as :h1}]

; Integer
[:div {:tabindex 0}]
```

This is a key difference from HTML templates, where all attribute values are strings. The HTML equivalent of a pipe expression must quote the list as a string:

```html
<presemble:insert data="input.title" apply="(-> text to_lower capitalize)" />
```

### URL paths in templates

Always write root-relative paths in templates. The publisher rewrites them at build time:

```html
<link rel="stylesheet" href="/assets/style.css">
<a href="/feature/schemas-as-contracts">Schemas</a>
```

See [URL rewriting](#url-rewriting) for deployment configuration.

## The data graph

Schemas, content, and templates connect through named data paths. Templates traverse these paths to build output.

### Data context

Each template receives a data context with two reserved names:

- **`input`** — the current page's own content. A post template rendering `my-post.md` sees that post's data as `input.title`, `input.summary`, etc.
- **`item`** — the current loop item inside a `data-each` loop.

Both names can be customised:

- `:input "article"` — rename `input` to `article` for the current template. All paths become `article.title`, `article.summary`, etc.
- `:item "p"` on a `data-each` element — rename `item` to `p` inside that loop.

These directives only change the binding name; the underlying data is the same.

### Path structure

| Path form | Example | Description |
|---|---|---|
| `input.field` | `input.title` | A named slot from the current page |
| `input.field.subfield` | `input.author.name` | A nested reference (linked content) |
| `input.url_path` | `input.url_path` | The current page's clean URL |
| `input.body` | `input.body` | The current page's free-form body as HTML |
| `item.field` | `item.title` | A named slot from the current loop item |
| `item.url_path` | `item.url_path` | The loop item's clean URL |

### Collections

Collections are accessed by the singular schema stem in `data-each`. A schema named `post` is iterated as `data-each="post"`:

```html
<template data-each="post">
  <presemble:insert data="item.title" as="h2" />
</template>
```

Every page template sees all collections — a guide page can iterate all posts, and a post page can iterate all guides. Loops extend the parent context, so `input` and other collections remain accessible inside a loop.

Because the schema defines which paths exist, the template vocabulary is finite and verified at build time. A template that references a non-existent path fails the build.

### Stylesheet tracking

CSS files are first-class nodes in the dependency graph. The publisher parses `@import` statements and `url()` references within each stylesheet to build dependency edges. This means:

- A change to an imported CSS file triggers a rebuild of all pages that (directly or transitively) use the importing stylesheet.
- `url()` asset references in CSS are tracked the same way as `src` and `href` in HTML — missing assets are reported at build time.
- The serve watcher detects `.css` changes and rebuilds only the affected pages.

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

Suggestion nodes are interactive: clicking one in the browser opens an inline editing form for that slot. This is Phase A of the browser editing feature (M5). The suggestion UI has dedicated CSS polish to distinguish editing state from normal content.

### Mascot overlay

The serve UI includes a mascot overlay in the corner of every served page. It indicates the current editorial state at a glance:

| State | Indicator | Meaning |
|---|---|---|
| Suggestions present | Mascot + badge | One or more suggestion nodes exist on this page |
| All clear | Thumbs up | No suggestions — all slots are filled |
| Edit mode | Pencil | Inline editing is active |

Clicking the mascot opens a popover menu with three mode options: View, Edit, and Suggest.

### Inline body editing

In Edit mode, click any rendered body element to open an inline textarea for that element. The textarea contains the raw markdown source. Save closes the textarea and triggers a live rebuild. The updated content appears in the browser within a second.

### Header folding in edit mode

In Edit mode, headings in the served page display a fold toggle. Click the toggle to collapse or expand the section beneath that heading. Two toolbar buttons collapse all sections or expand them all at once. Clicking anywhere inside a collapsed section unfolds it. Fold state is not persisted across page reloads.

### Suggest mode

In Suggest mode, missing slots render as suggestion nodes guided by schema hint text. Click a suggestion node to open an editing form for that slot. When a collaborator or Claude pushes a suggestion via the MCP server or conductor API, the browser shows the proposed value as an inline diff alongside the current content. A toolbar offers "Accept all" and "Reject all"; individual suggestions can be accepted or rejected from the diff view.

Suggestion UI is limited to Suggest mode. Edit mode no longer shows suggestion overlays — the two modes are kept visually separate. Suggestion markers and a speech bubble icon accompany each pending suggestion node to make them easy to locate on a busy page.

The preview toggle switches between the current state and a preview of the page with all suggestions applied.

### Slot-scoped suggestions (SlotEdit)

The `SuggestSlotEdit` command targets a specific slot with a search/replace operation rather than replacing the slot's entire value. This is distinct from full-slot suggestions — it makes targeted edits within a slot's content (for example, correcting one sentence in a long summary without replacing the rest). The conductor handles both suggestion kinds; they appear as separate diagnostic entries in the LSP and as separate nodes in the browser diff view.

### Creating new content from the browser

The "+" button in the serve toolbar opens a form to create a new content file. Select a content type, enter a slug, and submit. The conductor scaffolds the file and the browser navigates to the new page immediately, with all required slots present as suggestion nodes.

### Dirty buffer tracking

Edits made in the browser and accepted suggestions are held in the conductor's dirty buffer until explicitly saved. The mascot badge shows unsaved changes. Save writes the dirty buffer to disk and clears it. This lets you review several changes before committing any of them to the filesystem.

After a browser edit triggers a rebuild, the conductor resolves link expressions and cross-content references in the rebuilt page. Feature cards, author links, and any content that depends on linked documents render correctly without restarting the server.

### File watcher coverage

The serve watcher monitors all source file types that affect the build. Changes to any of the following trigger an incremental rebuild and browser reload:

| Extension | What changes |
|---|---|
| `.md` | Content files and schemas |
| `.html` | HTML templates |
| `.hiccup` | Hiccup templates |
| `.css` | Stylesheets tracked as graph nodes |

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

The LSP requires a conductor daemon. If `presemble serve` is already running for the site, the LSP connects to its conductor automatically. If no conductor is running, the LSP starts one on the first incoming request. There is no standalone mode — all classify, grammar, completions, and document-text operations go through the conductor.

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
| Completions | Data-path completions for `data="…"` attributes: `input.*` paths from the matching schema, `item.*` paths inside `data-each` loops, and collection names for `data-each` values |
| Diagnostics | `input.*` or `item.*` paths referencing fields not declared in the schema |
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

## Template composition

Templates are function files. A template file has two parts: optional local definitions at the top and a composition expression at the bottom. The composition expression is the template's return value — a DOM tree.

### juxt

`juxt` fans the same input to multiple templates and concatenates their DOM outputs in order:

```clojure
((juxt header self/body footer) input)
```

`header`, `self/body`, and `footer` all receive the full content tree. Their outputs are assembled into a single page DOM. `self/body` refers to the body of the current template file (the local definitions section, referenced as a named fragment).

File-qualified template references use `/` notation: `/fragments/structure#header` refers to the `header` definition inside `templates/fragments/structure.hiccup`. Unqualified references look up local definitions first, then the template file system.

### Pipe

`->` threads a value through a sequence of transforms as the first argument:

```clojure
(-> input :title upcase)
```

`->>` threads as the last argument — the standard idiom for collection pipelines:

```clojure
(->> input :tags (map :text) (str/join ", "))
```

### Local definitions

Local definitions at the top of a template file are reusable fragments:

```clojure
[:template "byline"
  [:p.byline
    [:presemble/insert {:data "input.author" :as "span"}]]]

((juxt /header self/byline self/body /footer) input)
```

The local `byline` fragment is referenced as `self/byline` in the composition expression.

## Link expressions in content

Content files can include link expressions that assemble collections at build time. A link expression is a parenthesised threading form inside a link:

```markdown
[]((->> :post (sort-by :published :desc) (take 5)))
```

The expression evaluates against the site graph. The result is a validated list satisfying the collection schema for that type. The template receives the assembled list ready to iterate.

Link expressions make content the place where assembly decisions live. The homepage content file decides which collections appear and in what order; the template decides how they look.

### Expression syntax

Expressions use Presemble Lisp — a small EDN-based language with threading macros and built-in functions for the operations content assembly needs:

| Expression | Effect |
|---|---|
| `(->> :post (sort-by :published :desc) (take 5))` | Latest 5 posts |
| `(->> :feature (sort-by :title :asc))` | Features sorted by title |
| `(->> :post (filter #(= :pinned (:status %))))` | Pinned posts only |
| `(->> :post (refs-to self))` | All posts that link to the current page |

Keywords act as accessor functions: `:title item` extracts the `:title` field from `item`.

### Reverse references with `refs-to self`

Any content file can populate a slot with all pages that link to it by using `(refs-to self)` in a link expression:

```markdown
[posts](->> :post (refs-to self))
```

This queries the site graph's edge index at build time and returns all posts whose link slots point to the current page's URL. The schema for the receiving page declares the slot with a link type and unbounded occurrence:

```markdown
[<post>](/post/<slug>) {#posts}
type
: link(post)
occurs
: *
```

The result is a typed list — the template iterates it exactly like any other collection. This replaces any need to maintain reverse-reference lists by hand.

## MCP server

```
presemble mcp <site-dir>
```

Starts an MCP server that exposes the site to Claude Code and other MCP-capable clients. The server provides tools for reading and modifying site content through the editorial suggestion protocol.

### Available tools

| Tool | Description |
|---|---|
| `get_content` | Read a content file by type and slug |
| `get_schema` | Read the schema for a content type |
| `list_content` | List all content files for a type (goes through conductor `ListContent`) |
| `suggest` | Push a suggestion for a named slot in a content file |

Each tool accepts an optional `site` parameter. Pass the site directory path to target a specific site when Claude has the MCP server configured globally rather than per-project.

### Workflow with Claude Code

1. Start `presemble mcp site/` in a terminal
2. Add the server to Claude Code's MCP configuration
3. Ask Claude to review and improve content. Claude reads schemas to understand the content model, reads content files to understand what exists, and pushes suggestions for specific slots with rationale.
4. Each suggestion appears as an LSP diagnostic in your editor. Accept or reject with a code action. The suggestion also appears as an inline diff in the browser preview.

Claude uses the same suggestion API as a human editor. There is no special path for AI collaboration.

## nREPL

```
presemble nrepl <site-dir>
```

Starts an nREPL server that Calva, CIDER, or `rep` can connect to. Evaluate Presemble Lisp expressions against the live content graph interactively.

### Connecting with rep

```
rep '(->> :post (sort-by :published :desc) (take 3))'
```

### Connecting with Calva

Run "Connect to a Running REPL Server" in VS Code and select nREPL. The default port is printed when the server starts.

### Connecting with CIDER

`M-x cider-connect` in Emacs.

### What you can do in the REPL

- Evaluate collection queries and inspect results
- Call suggestion operations programmatically
- Inspect the site graph's data model
- Prototype link expressions before adding them to content files
- Query the edge graph with `refs-to` and `refs-from`

### Edge queries

Two built-ins query the site's link graph directly:

| Expression | Returns |
|---|---|
| `(refs-to "/author/alice")` | All edges pointing to `/author/alice` |
| `(refs-from "/post/hello")` | All edges originating from `/post/hello` |

Each result is a list of edge records with `:source` and `:target` keys. Use these to explore cross-content relationships interactively before building them into content files:

```clojure
(->> (refs-to "/author/alice") (map :source))
```

## TUI REPL

```
presemble repl
presemble repl --port 1667
```

Opens a full-screen terminal REPL with three panels: output history, a doc panel, and an input editor.

**Auto-discovery:** with no port flag, the REPL walks the current directory and its parents looking for a `.nrepl-port` file. If a running conductor is found it connects automatically in connected mode. If no conductor is running it starts in standalone mode — language primitives and prelude functions work fully; site-specific operations (e.g. `query`, `get-content`) return informative errors.

### Key bindings

| Key | Action |
|---|---|
| Enter | Eval when delimiters are balanced; insert newline otherwise |
| Ctrl+J | Force-eval regardless of balance |
| Ctrl+O | Force-insert newline |
| Tab | Trigger completion popup |
| Up / Down | Navigate completion popup (when open) or command history |
| Esc | Dismiss completion popup |
| Ctrl+L | Clear output panel |
| Ctrl+D | Quit |

### Features

**EDN syntax highlighting** colours keywords (green), strings (yellow), numbers (magenta), and brackets and comments (dim).

**Completion popup:** Tab on a partial symbol shows matching completions with inline doc hints. Enter accepts the selected candidate. The same completions are available whether the backend is standalone or connected — in connected mode they reflect the live conductor's symbol registry.

**Doc panel:** updates automatically as you type. Shows the arglists and full doc string for the symbol immediately before the cursor.

**Delimiter balancing:** Enter only evaluates when all `(`, `[`, `{` are closed and no string literal is left open (respecting `"…\"…"` escaping and `;` line comments). Use Ctrl+J to evaluate regardless.

**Command history:** Up/Down navigate previous expressions when the completion popup is not open.

## Site wizard

Point `presemble serve` at an empty directory and the browser opens a guided setup wizard. Six steps take you from zero to a fully styled, working site.

### Steps

| Step | What you choose |
|---|---|
| Site type | Blog, personal site, or portfolio |
| Font mood | One of 7 curated type pairings |
| Color seed | A hue in degrees (0–360) |
| Palette type | Analogous, complementary, or triadic |
| Complexity | Sparse to rich |
| Template format | Hiccup (EDN) or HTML |

A live CSS preview panel updates on every step. The color step includes a light/dark theme toggle.

### Generated stylesheet

The wizard generates a `StyleConfig` from your choices and passes it to the CSS generator, which produces a complete custom-property stylesheet using HSL color math. The stylesheet covers typography, color system, spacing, layout, and a `prefers-color-scheme` light/dark block. It is written to `assets/style.css` in the scaffolded site.

### Starter templates

| Template | Contents |
|---|---|
| Blog | Post schema, author schema, posts index, homepage |
| Personal | About page, page schema, homepage |
| Portfolio | Project schema with image slots, projects index, homepage |

Each starter includes schemas, seed content, Hiccup templates, navigation partials, and the generated stylesheet.

### Navigation and index pages

Every starter includes shared navigation via `presemble:include` and breadcrumb navigation on item pages. Collection index pages list all items of that type. No page is a dead end.

### After scaffolding

The wizard writes files to disk and the file watcher picks them up immediately. Edit content files, see changes in the browser, use the LSP for completions and diagnostics. The normal serve workflow applies from the first page load.

`presemble init <dir>` is still available for scripted setups — it produces a minimal hello-world `note` site without the browser wizard.
