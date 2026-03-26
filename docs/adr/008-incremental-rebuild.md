# ADR-008: Incremental rebuild via dependency graph

## Status

Proposed

## Context

Full rebuild on every file change is wasteful. As a site grows, rebuilding every page on each
keystroke makes the serve loop feel sluggish and breaks the fast-feedback property that live editing
depends on.

The crawl model (ADR-007) already implies a dependency structure: the crawler resolves which
content files, templates, and schemas contribute to each output page. Making that structure
explicit — as a named, inspectable type — enables incremental rebuilds without duplicating the
traversal logic or introducing a separate dependency tracking mechanism.

## Decision

File-level dependency tracking. `BuildOutcome` gains a `DependencyGraph` field that records which
source files each output page depends on. A separate `rebuild_affected` function handles partial
rebuilds. `build_site()` stays the full clean build. The serve loop stores the current graph and
calls `rebuild_affected` on each watcher event.

### Key types

```rust
pub struct DependencyGraph {
    forward: HashMap<PathBuf, HashSet<PathBuf>>,  // output -> sources
    reverse: HashMap<PathBuf, HashSet<PathBuf>>,  // source -> outputs
}
```

The **forward** index answers: "what source files does this output depend on?" The **reverse**
index answers: "if this source file changes, which outputs need rebuilding?" Both directions are
maintained together to make lookups O(1).

### API

```rust
/// Full clean build. Populates and returns a DependencyGraph.
pub fn build_site(site_dir: &Path) -> Result<BuildOutcome>;

/// Partial rebuild. Only rebuilds outputs affected by dirty_sources.
/// Returns an updated graph (merged with current_graph).
pub fn rebuild_affected(
    site_dir: &Path,
    dirty_sources: &[PathBuf],
    current_graph: &DependencyGraph,
) -> Result<BuildOutcome>;
```

`rebuild_affected` looks up each dirty source in the reverse index, collects the union of affected
outputs, and runs the build pipeline for only those outputs. It does not mutate `current_graph`;
it returns a fresh `BuildOutcome` (including an updated graph) that the caller merges or replaces.

### Collection pages

Collection pages (e.g. `index.html` rendered from `site.articles`) depend on every content file
of their constituent types. Any change to an article therefore triggers a collection page rebuild.
This is correct by construction: the graph is populated during the crawl, and the crawl already
gathers all items for a collection reference.

### Serve loop integration

```
1. cold start: graph = build_site(site_dir).graph
2. on watcher event with dirty_sources:
   a. outcome = rebuild_affected(site_dir, dirty_sources, &graph)
   b. graph = outcome.graph          // replace; graph is re-derived each rebuild
   c. notify browser of changed outputs
```

The graph is in-memory only. Cold start always does a full build; there is no on-disk graph cache
to invalidate or migrate.

## Alternatives considered

**mtime-based timestamp comparison** — simple, but only detects whether a source file changed, not
which outputs are affected. Does not handle transitive dependencies (a shared template partial
changes — which pages use it?).

**Content hashing** — more precise than timestamps (avoids rebuilds when a file is touched but
unchanged), but adds I/O overhead on every check and does not by itself identify which outputs need
rebuilding. Can be layered on top of this ADR's graph later.

**Modifying `build_site()` to accept a dirty-file list** — conflates full and partial builds in a
single function, complicates the main build path, and makes it harder to reason about correctness.
Keeping `build_site()` and `rebuild_affected()` separate preserves the invariant that a clean build
is always available as a fallback.

## Consequences

**Positive:**

- Only changed pages rebuild; unaffected pages are untouched
- Correctness by construction: deps are recalculated on each rebuild, not cached across builds
- `build_site()` is unchanged — the full build path stays simple and auditable
- The graph is a first-class type, inspectable for debugging and future tooling (e.g. a
  `presemble deps <file>` subcommand)

**Negative / open questions:**

- Collection page rebuilds re-read all content files of the relevant type even when only one
  article changed; this is correct but not maximally efficient
- Template includes and partials (not yet implemented) will need to add sub-template paths to the
  dependency graph when that feature lands; omitting them would cause stale outputs silently
- The graph is lost on process restart — cold start cost is always paid after a crash or manual
  restart
- Granularity is file-level, not block-level; a change anywhere in a large content file triggers
  rebuild of all outputs that depend on it
