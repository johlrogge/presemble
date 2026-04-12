# ADR-010: Data-driven asset discovery

## Status

Accepted

## Context

Presemble needs to copy static assets (CSS, images, scripts) from the site source directory
into the output directory so the published site can reference them.

The initial implementation used a blind directory copy: if `assets/` exists in the site root,
copy the entire directory to `output/assets/`. This is simple but has two problems:

1. Dead files accumulate in output — any file placed in `assets/` is copied regardless of whether
   any template references it.
2. Missing assets are silent: a template can reference `/assets/style.css` but if that file
   doesn't exist, the build succeeds and the deployed site has a broken stylesheet.

ADR-005 establishes that templates are data. The template DOM tree is already parsed during
rendering. Asset references (`<link href="...">`, `<img src="...">`, `<script src="...">`) are
structured data in that DOM tree — not strings that require post-render scanning.

## Decision

Asset files are copied to the output directory only when they are referenced by a parsed template
DOM tree.

During `build_site()`, before schema processing, all `.html` files in `templates/` are parsed with
`parse_template_xml`. The resulting DOM trees are walked by `extract_asset_paths`, which collects
`href` values from `<link>` elements and `src` values from `<img>` and `<script>` elements that
start with `/` and do not contain `://` (i.e., local paths, not external URLs).

Presemble annotation elements (`presemble:*`) are skipped entirely during the walk — their
attribute values are data-graph paths, not asset references.

The collected paths are deduplicated and sorted. For each path, `copy_referenced_assets` verifies
the source file exists and copies it to the corresponding location under `output/`. A missing
referenced asset is a build error.

Templates that fail XML parsing during asset discovery emit a warning and are skipped — they do
not prevent the build from collecting assets from other templates.

## Alternatives considered

**Blind directory copy** (previous implementation) — simple, but copies unreferenced files and
silently ignores missing files. Inconsistent with the data-driven philosophy.

**Post-render scan** — scan rendered HTML output for asset references after all pages are built.
This is too late: the output files exist but the mapping back to source paths would require
re-parsing. More importantly, it inverts the dependency: assets should be a build input, not
derived from build output.

**Manifest file** — require authors to declare assets in a separate manifest (`assets.toml`,
`assets.json`). This creates a second source of truth: templates already declare what they need
via `<link>` and `<img>` tags. A manifest would have to be kept in sync with templates manually.

## Consequences

**Positive:**
- No dead files in output — only referenced assets are copied.
- Missing assets are build errors, not silent failures at serve time.
- Asset references are validated at build time against the actual file system.
- Follows the same data-driven philosophy as template rendering (ADR-005).

**Negative / trade-offs:**
- Templates are parsed twice: once for asset discovery, once for rendering. This is acceptable
  because template parsing is fast and the separation keeps the two concerns independent.
- Templates that are not valid XML (e.g., partial fragments using a different syntax) cannot
  contribute to asset discovery and produce a warning. In the current fixture, the main entry-point
  templates (`article.html`, `author.html`, `index.html`) are valid XML and carry the relevant
  asset references.
- Asset paths that appear only in content (not templates) are not discovered. This is intentional:
  content files reference data, not assets.
