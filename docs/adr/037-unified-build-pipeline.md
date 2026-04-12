# ADR-037: Unified build pipeline via site_builder component

## Status
Accepted

## Context

The CLI (`build_site()` in publisher_cli) and the conductor (`build_full_graph()`) each implement their own version of the site graph building pipeline. Both iterate schema stems, parse content, build data graphs, inject metadata, and construct SiteNode entries — but with subtle differences. This duplication means bug fixes and new features (e.g., adding collection pages) must be applied twice, and the two pipelines can drift out of sync.

The conductor's version was originally "intentionally simplified" (item pages only), but has grown to include collection pages and legacy fallback. It now mirrors the CLI's phases 1a, 1b, and 1c — duplicating ~250 lines of logic.

## Decision

Extract the shared graph-building phases into a new polylith component `site_builder` with three public functions:

- `build_graph(repo, output_dir, source_attachment)` — builds item pages (Phase 1a), collection pages (Phase 1b), and legacy fallback root (Phase 1c). Returns a `GraphBuildResult` containing the `SiteGraph` plus per-page diagnostics.
- `resolve_link_expressions(graph)` — resolves PathRef and ThreadExpr link expressions in all pages (Phase 1.5).
- `resolve_cross_references(graph)` — resolves cross-content link references (Phase 2).

The CLI calls all three functions, then applies its `BuildPolicy` to diagnostics, then handles rendering, validation, and cleanup. The conductor calls `build_graph()` with `SourceAttachment::Attach` (for browser editing) and stores the graph. Link resolution in the conductor remains lazy per-page in `rebuild_page`.

## Alternatives considered

- **Keep separate implementations** — rejected because the duplication has already caused bugs (collection pages missing from conductor) and will continue to diverge.
- **Have the conductor call the CLI's `build_site()` directly** — rejected because `build_site()` is tightly coupled to CLI concerns (rendering, dep_graph, policy, rayon parallelization, file output). Extracting the graph-building phases is cleaner.
- **Put the shared code in `site_index`** — rejected because `site_index` is a data structure component (types, paths, graph operations). Build logic with content parsing and template graph construction is a different concern.

## Consequences

- Single source of truth for graph building — fixes and features apply once.
- `SourceAttachment` enum controls whether markdown source is embedded (conductor needs it for browser editing, CLI does not).
- The CLI loses ~300 lines of inlined build phases, replaced by calls to `site_builder`.
- The conductor loses ~250 lines of duplicated build code, replaced by a single `site_builder::build_graph()` call.
- New dependency: both `publisher_cli` and `conductor` depend on `site_builder`.
- Build policy remains CLI-only — `site_builder` returns raw diagnostics, the CLI decides what to do with them.
- Rayon parallelization can be added to `site_builder::build_graph()` later, benefiting both callers.
