# ADR-029: Stylesheets as first-class SiteGraph nodes
## Status
Proposed
## Context
Presemble tracks what it references and publishes only what is needed. Templates, content, and schemas are all nodes in the SiteGraph with typed identities and explicit dependency edges.

Stylesheets (CSS) currently bypass this model entirely. Template asset extraction discovers stylesheet paths (e.g., `<link href="/assets/style.css">`), but those paths land in a flat `BTreeSet<String>` alongside images and fonts. A CSS file that references a font via `url()` or imports another stylesheet via `@import` has dependencies — but those dependencies are invisible to the site graph. The build pipeline handles them with a procedural fixpoint loop bolted onto the side.

This is the "asset directory" pattern: copy and hope it is correct. Presemble should *know* it is correct.

## Decision
Stylesheets become first-class nodes in the SiteGraph, symmetric with content.

- A **content node** is something that produces structured data (from .md, .edn, etc.). The file format is serialization; the role is the node.
- A **stylesheet node** is something that produces a CSS DOM (from .css, .scss, etc.). Same principle.
- A **leaf asset** (image, font, video) is a node with no dependencies — a terminal in the graph.

Stylesheet nodes have typed dependency edges to other stylesheets (`@import`) and to leaf assets (`url()`). These edges are part of the SiteGraph, not a separate data structure.

The build is demand-driven from one or more seed files. The default seed is `/index.md`; `presemble build` may optionally accept additional start files to seed the graph walk. Anything not reachable from the seed set is not published. Stylesheets are reachable because templates reference them, and templates are reachable because content uses them. The graph walk discovers the full transitive closure.

There is one model — whether used by the publisher, the LSP, the conductor, or the browser editor. In-memory vs. on-disk is a backing store detail, not a different graph.

## Principles
- **Symmetry**: every publishable entity is a node with a role. No entity type gets special "just copy it" treatment.
- **Nodes are roles, not files**: a stylesheet is "something that produces a CSS DOM." The file format (.css, .scss) is serialization.
- **One model**: the site graph is the single source of truth across all consumers.
- **Reachability = publication**: the graph walk from the root determines what gets published. Unreachable nodes are not built.
- **Schemas can produce**: a schema is not limited to validation — it can produce artifacts (e.g., SCSS compilation). Not initially supported, but the model must allow it.

## Alternatives considered
- **Keep the flat asset set** — bolting CSS scanning onto `all_asset_paths` with a procedural loop. Works mechanically but violates the model. Cannot answer "what pages are affected when this stylesheet changes?" Cannot validate the full dependency closure. This is the pattern every other static site generator uses; presemble should do better.
- **Separate stylesheet tracker outside SiteGraph** — a parallel data structure for stylesheet dependencies. Violates the one-model principle. Two graphs that must stay in sync are worse than one.

## Consequences
- `SiteGraph` gains new entry kinds for stylesheets and leaf assets, or a generalized node type that encompasses all roles.
- `SiteEntry` evolves to accommodate nodes that are not pages (no template_path, no content_path — different shape per role, or a common core with role-specific data).
- The flat `all_asset_paths` set and the CSS scanning loop in publisher_cli are replaced by graph edges.
- The `dep_graph` component's role narrows: it records build-time dependency tracking for incremental rebuilds, downstream of the site graph. The site graph owns the reference model; dep_graph owns the cache invalidation.
- `cssparser` (already added) is used for stylesheet parsing.
- Incremental rebuild gains stylesheet awareness: change `reset.css` → the graph knows which pages transitively depend on it.
- Future: SCSS support slots in naturally as a schema-that-produces, with the same node shape.
