# Getting Started

scaffold and build a Presemble site in two commands.

----

## Install

Presemble is built with Rust. Install from source:

```
cargo install --path projects/publisher
```

Once the project reaches its first release, `cargo install presemble` will work directly.

## Scaffold a site

Run `presemble init my-site/` to generate a working hello-world site:

```
presemble init my-site/
```

This creates the following files:

- `schemas/note.md` — defines the "note" content type with a title and body. See [schemas](/feature/schemas-as-contracts) for details.
- `content/note/hello-world.md` — your first note, validated against the schema at build time.
- `templates/index.html` — home page, iterates over all notes. See [templates](/feature/templates-are-data) for details.
- `templates/note.html` — renders an individual note.
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

Starts a local server with file watching and live rebuild on every change. The browser reloads automatically — and navigates directly to the changed page if you were on a different one.

## Next steps

- [Schemas](/feature/schemas-as-contracts) — learn the schema grammar and compile-time content safety
- [Templates](/feature/templates-are-data) — data-bound HTML templates without a template language
- [The Data Graph](/feature/the-data-graph) — how content is structured and accessed in templates
- [User Guide](/guide/user-guide) — full reference for all Presemble features
