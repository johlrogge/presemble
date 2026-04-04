# dep_graph

Dependency tracking for incremental builds in Presemble.

Maintains a bipartite graph mapping each output page to the set of source files (schema, content file, template, assets) it was built from. On file-system change events, the reverse index identifies which output pages need rebuilding without touching unrelated pages.

## Responsibilities

- Record `output → {source files}` edges after each build
- Provide `rebuild_affected(changed: &Path) -> Vec<OutputPage>` via the reverse index
- Track CSS files as first-class graph nodes, with edges for `@import` and `url()` references (ADR-029)
- Serve as the subscription/notification backbone for WebSocket live reload (M3)

## Used by

`publisher_cli`

---

[Back to root README](../../README.md)
