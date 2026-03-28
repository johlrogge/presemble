# Getting Started

Go from zero to a published Presemble site.

----

## Install

Presemble is built with Rust. Install from source for now:

```
git clone https://github.com/presemble/presemble
cargo install --path projects/publisher
```

Once the project reaches its first release, `cargo install presemble` will work directly.

## Create a site

A Presemble site is a directory with four subdirectories:

```
my-site/
  schemas/      # document grammars
  content/      # content files validated against schemas
  templates/    # HTML templates that consume content data
  assets/       # CSS, images, fonts — copied verbatim to output
```

Start by creating the directories:

```
mkdir -p my-site/{schemas,content,templates,assets}
```

## Define a schema

A schema is a plain markdown file that describes the structure of a content type. Create `schemas/note.md`:

```markdown
# Note title {#title}
occurs
: exactly once
content
: capitalized

A short description. {#body}
occurs
: 1..3
```

Every named field (`{#title}`, `{#body}`) becomes a data path you can reference from templates. See [schemas as contracts](/feature/schemas-as-contracts) for the full grammar.

## Write content

Content files match the schema by position and structure — no annotations needed in the author's document. Create `content/note/hello.md`:

```markdown
# Hello, Presemble

This is my first note built with Presemble.
```

The publisher validates this file against `schemas/note.md` at build time. A mismatch fails the build with a clear error message.

## Write a template

Templates are HTML files with `presemble:insert` elements that pull in named data. Create `templates/note.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>Presemble</title>
  <link rel="stylesheet" href="/assets/style.css" />
</head>
<body>
  <main>
    <presemble:insert data="note.title" as="h1" />
    <presemble:insert data="note.body" as="p" />
  </main>
</body>
</html>
```

The template vocabulary is finite and verifiable: only data paths that exist in the schema are valid.

## Build

Run the build command from your workspace root:

```
presemble build my-site/
```

Output lands in `my-site/output/`. Each content file becomes a clean URL directory:

```
my-site/output/note/hello/index.html
```

Zero schema violations, zero runtime surprises. See [instant feedback](/feature/instant-feedback) for details on build output and error reporting.

## Serve locally

Start the development server with:

```
presemble serve my-site/
```

The server watches `schemas/`, `content/`, and `templates/` for changes and rebuilds incrementally. Refresh the browser to see updates. A future release will add live reload.
