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

In templates, iterate the list with `data-each`; each item is its content: insert `item` to place it, or call `(text item)` for its plain text. To place all items joined with spaces, call `text` on the list (see [Expressions](#expressions)):

```html
<presemble:insert data="(text input.tags)" />
```

This places:

```output
Rust WebAssembly Publishing
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

Any text line ending with `{#name}` is a paragraph slot (in schema files; in content see *Choosing the slot with a slot marker*). The text before the anchor is shown as a hint.

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

### Preamble and body

The preamble (above `----`) only holds what the schema's slots allow, in order. Anything else there is kept and saved but not shown, and stops the site from publishing (see [Publishing gate](#publishing-gate)); on save it moves to just above `----`. The body below `----` is free content. To allow free paragraphs near the top, declare a paragraph slot with `occurs: *`.

### Body content types

The body section supports the full range of markdown block and inline content:

**Block elements:**

| Element | Syntax |
|---|---|
| Paragraph | Plain text |
| Heading | `##`, `###`, … (within the schema's `headings` constraint) |
| Blockquote | `> quoted text`; it can hold any blocks, nested quotes included |
| Unordered list | `- item` |
| Ordered list | `1. item` |
| Code block | Triple-backtick fence |

List items can hold any blocks, nested lists and quotes included. Presemble writes list markers as `-` and `N.`, counting from the list's start, and keeps a list tight or loose as it was written.

Two lists of the same kind written one after the other would merge in markdown. Presemble keeps them apart by writing `<!-- -->` between them, and reads that comment back as the separator, not as a comment.

**Inline formatting within paragraphs and headings:**

| Format | Syntax |
|---|---|
| Bold | `**text**` or `__text__` |
| Italic | `*text*` or `_text_` |
| Inline code | `` `code` `` |

Headings keep their markdown as written, so `` ## The `<div>` element `` is saved as it was typed. Underlined (setext) headings, a line of text with `===` or `---` below it, are rewritten as `#` headings when the file is saved; until then they cannot be suggested on in the browser.

In a table, a row with more cells than the header keeps them: the header is padded with empty columns when the file is saved, so no cell is lost.

### HTML and comments in content

Presemble keeps content free of HTML, even though markdown allows it, like Hugo does by default (goldmark's `unsafe = false`). Presentation belongs in templates.

- **HTML blocks** (a line that starts with a tag such as `<div>`) are kept in the file, but the page cannot be published until they are removed. In `presemble serve` a placeholder shows where the block is.
- **Inline HTML** in paragraphs, headings and lists (such as `<b>bold</b>` inside a sentence) also stops the page from publishing.
- **In blockquotes**, HTML is treated as it is anywhere else: a blockquote holds any blocks (paragraphs, headings, code, lists, tables and nested quotes), and HTML in one stops the page from publishing, like HTML in a paragraph. Comments inside a blockquote are kept. A link expression inside a blockquote has no effect.
- **The fix:** delete the HTML, write `\<` for a literal `<`, or put code in backticks (`` `<div>` ``).
- **HTML comments** (`<!-- … -->`) are kept in the file, move with the block that follows them, and are never published.
- **At the start of a line** inside a paragraph, HTML or a comment is kept as it is: Presemble writes that line indented by four spaces, so it stays part of the paragraph.

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

### Expressions

A template is a pure function of its page's content: it reads the content and sets nothing. Every place where a template reads the content takes an **expression**: the `data` of `presemble:insert`, `data-each`, `data-slot`, any attribute value, and a call written as a child. There is one expression language, and it is the same in HTML and in Hiccup.

#### `ref`

`(ref input.title)` is the value at a path. A path is dotted and starts at `input` (the current page's own content), at `item` (the current loop item, see [`data-each`](#data-each)), or at a collection name under `input` (`input.post`). `input` always refers to the current page.

In HTML, an expression is an attribute value:

```html
<presemble:insert data="(ref input.title)" as="h1" />
<presemble:insert data="(ref input.summary)" as="p" />
<presemble:insert data="(ref input.body)" />
```

In Hiccup the same expression is written without quotes, and a bare symbol is shorthand for `(ref symbol)`:

```clojure
[:presemble/insert {:data input.title :as "h1"}]
[:presemble/insert {:data input.summary :as "p"}]
[:presemble/insert {:data input.body}]
```

#### Calls

A list is a call. The functions are pure, and the first argument is the value they work on:

| Function | Effect |
|---|---|
| `ref` | The value at a path (or the literal itself) |
| `text` | Plain text (Display representation); a list of values is joined with spaces |
| `to_lower` | Lowercase |
| `to_upper` | Uppercase |
| `capitalize` | Uppercase the first character |
| `truncate` | Truncate to a default length |
| `juxt` | Several templates side by side, used as `((juxt a b) input)`, see [Template composition](#template-composition) |
| `->` | Thread a value through functions: `(-> input.title text to_lower capitalize)` |

```html
<presemble:insert data="(-> input.title text to_lower capitalize)" as="h1" />
```

```clojure
[:presemble/insert {:data (-> input.title text to_lower capitalize) :as "h1"}]
```

In Hiccup a list written as a child, on its own, is an insert of that call: `[:h1 (text input.title)]` is `[:h1 [:presemble/insert {:data (text input.title)}]]`.

A template is a function too. Calling one is covered in [Calling templates](#calling-templates).

#### Literals

A string, an integer or a keyword is a literal; `(ref "text")` is the string itself. A literal is rarely needed except to write text that would otherwise be read as an expression, see [HTML attributes](#html-attributes).

#### Attributes

An expression is also valid as the value of any attribute of an ordinary element. It reads the content and gives plain text:

```html
<a href="(ref item.link.href)" title="(ref input.title)">Read more</a>
```

```clojure
[:a {:href item.link.href :title input.title} "Read more"]
```

- Text, headings and paragraphs give their plain text, never markup; the serializer escapes it like any literal.
- An integer gives its decimal text.
- A value that is absent gives nothing, and the attribute is left out rather than written empty: `<a href="(ref input.homepage)">x</a>` is `<a>x</a>` when the page has no homepage.
- A record or a list cannot be an attribute value and is an error. Name the field: `item.link.href`, `item.cover.src`, not `item.link`.

#### HTML attributes

In HTML an attribute value that reads as an EDN list is an expression; any other value is literal text. A literal text that starts with `(` and would read as a list is written as an expression that gives the text:

```html
<a title='(ref "(draft)")'>Notes</a>
```

This gives `<a title="(draft)">Notes</a>`. The checker warns about a value that starts with `(` but does not read as a list.

#### Placing values

An insert places a value in the page. Without `as`, the value is placed as it is: text as text, inline and node content unchanged, the output of a template as it is, and nothing at all when the value is absent. Nothing is added around it:

```html
<h1><presemble:insert data="(ref input.title)" /></h1>
```

This places:

```output
<h1>Hello</h1>
```

With `as`, the value becomes an element of that tag, carrying the semantic class of its slot (`input-title`) and, when the server renders for editing, what the browser needs to edit it in place:

```html
<presemble:insert data="(ref input.title)" as="h1" />
```

So `as` is the editable form. A value placed without `as` has no editor attributes and cannot be edited in place in the browser; use `as` for content that authors should edit on the page.

#### A link with `as`

A link slot is a record. Inserted whole it is its own element, an `<a>`, whether or not there is an `as`. With `as`, the linked text is wrapped in that element, and the whole thing in an `<a>` pointing to the linked page:

```html
<presemble:insert data="(ref input.author)" as="h3" />
```

Places (the semantic class and data attributes are left out here):

```html
<a href="/author/jo"><h3>Jo</h3></a>
```

In Hiccup:

```clojure
[:presemble/insert {:data input.author :as "h3"}]
```

#### Link and image records

A link slot or an image slot gives the template a record:

| Slot | Fields |
|---|---|
| Link | `href`: where the link points; `text`: the link text |
| Image | `src`: the image source; `alt`: the alt text (absent when the image has none) |

Insert the whole record to render the link or image, or a single field to insert its text on its own:

```html
<presemble:insert data="(ref input.cover)" />
<presemble:insert data="(ref input.cover.alt)" as="figcaption" />
```

The first line renders `<img src="…" alt="…">`. The second inserts only the alt text, wrapped in a `<figcaption>`; like any absent value, it is removed when the image has no alt text.

### `data-each`

Iterate on a `<template>` element. The value of `data-each` is an expression that gives a list. A collection is the list of all the items of a content type, in URL order, found under `input` by the singular schema stem:

```html
<template data-each="(ref input.post)">
  <li>
    <presemble:insert data="(ref item.title)" as="h3" />
    <a href="(ref item.link.href)">Read more</a>
  </li>
</template>
```

In Hiccup:

```clojure
[:template {:data-each input.post}
  [:li
    [:presemble/insert {:data item.title :as "h3"}]
    [:a {:href item.link.href} "Read more"]]]
```

The `<template>` wrapper is not emitted. Inside the loop, each item is bound to `item`. The parent context — `input`, other collections, and outer loops — remains accessible inside the loop body. The "Read more" link takes its `href` from the item's link record (`item.link` is the link every item has, with `href` and `text`).

The item binding name can be changed with the `item` attribute (`:item` in Hiccup):

```html
<template data-each="(ref input.post)" item="p">
  <presemble:insert data="(ref p.title)" as="h3" />
</template>
```

Every page template has access to all collections regardless of content type, so a `post` item template can iterate all `guide` items and vice versa.

### `data-slot`

A `<template>` with `data-slot` renders its content only when the expression has a value, so a block is dropped when a slot is empty:

```html
<template data-slot="(ref input.cover)">
  <figure><presemble:insert data="(ref input.cover)" /></figure>
</template>
```

### Calling templates

A template is a function of its input, and it is used by calling it. Insert the call; in HTML that is an insert of a call, and in Hiccup a list as a child:

```html
<div class="top">
  <presemble:insert data="(header input)" />
</div>
```

```clojure
[:body
  (header input)
  [:main [:presemble/insert {:data input.body}]]]
```

`(header input)` renders the template `header` with the page's content as its `input`. A call takes one argument, and that value is bound as `input` inside the called template; an argument of `input` itself leaves the content unchanged. Inside a loop, call a template with the item:

```html
<template data-each="(ref input.post)">
  <presemble:insert data="(card item)" />
</template>
```

Here `card` sees the item as its `input`. A call with an argument that is absent places nothing. A call of a template that does not exist is an error: the checker reports it in the editor, and the page does not render.

The name is a template of the site (`templates/header.html`, `templates/header.hiccup`, `templates/card/item.hiccup`), or a local definition of the same file written `self/name` (see [Template composition](#template-composition)).

### Hiccup syntax

Templates can be written in Hiccup (EDN) format instead of HTML. Use `presemble convert` to translate between the two: it writes the bare-symbol shorthand when it outputs Hiccup, and `(ref …)` when it outputs HTML. Hiccup templates support `;` line comments, which are stripped at parse time:

```clojure
; Place the title as h1
[:presemble/insert {:data input.title :as "h1"}]
```

#### EDN attribute types

Hiccup attribute values are not limited to strings — the full EDN type system is available. Symbols, lists, integers, and keywords are all valid attribute values; a symbol or a list is an expression, and a string is literal text:

```clojure
; Symbol: a reference to the content
[:a {:href item.link.href} "Read more"]

; List: a call
[:presemble/insert {:data (-> input.title text to_lower capitalize) :as "h1"}]

; String: literal text
[:a {:title "(draft)"} "Notes"]

; Integer
[:div {:tabindex 0}]
```

This is a difference from HTML templates, where all attribute values are strings. In HTML an attribute value is an expression when it reads as a list; see [HTML attributes](#html-attributes).

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

`input` is the one name every template reads its page from. The `item` name can be changed on a `data-each` element with the `item` attribute (`item="p"` renames `item` to `p` inside that loop); the underlying data is the same. A template that is called receives its argument as `input`.

### Path structure

| Path form | Example | Description |
|---|---|---|
| `input.field` | `input.title` | A named slot from the current page; for a heading slot such as `input.title`, the heading's plain text (markup removed) |
| `input.field.subfield` | `input.author.name` | A nested reference (linked content) |
| `input.url` | `input.url` | The current page's clean URL |
| `input.link` | `input.link.href` | A link to the current page, a record with `href` and `text` |
| `input.body` | `input.body` | The current page's free-form body |
| `input.collection` | `input.post` | All the items of a content type, as a list |
| `item.field` | `item.title` | A named slot from the current loop item |
| `item.link` | `item.link.href` | A link to the loop item's page, a record with `href` and `text` |

### Collections

Collections are found under `input` by the singular schema stem. A schema named `post` is iterated as `data-each="(ref input.post)"`:

```html
<template data-each="(ref input.post)">
  <presemble:insert data="(ref item.title)" as="h2" />
</template>
```

Every page template sees all collections — a guide page can iterate all posts, and a post page can iterate all guides. Loops extend the parent context, so `input` and other collections remain accessible inside a loop. Items are listed in URL order.

Because the schema defines which paths exist, the template vocabulary is finite, and the checker verifies it: in the editor (the LSP) a path that does not exist in the schema is reported, `input.post.title` and `item.title` inside a loop over `input.post` are checked against the `post` schema. At build time a path that resolves to nothing places nothing.

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

### Publishing gate

Before anything is written, `presemble build` validates every schema and content file. If any of them has an error, nothing is written (no pages, no assets, and output from an earlier build is left as it was) and the build exits with code 1. Every problem is printed as one line, with the path relative to the site directory:

```
content/post/my-post.md: error: slot 'title': missing required element
1 validation error(s) in 1 file(s); site not published
```

Warnings are printed too, but they don't fail the build. A schema that fails to parse is an error, and so is every page that uses it ("not built: schema `schemas/post/item.md` does not parse"). Fix the errors and build again; `presemble serve` shows the same pages while you work on them. HTML in content is one such error; see [HTML and comments in content](#html-and-comments-in-content).

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
presemble serve <site-dir> --dev-session
```

Starts a local server on port 3000. The server watches `schemas/`, `content/`, and `templates/` for changes and rebuilds affected pages incrementally. Validation errors and warnings for the whole site are reported to the terminal at startup; pages still render. The editor's browser also shows each page's own errors (see [Seeing a page's errors](#seeing-a-pages-errors)); reporting them in the terminal on every rebuild is future work. Plain `presemble serve` is preview-only. To edit in the browser (Edit and Suggest modes, creating content, the site wizard), start the server with `--dev-session` and open the Login URL it prints. Without the flag the editor shows "Editing is disabled on this server. Restart presemble serve with --dev-session to enable it."

### What readers and editors see

The server keeps two outputs. Editors, who have a session, see the working copy: it includes unsaved edits, the editing controls, and error pages for pages that fail to build. Everyone else sees the published site: what `presemble build` would produce from the files saved on disk, with no editing controls. A page that is not published is a plain 404 for readers.

Previewing drafts therefore needs a session. Start the server with `--dev-session` and open the Login URL it prints; plain `presemble serve` shows the published site.

The published site is rebuilt at start-up, after every save, and after any change to a file on disk. While any page fails the publish gate, readers keep seeing the last published version; once the site passes again, they see the new one. Unsaved edits never reach readers.

While serving, the published version lives in a versioned directory beside the output directory (`output/<site>.publishing-<n>`), which the server swaps for the next one in a single step, so there is no moment without a page. `output/<site>/` is written only by `presemble build`.

### Live reload

When a file changes and the rebuild completes, the editor's browser reloads automatically — no manual refresh needed. If the changed page is different from the one currently open, the browser navigates directly to the changed page. If multiple pages changed, the browser navigates to the first one.

A reader's browser reloads only when the published site changes: after a save, or after a file changes on disk. An edit that has not been saved does not reload it.

### Suggestion nodes

In serve mode, missing or invalid content slots render as inline suggestion nodes instead of error pages. Each suggestion node is derived from the schema: it shows the slot's hint text and indicates what is expected. The page is always browsable — a content file with no fields renders as a fully scaffolded guide. Suggestion nodes never appear in a published build; `presemble build` still fails on missing required slots.

When the server runs with `--dev-session`, suggestion nodes are interactive: clicking one in the browser opens an inline editing form for that slot. This is Phase A of the browser editing feature (M5). The suggestion UI has dedicated CSS polish to distinguish editing state from normal content.

### Seeing a page's errors

In the editorial view (a session started with `--dev-session`), a page shows its own validation problems where they arise. Errors and warnings are outlined on the element they belong to: red for an error, amber for a warning, and red when an element has both. A small badge on the outlined element shows how many problems it has; hover over it or click it to read the messages. A paragraph with inline HTML is outlined on that paragraph, and a list item or a quote with inline HTML is outlined on that item or quote child, not on the whole list. A missing required heading is outlined on its placeholder.

Some problems have no place on the page, such as content outside any slot of the schema. These are listed in a panel at the bottom left of the page, under a summary such as "1 error on this page. The site can't be published until it's fixed." Warnings are counted separately. Click the panel's heading to collapse it. A page with neither errors nor warnings has no panel and no outlines.

The site is published only when no page has an error, so a single outlined element anywhere keeps readers on the last published version. Every page can still be previewed with its errors in the editorial view, so you can find and fix them there. When you fix the problem in the browser editor or in your text editor, the page reloads and the outline and the panel update without a manual refresh. Readers never see any of this: the published pages carry no outlines, badges or panel.

### Mascot overlay

The serve UI includes a mascot overlay in the corner of every served page. It indicates the current editorial state at a glance:

| State | Indicator | Meaning |
|---|---|---|
| Suggestions present | Mascot + badge | One or more suggestion nodes exist on this page |
| All clear | Thumbs up | No suggestions — all slots are filled |
| Edit mode | Pencil | Inline editing is active |

Clicking the mascot opens a popover menu with three mode options: View, Edit, and Suggest. Edit and Suggest require `presemble serve <site-dir> --dev-session`; open the Login URL it prints to sign in.

### Inline body editing

In Edit mode, click any rendered body element to open an inline textarea for that element. The textarea contains the raw markdown source. Save closes the textarea and triggers a live rebuild. The updated content appears in the browser within a second.

### Header folding in edit mode

In Edit mode, headings in the served page display a fold toggle. Click the toggle to collapse or expand the section beneath that heading. Two toolbar buttons collapse all sections or expand them all at once. Clicking anywhere inside a collapsed section unfolds it. Fold state is not persisted across page reloads.

### Suggest mode

In Suggest mode, missing slots render as suggestion nodes guided by schema hint text. Click a suggestion node to open an editing form for that slot. When a collaborator or Claude pushes a suggestion via the MCP server or conductor API, the browser shows the proposed value as an inline diff alongside the current content. A toolbar offers "Accept all" and "Reject all"; individual suggestions can be accepted or rejected from the diff view.

Suggestion UI is limited to Suggest mode. Edit mode no longer shows suggestion overlays — the two modes are kept visually separate. Suggestion markers and a speech bubble icon accompany each pending suggestion node to make them easy to locate on a busy page.

The preview toggle switches between the current state and a preview of the page with all suggestions applied.

### NED-based suggestions

Suggestions are expressed as NED mutations via `CreateNedSuggestion`. Each suggestion carries a selection expression that is re-evaluated against HEAD when the suggestion is accepted, plus a structured mutation (`SetText`, `SearchReplace`, `Replace`, `InsertChild`, `InsertBefore`, `InsertAfter`, or `Delete`). This lets a collaborator target a phrase within a slot, rewrite a paragraph, or perform a structural change — and the accept-time re-evaluation catches conflicts before any edit is applied. Suggestions appear as separate diagnostic entries in the LSP and as separate nodes in the browser diff view.

### Creating new content from the browser

With `--dev-session` and a signed-in browser, the "+" button in the serve toolbar opens a form to create a new content file. Select a content type, enter a slug, and submit. The conductor scaffolds the file and the browser navigates to the new page immediately, with all required slots present as suggestion nodes.

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

### Typing in the editor

What you type in a content file reaches the site as you type it.

- **Live in the browser.** Every keystroke is lowered into the document the browser shows, with no delay and no save. The page in the browser changes as you type.
- **Only the block you change is replaced.** An edit inside a paragraph, a list item or a quote touches that block and nothing else. Edits made elsewhere (in the browser, through a suggestion, from the REPL) to other blocks survive your typing, and your edits to other blocks survive theirs.
- **A block changed in two places is a drift.** If you edit a block that was changed elsewhere since you began editing it, your version is not applied. The block gets a warning with two code actions: *Take the current version* replaces your text with the current one, and *Keep mine* makes your version the document's.
- **An unfinished construct is held.** While a code fence (or an HTML block that has no end yet) is open, the editor shows an information message: the fence is not closed yet, and the blocks after it are unchanged until you close it. Closing the fence inserts one code block. Saving with the fence still open takes it as it stands.
- **`:w` saves.** Saving writes the document. If something else changed the document meanwhile, the file gets the document's text, which includes those changes. If the document already reads as what you saved, the file stays exactly as you wrote it.
- **A change from outside is a conflict.** If a program that is not Presemble (a `sed`, a `git checkout`) rewrites the file while the document has unsaved edits, the file shows a warning. Code actions, offered on any line, resolve it: *Take the file on disk (discard unsaved edits)* replaces the buffer with the file, and *Keep the unsaved edits (overwrite the file)* writes the document over it.
- **Match the file on disk.** When the buffer and the file mean the same but are spelled differently (`*` and `-` list markers, say), this code action makes the buffer the file's text. It is not offered when the content differs.
- **Formatting.** Formatting a content file rewrites it as the canonical markdown Presemble writes. With format on save, what you save is what Presemble would write.

### Content file capabilities

| Capability | Description |
|---|---|
| Completions | Slot names from the schema; for link slots, actual content files formatted as `[Title](/type/slug)` |
| Diagnostics | Schema violations — missing slots, occurrence counts, capitalization, broken link references; blocks that were changed elsewhere (drift); constructs not closed yet; a conflict with the file on disk |
| Hover | Schema hint text for the slot at the cursor |
| Go-to-definition | Navigates to the linked content file |
| Code actions | Quickfix: capitalize first letter; insert missing slot snippet; resolve a drift; resolve a conflict; match the file on disk |
| Formatting | Canonical markdown |

### Template file capabilities

| Capability | Description |
|---|---|
| Completions | Data-path completions for `data="…"` attributes: `input.*` paths from the matching schema, `item.*` paths inside `data-each` loops, and collection names under `input` for `data-each` values |
| Diagnostics | Every expression is checked: `input.*` or `item.*` paths referencing fields not declared in the schema (and collections' item schemas), unknown templates and functions, wrong argument counts, and a record or list used as an attribute value |
| Hover | Schema hint text for the field at the cursor |
| Go-to-definition | Jumps from a template call, such as `(header input)`, to the template file or its `presemble:define` block |

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
| Template checking | Every expression in a template: paths exist in the schema, templates and functions exist, argument counts, no record or list as an attribute value (reported in the editor); an unknown template or a removed spelling fails the page |
| Asset resolution | Assets referenced by templates exist on disk |
| Link validation | Internal `href` values resolve to built pages |
| HTML output | Final HTML is well-formed XML |

Validation failures are build failures. There is no runtime state where invalid content reaches a browser.

See [validated at every level](/feature/validated-at-every-level) for the validation model.

## Template composition

Templates are function files. A template file has two parts: optional local definitions at the top and a composition expression at the bottom. The composition expression is the template's return value — a DOM tree. A template is used by [calling it](#calling-templates).

### Local definitions

Local definitions at the top of a template file are reusable fragments, named with `presemble:define`:

```clojure
[:template {:presemble/define "byline"}
  [:p {:class "byline"}
    [:presemble/insert {:data input.author :as "span"}]]]

[:main ((juxt header self/byline footer) input)]
```

The local `byline` fragment is referenced as `self/byline` in the composition expression.

### juxt

`juxt` fans the same input to multiple templates and concatenates their DOM outputs in order:

```clojure
[:template {:presemble/define "body"}
  [:p [:presemble/insert {:data input.summary :as "span"}]]]

[:body ((juxt header self/body footer) input)]
```

`header`, `self/body`, and `footer` all receive the full content tree as their `input`. Their outputs are assembled into a single page DOM. `self/body` refers to the local definition `body` of the current template file. A name without `self/` is a template of the site, `templates/header.hiccup` or `templates/header/item.hiccup`, and also a local definition of the same name.

### Threading

`->` threads a value through a sequence of the functions of [Expressions](#expressions), the value as the first argument:

```clojure
[:h1 (-> input.title text to_upper)]
```

These are the only functions; they are added when a template needs another.

## Link expressions in content

Content files can include link expressions that assemble collections at build time. A link expression is a query written as the destination of a link:

```markdown
[](->> :post (sort-by :published :desc) (take 5))
```

The expression is resolved against the site graph. The result is a validated list satisfying the collection schema for that type. The template receives the assembled list ready to iterate.

Link expressions make content the place where assembly decisions live. The homepage content file decides which collections appear and in what order; the template decides how they look.

### Expression syntax

A link expression starts with `->>` and a source collection, followed by any number of operations applied in order. The table shows the parenthesised form you can also type at the REPL; in a content file, drop the outer parentheses:

| Expression | Effect |
|---|---|
| `(->> :post (sort-by :published :desc) (take 5))` | Latest 5 posts |
| `(->> :feature (sort-by :title :asc))` | Features sorted by title |
| `(->> :post (filter :status "pinned"))` | Pinned posts only |
| `(->> :post (refs-to self))` | All posts that link to the current page |

### Link expression reference

Link expressions are a fixed query vocabulary, not general code: the destination is read by Presemble's own small grammar and never evaluated as Presemble Lisp. The complete grammar is:

```
thread  := '->>' WS ':' keyword (WS op)*
op      := '(' 'sort-by' WS ':' keyword [WS (':asc' | ':desc')] ')'
         | '(' 'take' WS digits ')'
         | '(' 'filter' WS ':' keyword WS string ')'
         | '(' 'refs-to' WS ('self' | string) ')'
keyword := [A-Za-z0-9_-]+
string  := '"' ( [^"\\] | '\\' ["\\] )* '"'
WS      := [ \t]+
```

`sort-by` without a direction sorts ascending. Strings are double-quoted and may contain spaces; write a literal quote as `\"` and a backslash as `\\`. `(refs-to self)` means the current page, while `(refs-to "/author/alice")` names a page by URL. Only `->>` is accepted; `->` is reported with a suggestion to use `->>`.

A link expression takes effect only as a whole paragraph in a preamble slot; anywhere else (the body, a blockquote, a list) it has no effect and is reported. Write it without outer parentheses: `[posts](->> :post (take 4))`. Older spellings (`[posts]((->> :post (take 4)))`, `%20`-encoded or angle-bracket destinations) are still read and are written back in this form. A link expression inside running text has no effect and is reported as a diagnostic, as is a malformed one.

A link expression only takes effect in a preamble slot, above the `----` separator. In the body it is ignored and reported with a diagnostic asking you to move it above `----`.

### Choosing the slot with a slot marker

By default a link expression fills the next preamble slot by position, like any other preamble element. To put it in a specific link slot, end the line with a slot marker, the same `{#name}` syntax schemas use:

```markdown
[Latest posts](->> :post (sort-by :published :desc) (take 4)) {#latest}

[In the news](->> :post (filter :category "news")) {#news}
```

A marked expression fills the named link slot wherever it appears in the preamble, so two adjacent slots that both accept any number of links each get their own expression. The text in brackets is display text only; it never chooses the slot. The marker goes on the same line as the link expression, after at least one space; a marker on the next line is reported. When the file is saved, markers are kept and slots are written in schema order. This works for every link-expression spelling, including `[text](:path)`.

If a marker names no slot or a slot that is not a link slot, is invalid, is on its own line, follows a multi-line expression, or is on anything other than a link expression (text, an image, a plain link), the line is kept exactly as written but not used, and the page cannot be published until it is fixed. A line that is only `{#name}` is reported the same way. A paragraph ending in `{#name}` is reported the same way: paragraphs, headings, images and lists fill slots by position. To end a paragraph with a literal `{#name}`, write `\{#name}`. A heading ending in `{#name}` is reported the same way; to end a heading with a literal `{#name}`, write `\{#name}`, or end the heading with a code span. If an unmarked expression's text equals the name of a link slot it did not land in, for example `[news](->> :post)` placed in `latest`, a hint suggests adding the marker: `[news](->> :post) {#news}`.

### Reverse references with `refs-to self`

Any content file can populate a slot with all pages that link to it by using `(refs-to self)` in a link expression:

```markdown
[Posts](->> :post (refs-to self)) {#posts}
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

Run `presemble serve <dir> --dev-session` on an empty directory and open the Login URL it prints to start the guided setup wizard. Without `--dev-session` the wizard shows "Editing is disabled on this server." Six steps take you from zero to a fully styled, working site.

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

Every starter includes shared navigation by calling the `nav` template (`(nav input)`) and breadcrumb navigation on item pages. Collection index pages list all items of that type. No page is a dead end.

### After scaffolding

The wizard writes files to disk and the file watcher picks them up immediately. Edit content files, see changes in the browser, use the LSP for completions and diagnostics. The normal serve workflow applies from the first page load; keep `--dev-session` on to keep editing in the browser.

`presemble init <dir>` is still available for scripted setups — it produces a minimal hello-world `note` site without the browser wizard.
