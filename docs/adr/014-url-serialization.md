# ADR-014: URL serialization at publish time

## Status

Proposed

## Context

Presemble sites are built from content, templates, and a data graph. URLs appear throughout:
`<a href="/author/johlrogge">`, `<link rel="stylesheet" href="/css/style.css">`,
`<img src="/images/cover.jpg">`. These URLs are written as root-relative paths by content
authors, template authors, and data graph entries.

Root-relative URLs (`/path/from/site-root`) work correctly when the site is served at the
domain root (`https://example.com/`). They break silently when the site is hosted at a
subdirectory — a common case on GitHub Pages (`https://johlrogge.github.io/presemble/`),
staging environments (`https://staging.company.com/docs/`), or any CDN with a path prefix.

At the same time, baking the deployment topology into content or templates creates a coupling
that conflicts with Presemble's "data not text" principle: content should describe what is true
about a document, not where it will be deployed. Exposing a `base_url` variable to templates
puts deployment concerns inside editorial artifacts, leading developers to embed hosting details
in content that must then be surgically updated when deployments change.

The URL rewrite problem is a publishing concern, not a content concern. It belongs at the
serialization layer.

## Decision

The publisher rewrites all root-relative URLs at HTML serialization time. Sources (content,
templates, and the data graph) always use root-relative paths. The publisher transforms them
at output time according to the configured URL style.

### URL styles

Three styles are supported:

**Relative** (default): URLs are rewritten to be relative to the output file's location.
`/author/johlrogge` in a page at `/blog/2024/post.html` becomes `../../author/johlrogge`.
Works at any deployment root with zero configuration.

**Root** (with optional `base-path`): URLs keep their root-relative form, optionally prefixed
with a base path. `/author/johlrogge` becomes `/author/johlrogge` (no base path) or
`/presemble/author/johlrogge` (with `base-path: /presemble`). Requires the site to be served
at a known path prefix.

**Absolute** (with `base-url`): URLs are rewritten to fully qualified form.
`/author/johlrogge` becomes `https://presemble.io/author/johlrogge`. Required for RSS feeds,
Open Graph tags, and canonical links.

### Key invariant

Sources always write root-relative paths. The publisher handles transformation:

| Source path | Relative (from `/blog/post.html`) | Root + `/presemble` | Absolute + `https://presemble.io` |
|---|---|---|---|
| `/author/johlrogge` | `../../author/johlrogge` | `/presemble/author/johlrogge` | `https://presemble.io/author/johlrogge` |
| `/css/style.css` | `../../css/style.css` | `/presemble/css/style.css` | `https://presemble.io/css/style.css` |

### Configuration

URL style is controlled by `.presemble/config.json` (optional). The file is absent by default;
the publisher uses Relative mode when no config is present.

```json
{
  "url-style": "root",
  "base-path": "/presemble"
}
```

```json
{
  "url-style": "absolute",
  "base-url": "https://presemble.io"
}
```

CLI flags override the active config file:

| Flag | Description |
|---|---|
| `--url-style <relative\|root\|absolute>` | Override the URL style |
| `--base-path <path>` | Override the base path (Root style) |
| `--base-url <url>` | Override the base URL (Absolute style) |
| `--config <path>` | Load a named config file instead of the default |

### Multiple config files for multiple deployment targets

Multiple configs can live under `.presemble/` for different deployment targets:

- `.presemble/config.json` — default (Relative mode, works for local preview)
- `.presemble/github-pages.json` — `{"url-style": "root", "base-path": "/presemble"}`
- `.presemble/production.json` — `{"url-style": "absolute", "base-url": "https://presemble.io"}`

Selected with `presemble build site/ --config=.presemble/github-pages.json`.

This lets the same source tree produce correct output for local preview, staging, and
production with no source changes.

## Alternatives considered

**`<base href>` tag**: The HTML `<base>` element sets a base URL for all relative references
in the page. Fragile in practice — it breaks anchor links (`#section`), same-page references,
and any JavaScript that calls `location.href`. Not all HTML consumers respect it. Ruled out.

**Template URL helper functions**: Provide a `url()` helper that templates call to resolve
paths. This leaks deployment configuration into templates. Authors must remember to call the
helper on every URL. Content documents (which are not templates) cannot call helpers. Ruled
out — violates the "data not text" principle.

**Environment variables**: Set `PRESEMBLE_BASE_URL` at build time. Less discoverable than
config files, does not compose (cannot define multiple targets), and differs between CI
systems. Named config files under `.presemble/` are preferred.

**Rewrite at content-parse time**: Transform URLs when content is parsed rather than at
serialization. Breaks the data graph: if the graph stores absolute URLs, the same data cannot
produce correct output for multiple deployment targets from a single build run. The data graph
must remain target-independent.

## Consequences

**Positive:**

- Default output (Relative mode) works at any deployment root with zero configuration — no
  `config.json` required for local development or single-domain deployments
- Content authors, template authors, and data graph contributors never see or think about
  deployment topology
- Multiple deployment targets are first-class: one source tree, several named configs, zero
  source changes
- Link validator can check graph edges without knowing the output URL style — validation
  happens on root-relative paths before transformation
- Relative mode URL correctness is mechanically derived from validated graph edges, so the
  link validator skips a redundant structural check in that mode

**Negative / open questions:**

- Relative URLs (`../../style.css`) are less readable in generated HTML than root-relative
  (`/style.css`) — accepted trade-off for universal portability
- The serialization layer must have access to both the source URL and the output file path to
  compute relative rewrites — requires a two-pass or path-aware serializer
- Adds `serde_json` dependency to `publisher_cli` for config file parsing
- Rewriting every attribute that can carry a URL (`href`, `src`, `action`, `srcset`, data
  URLs, CSS `url()` in inline styles) requires an exhaustive attribute list — missing an
  attribute is a silent bug; the attribute set must be kept current as HTML evolves
