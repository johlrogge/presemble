# ADR-007: Crawl-based publishing model

## Status

Accepted

## Context

The current publisher scans directories for schemas and content files, then builds everything it
finds. This has several gaps:

- No dead link detection — internal links in content or templates can silently point nowhere
- Index/collection pages have no model — a page that aggregates all articles has no schema, no
  content file, and no clear place in the pipeline
- No build ordering — content that references other content (an article's author) may be built in
  any order
- Unreachable content has no status — content not linked from anywhere is built silently, with no
  indication it may be orphaned
- Cross-content reference validation (an M0 deliverable) has no architectural home

## Decision

The publisher adopts a **crawl-based publishing model**: instead of scanning directories, it
traverses the site graph starting from declared entry points.

### Entry points

The default entry point is `templates/index.html`. The user may declare additional entry points in
site configuration (e.g. `site.yaml`):

```yaml
entry_points:
  - templates/index.html
  - templates/feed.xml
```

If `templates/index.html` does not exist, the publisher reports an error rather than scanning for
content to build.

### Crawl and build algorithm

1. **Start** at each entry point template
2. **Parse** the template to find all outbound references:
   - `<presemble:insert data="article.title">` → depends on an `article` data item
   - `<template data-each="site.articles">` → depends on the full `article` collection
   - Rendered `<a href="/authors/johlrogge">` links → depends on the `/authors/johlrogge` page
3. **Resolve** each reference:
   - Collection references (`site.articles`) → gather all content items matching that schema
   - Item references (`article.author`) → find the content item at the referenced path
   - URL links in output → schedule the linked page for building if it maps to a known
     template/content
4. **Schedule** unbuilt dependencies for building (depth-first)
5. **Detect cycles** — if A depends on B and B depends on A, build both with a placeholder for the
   circular reference on the first pass, then patch on the second pass (or error, depending on
   severity)
6. **Build** each item: parse schema, parse content, validate, render template, write output
7. **Validate links** after rendering: check that every internal link in the output resolves to a
   built page

### Collection pages (virtual pages)

Some pages have no associated content file — they are derived from collections. The `index.html`
template has access to the full `site` data graph, including:

- `site.articles` — all content items matching the `article` schema
- `site.authors` — all content items matching the `author` schema

The publisher resolves these collection references by gathering all content of the matching type.
Collection pages are rendered last (after their members are built), so the data graph is complete
when they render.

### Link validation

**Internal links** (links to pages within the site) are validated at build time:

- Hard fail: a link in output HTML that targets a path not produced by the build
- This catches broken `<a href="/articles/missing">` links, broken image `src` attributes, and
  unresolved `<presemble:insert>` references

**External links** (links to other domains) are validated optionally:

- Default: skip (too slow, too fragile for CI)
- With `--check-links` flag: HTTP HEAD request for each external URL, warning (not error) on
  non-200

**Unresolved schema slot references** (e.g. `article.author` points to `/authors/johlrogge` but
no author content exists at that path):

- Hard fail at build time (this is a cross-content reference violation, already part of M0 scope)

### Build output structure

Output paths mirror the declared URL structure, not the content directory structure. The publisher
maps:

- `content/article/hello-world.md` → URL `/article/hello-world` → `output/article/hello-world.html`
- `content/authors/johlrogge.md` → URL `/authors/johlrogge` → `output/authors/johlrogge.html`

The URL structure is determined by where links point, not by filesystem conventions. The publisher
validates that every output path is linked from somewhere (or is an entry point).

## Alternatives considered

**Directory scanning (current approach)** — simpler to implement, but produces no link validation,
no build ordering, and no collection pages. Does not scale to sites with cross-content references.

**Explicit build manifest** — user declares every page to build in a config file. Precise, but
labour-intensive for large sites and duplicates information already present in templates and
schemas.

**Sitemap-driven** — publisher reads a `sitemap.xml` as the build manifest. Familiar but requires
maintaining a sitemap separately from content, creating a second source of truth.

## Consequences

**Positive:**

- Dead links are impossible to publish — internal link validation is structural
- Collection pages (index, feeds, archives) have a natural model: templates with access to
  `site.*` collections
- Cross-content references are validated by construction — the crawler fails if a referenced item
  cannot be built
- Build order is deterministic and correct — dependencies built before dependents
- Unreachable content is surfaced — content not linked from any entry point produces a warning

**Negative / open questions:**

- Requires a crawler / build graph component — significant new infrastructure
- Cycle detection adds complexity; the policy for circular references (error vs. placeholder)
  needs specification
- `site.articles` collection resolution needs a query model — how does the publisher know which
  content items belong to which collection? (By schema name matching the collection name is the
  simplest answer)
- URL mapping from content paths to output paths needs a convention or configuration
- The `--check-links` external link checker needs rate limiting and caching for large sites
- This is a significant departure from M0's directory-scanning model; M0 should complete before
  adopting this model (target: M1 or M2)

## Experiment scope

Before full implementation:

1. Implement `templates/index.html` as the default entry point — the publisher starts there
   instead of scanning
2. Expose `site.articles` as a collection in the data graph (gathered by schema name convention)
3. Validate that internal links in rendered output resolve to built pages
4. Defer: cycle detection, external link checking, `--check-links` flag

---

## Addendum — 2026-03-27

Collections moved to the root-level namespace. All references to `site.articles`, `site.authors`,
and similar `site.*` collection names throughout this ADR are superseded. The correct form is
bare collection names (`articles`, `authors`) looked up directly from the data graph root. The
`site.*` prefix was a transitional design that has been removed.
