# ADR-012: Cross-content reference resolution

## Status

Proposed

## Context

Content pages link to other pages via markdown links. The link text is hardcoded in the source
document at authoring time. When a referenced page changes — an author renames themselves, a
product page updates its title — every linking page continues to show the old, hardcoded text.

The data graph for a content page already contains `Value::Record` entries for each link slot.
A link record has an `href` field (the URL path to the target) and a `text` field (the visible
link label). These fields are the only data available to a template when rendering a link — the
canonical data for the referenced page (author bio, display name, avatar) is not present.

Templates currently work around this by relying on the hardcoded `text` field or by requiring
content authors to duplicate data. Neither approach is correct: hardcoded text goes stale, and
duplication defeats the purpose of having canonical pages.

The build pipeline processes pages individually. At the time `build_content_page` runs for an
article, the author page may not yet have been built — or may have been built but its data is
not accessible to the page under construction. A single-pass build cannot resolve cross-page
references inline.

## Decision

After all content pages are built, a dedicated resolution phase walks each page's `DataGraph`
looking for `Value::Record` entries whose `href` field matches the `url_path` of another
`BuiltPage`. When a match is found, the referenced page's data fields are merged into the link
record. The `href` and `text` fields from the original link record are preserved; all other
fields from the referenced page's data graph root are added.

This makes canonical data available to templates via ordinary path resolution. A template that
renders an article can access `post.author.name`, `post.author.bio`, and `post.author.avatar`
without the article author duplicating any of that data.

Resolution is one level deep. A resolved field that itself contains an `href` is not further
resolved. This bound keeps the algorithm O(pages * fields) and makes cycles impossible: the
walk does not recurse into merged records.

The resolution phase runs as a post-build step inside `build_site`, after the full set of
`BuiltPage` values is available. It operates on the already-built page list and returns a new
list with resolved data graphs — it does not mutate build state in place.

## Alternatives considered

**New `Value::Ref` variant with lazy loading** — introduce a reference variant into the `Value`
enum that carries a `url_path` and resolves on first access during template rendering. This
would avoid a separate phase but requires the template renderer to carry a reference to the
full page set, adds lazy evaluation semantics to `Value`, and changes how every renderer
consumer reasons about the enum. The added complexity is not justified when an eager post-build
pass is sufficient.

**Schema-level type references (ADR-002 section 5)** — the `[[path]]` reference mechanism
describes shared schema fragments, not runtime data. It solves schema composition (reusing a
field declaration across content types) rather than runtime data resolution (making a referenced
page's actual field values available). The two mechanisms are orthogonal; ADR-002 section 5
remains future work and does not address this problem.

**Resolve during `build_content_page`** — impossible without access to other pages' data. The
page-level build function operates on a single content document and its schema. The full set of
`BuiltPage` values does not exist until all pages have been built.

**Template-level resolution** — allow templates to declare cross-page data dependencies
explicitly (e.g., a `presemble:include` that fetches a named page's data graph). This shifts
the resolution burden onto template authors, creates a second mechanism for what is already
expressed by link `href` values, and makes the data available to templates inconsistent
depending on whether a resolution annotation was added.

## Consequences

**Positive:**
- Templates access referenced page data via path resolution: `post.author.name`,
  `post.author.bio`, `post.author.avatar` all work without author duplication in content.
- Canonical data propagates automatically when a referenced page changes and the site is rebuilt.
- No changes to the schema format, the `Value` enum, or the template renderer.
- The post-build phase is a pure function over the `BuiltPage` list — straightforward to test
  and reason about in isolation.
- Resolution cost is O(pages * fields): negligible for typical site sizes.

**Negative / trade-offs:**
- Incremental rebuild (ADR-008) must re-run the resolution phase after rebuilding any affected
  page. A page that is not rebuilt but whose referencing pages were rebuilt must still be
  re-resolved. The resolution phase is cheap enough to run in full after any incremental build.
- Resolution is one level deep by design. Templates cannot traverse multiple hops
  (`post.author.publisher.name`) via this mechanism. Deeper traversal would require either
  multi-pass resolution (with cycle detection) or the `Value::Ref` approach — both are deferred.
- Link records whose `href` does not match any built page are left unchanged. No error is
  raised for unresolved links. This is intentional: external links and anchors are valid `href`
  values that will never match a `BuiltPage`.
