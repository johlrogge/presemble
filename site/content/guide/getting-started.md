# Getting Started

Scaffold and build a Presemble site in two commands.

----

## Install

Presemble is built with Rust. Install from source:

```
cargo install --path projects/publisher
```

## Start with the site wizard

The fastest way to start is to point `presemble serve` at an empty directory:

```
mkdir my-site
presemble serve my-site/
```

The browser opens a six-step wizard: site type (blog, personal, or portfolio), font mood, color seed, palette type, complexity, and template syntax (Hiccup or HTML). A live preview panel updates on each step. The wizard generates a custom stylesheet and scaffolds a complete working site, then navigates the browser to your new homepage.

## Or scaffold from the command line

Run `presemble init my-site/` to generate a hello-world site without the browser wizard:

```
presemble init my-site/
```

This creates:

- `schemas/note/item.md` — defines the "note" content type with a title and body. See [schemas](/feature/schemas-as-contracts) for details.
- `content/note/hello-world.md` — your first note, validated against the schema at build time.
- `templates/index.html` — home page, iterates over all notes. See [templates](/feature/templates-are-data) for details.
- `templates/note/item.html` — renders an individual note.
- `assets/style.css` — only files *referenced by templates* are copied to output.

## Build

```
presemble build my-site/
```

Output goes to `output/my-site/` (a sibling of your site directory). The publisher validates content against schemas, renders templates with the content data, and copies only the assets referenced by templates.

## Serve locally

```
presemble serve my-site/
```

Starts a local server with file watching and live rebuild on every change. The browser reloads automatically — and navigates directly to the changed page if you were on a different one. Click any body element to edit it inline.

## Editor support

```
presemble lsp my-site/
```

Point your editor's LSP configuration at this binary. The server provides completions,
diagnostics, hover, and go-to-definition for content files, template files, and schema
files from a single process.

See [Editor LSP Support](/feature/editor-lsp-support) for setup instructions.

## Claude integration

```
presemble mcp my-site/
```

Starts an MCP server for Claude Code integration. Claude can read your schemas, read your content, and push editorial suggestions to specific slots. Each suggestion appears as an LSP diagnostic in your editor with an accept/reject code action.

See [Editorial Collaboration](/feature/editorial-collaboration) for details.

## Next steps

- [Schemas](/feature/schemas-as-contracts) — learn the schema grammar and compile-time content safety
- [Templates](/feature/templates-are-data) — data-bound HTML templates without a template language
- [The Data Graph](/feature/the-data-graph) — how content is structured and accessed in templates
- [Editor LSP Support](/feature/editor-lsp-support) — completions and diagnostics in your editor
- [Editorial Collaboration](/feature/editorial-collaboration) — Claude and human editorial suggestions
- [Browser Editing](/feature/browser-editing) — edit content directly in the browser
- [The Presemble REPL](/feature/the-presemble-repl) — evaluate expressions against live content
- [User Guide](/guide/user-guide) — full reference for all Presemble features
