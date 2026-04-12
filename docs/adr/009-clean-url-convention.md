# ADR-009: Clean URL convention

## Status

Accepted

## Context

Presemble builds static HTML pages from content files. The initial implementation wrote output
files as flat `.html` files: `output/article/hello-world.html`, reachable at
`/article/hello-world.html`.

Content authors write links in templates and cross-content references. If the output format is
`.html`, those links must include the `.html` extension. This couples the author-facing URL
convention to the output file format — a leaky abstraction that violates the principle that content
should not encode output format details.

Modern static site conventions use clean URLs: `/article/hello-world` rather than
`/article/hello-world.html`. Standard web servers serve directory index files automatically
(`/article/hello-world/` resolves to `/article/hello-world/index.html`) with no special
server configuration needed.

## Decision

Pages are published at `/{schema_stem}/{slug}/index.html` and are reachable at
`/{schema_stem}/{slug}`.

Content authors link to pages using clean paths without `.html` extensions. For example,
a link to the hello-world article is written as `/article/hello-world`.

The `url` field in the article data graph contains the clean URL (e.g. `/article/hello-world`),
not the file path.

## Alternatives considered

**Extension URLs** (`/article/hello-world.html`) — simpler output structure (flat files) but
forces `.html` into every link an author writes. The output file format leaks into the content
authoring experience.

**Flat directory with index.html** (`output/index.html`, `output/article.html`) — produces
extension URLs, same problem as above.

## Consequences

**Positive:**
- Authors write clean links: `/article/hello-world`, `/author/johlrogge`
- The `url` field in templates never contains `.html`
- No web server configuration needed — directory index serving is a universal default
- Output layout matches the mental model: each page is a self-contained directory

**Negative / trade-offs:**
- Each page occupies a directory rather than a flat file; output tree is slightly deeper
- The `parent()` of `output_path` must be created before writing `index.html`
- Link validation must recognise `/slug`, `/slug/`, and `/slug/index.html` as equivalent

**Output layout example:**

```
output/
  index.html
  article/
    hello-world/
      index.html
  author/
    johlrogge/
      index.html
```
