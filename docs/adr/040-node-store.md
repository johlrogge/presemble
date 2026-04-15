# ADR-040: Node Store — persistent DAG as primary site model

## Status

Proposed

## Context

Presemble currently represents content, templates, and schemas as separate type hierarchies (`ContentElement`, `DocumentSlot`, `template::dom::Node`, `Grammar`). The conductor holds parsed documents in memory but each subsystem has its own tree structure with its own traversal logic.

NED (Node Editor) needs a universal editing kernel — all editors (browser, LSP, REPL, MCP) must speak the same language of nodes and operations. This requires a unified, in-memory graph that can represent everything: content, templates, schemas, rendered pages, and the relationships between them.

Key requirements from the NED design:
- Selection-as-subgraph: filter the graph, get a smaller graph
- COW templates: structural sharing via persistent data structures
- Cross-document references: hrefs, imports, link expressions (potentially circular)
- Derived relationships: CSS styling, schema validation edges
- Suggestions as graph transformations pinned to snapshots
- The whole site fits in memory (<5s load budget)

## Decision

Create a `node_store` polylith component that provides a persistent, in-memory DAG built on the `im` crate. This is the primary site model — the source of truth while the conductor is running.

### The node store is the site

Files on disk are "restoration scripts" — serializations for git and text editors. HTML pages are another serialization. The node store is the live model (analogous to a Smalltalk image). On startup, the conductor deserializes restoration scripts into the node store. All editing happens in memory. Saving serializes back to files.

```
content + schema → semantic data
semantic data + template → page

template path + content path + schema path = page path
```

Sources and pages are both serializations — different formatters applied to the same graph.

### Node model

Nodes are pure values. All structure lives in edges.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);  // internal only, never serialized, never in NED programs

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(NameId);  // interned for fast comparison

pub enum Node {
    Element(Name),
    Text(String),
    Keyword(Name),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Nil,
}
```

7 variants. No inline children or attributes — those are edges.

### Edge model

Edges are a unified collection per node. Children, attributes, references, and derived relationships are all edges.

```rust
pub enum Edge {
    Child(NodeId),
    Attribute { name: Name, value: NodeId },
    Reference { name: Name, target: NodeId },
    Derived { name: Name, target: NodeId },
}
```

Stored as `im::Vector<Edge>` per source node. Child order is position in the vector (among Child variants). Filtered views provide `children()`, `attributes()`, `references()` without separate collections.

This model handles:
- **Tree structure**: Child edges define parent-child containment
- **Named properties**: `heading --[:level]--> 2` is an Attribute edge
- **Cross-document links**: `link --[:href]--> /post/other` is a Reference edge (circular references are just two edges)
- **Derived knowledge**: `div --[:styled-by]--> .my-class` is a Derived edge (ephemeral, recomputed, never serialized)

### Store structure

```rust
#[derive(Clone, Debug)]
pub struct NodeStore {
    nodes: im::HashMap<NodeId, Node>,
    edges_from: im::HashMap<NodeId, im::Vector<Edge>>,
    edges_to: im::HashMap<NodeId, im::Vector<Edge>>,  // reverse index
    names: NameInterner,  // string interning for Name
    next_id: u64,
}
```

- **Persistent**: built on `im` — clone is O(1), structural sharing across versions
- **Immutable updates**: every mutation returns a new NodeStore
- **Dual edge index**: forward (edges_from) for rendering/tree-walking, reverse (edges_to) for parent lookup, impact resolution, back-references
- **Snapshots are free**: keeping a reference to an old store preserves that version

### NodeId is internal only

NodeIds are an implementation detail. They never appear in:
- Files on disk (restoration scripts)
- NED programs (selections use structural paths: names, types, positions)
- The REPL interface
- EDN serialization

NED navigates by structure, not by identity. This aligns with schemas, which are also purely structural.

### Preamble/body dissolved

There is no preamble/body distinction in the node store. A document is a tree of nodes — some named (slots), some anonymous (body elements). Schemas can name sections at any depth. NED traverses all nodes uniformly.

### Three roles, same structure

| Role | What it is in the graph |
|------|------------------------|
| Schema | Constraint tree — valid names, cardinality, types |
| Template | Pre-filled tree with holes (slots) |
| Content | Values filling schema-defined structure |
| Rendered page | Template tree with content materialized at slots (COW) |

Schema validation is walking the schema tree alongside the content tree and checking conformance.

### COW templates

Applying a template to content:
1. Clone the template subtree (O(1) via `im`)
2. Walk the slots, wire edges to existing content nodes
3. Only slot branches are new — everything else is shared structure

### Impact resolution

The reverse edge index (`edges_to`) enables "which pages need re-rendering when this node changes?" — follow reverse edges from the changed node up to page roots. This is the dependency graph for incremental builds, derived from the same edge structure.

### Future: KV-store backing

The node-and-edge model maps directly to key-value storage (`node:{id} → Node`, `edges:{id} → Vec<Edge>`). If multiplayer requires shared state across conductor instances, the same interface can be backed by Redis, SQLite, or similar without changing the API.

## Alternatives considered

- **Keep separate type hierarchies** — each subsystem (content, template, schema) maintains its own AST. Works today but prevents unified traversal, cross-document queries, and the NED editing model. Every new feature requires touching multiple type systems.

- **Content-addressed NodeId (hash-based)** — elegant for deduplication and diffing, but identity changes on every edit (hash cascade to root), two identical headings in different posts get the same ID, and path-based selection becomes a moving target. Content hashing can be computed on demand for snapshots without using it as identity.

- **Inline children/attributes on Element nodes** — the traditional DOM model. Simpler for tree-only structures but cannot represent circular cross-document references, derived relationships, or uniform edge traversal. Attributes become a special case with their own query path.

- **Separate collections for children vs edges** — children in `im::Vector<NodeId>`, other edges in `im::Vector<Edge>`. Adds a structural distinction where none is needed — children are just edges. One collection, filtered views.

## Consequences

- New `node_store` component with dependency on `im` only — zero domain coupling
- Existing types (`ContentElement`, `DocumentSlot`, `template::dom::Node`) continue working; bridge via `From`/`Into` conversions added incrementally to consuming components
- The conductor becomes the custodian of the live NodeStore
- All NED operations (selection, mutation, suggestions) build on this foundation
- Serialization to/from markdown, EDN, HTML lives in consuming components, not in node_store
- The reverse edge index enables impact resolution for incremental rendering
- First validation: round-trip test (read site/ → node store → write → compare)
