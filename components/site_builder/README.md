# site_builder

Unified graph-building pipeline for Presemble. Shared by the CLI and the conductor so both use identical build logic.

See [ADR-037](../../docs/adr/037-unified-build-pipeline.md) for the design rationale.

## What it does

Extracts the three shared graph-building phases that previously existed in duplicate across `publisher_cli` and `conductor`:

| Function | Phase | Description |
|---|---|---|
| `build_graph(repo, output_dir, source_attachment)` | 1a, 1b, 1c | Builds item pages, collection pages, and legacy root fallback. Returns a `GraphBuildResult` with the `SiteGraph` and per-page diagnostics. |

Link expressions are evaluated in the NodeStore pipeline (`node_store_bridge::store_pipeline`, ADR-058 "Evaluation and invalidation"), not on the `SiteGraph`.

The `SourceAttachment` enum controls whether markdown source text is embedded in data graph nodes. The conductor passes `Attach` (needed for browser editing); the CLI passes `Omit`.

Build policy (what to do with diagnostics, rendering, dep_graph, file output) remains in the CLI. `site_builder` returns raw `GraphBuildResult` values and does no I/O beyond reading from the `SiteRepository`.

## Used by

`publisher_cli`, `conductor`

---

[Back to root README](../../README.md)
