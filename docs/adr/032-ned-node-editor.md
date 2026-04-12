# ADR-032: NED — Node Editor instruction set for DAG transforms
## Status
Proposed
## Decision

NED (Node Editor) is a RISC instruction set for transforming node trees (DAGs). It is the non-visual editing kernel underneath all Presemble editors — browser, LSP, REPL, MCP. The name parallels SED (stream editor): SED edits streams of text, NED edits trees of nodes.

**Selection-first model** (inspired by kakoune/helix): every operation starts with selecting nodes, then transforming them. Selections can be refined: start with a path query, narrow with text search, combine with multi-select.

**RISC primitives:** A small set of orthogonal operations:
- `select` — path-based node selection in the DAG (multi-select supported)
- `insert` — add a node at a cursor position
- `replace` — replace content of selected nodes
- `remove` — delete selected nodes
- `move` — relocate selected nodes to a target position
- `wrap` / `unwrap` — add or remove a parent node around a selection

Higher-level operations (swap, etc.) desugar into RISC primitives.

**Homoiconic:** NED operations are EDN data, serializable, diffable, composable:
```clojure
(-> (select /post/hello #title)
    (replace "New Title"))

(-> (select /post/* #summary)
    (remove))

(-> (select #body/section-a #body/section-c)
    (move-after #body/intro))
```

**Scale-invariant:** The same vocabulary works for one character in one slot or a bulk operation across 1000 documents. The scope is just the selection.

**DAG as primary representation:** Node trees are the primary document representation. The filesystem is one materialization. NED operations transform between node tree states. Diffing two snapshots produces a NED program (minimal RISC sequence).

**Filesystem integration:** When Helix saves a file, the conductor diffs old and new node trees to derive the minimal NED program. This is a pure optimization problem.

**Suggestions as NED programs:** A suggestion is a pending NED operation pinned to a document version. Current search/replace suggestions are replaced by structural NED operations that survive document changes (node paths are stable, unlike text positions).

## Why

1. **Unification.** Every editing feature (browser edits, LSP code actions, suggest mode, REPL commands, MCP suggestions) currently builds its own transform logic. NED provides one vocabulary for all.

2. **Homoiconicity.** NED operations are data (EDN), the same format used throughout Presemble. Operations can be stored, transmitted, composed, and diffed.

3. **Structural precision.** Text-based search/replace suggestions are fragile — they break when surrounding text changes. Node-addressed operations survive changes to other nodes.

4. **REPL editing.** The nREPL already evaluates expressions against the site graph. NED makes the graph mutable from the REPL.

5. **Multiplayer foundation.** NED operations are the unit of collaboration. Conflict detection and resolution operate at the node level, not the line level.

## Alternatives considered

- **Keep ad-hoc transforms** — each client implements its own editing logic. Rejected: duplication grows with each new client, no composability.
- **CISC instruction set** — rich, high-level operations for every use case. Rejected: large surface area, hard to compose. RISC primitives with desugaring is more flexible.
- **Text-based OT/CRDT** — character-level operational transformation. Rejected: operates below the semantic level. Node-level operations are rarer conflicts and more meaningful merges.

## Consequences

- All editing operations must be expressible as NED programs
- The conductor becomes the NED runtime
- Existing transforms (InsertSlot, SlotEdit, etc.) are migrated to NED instructions
- The Transform trait (ADR-023), structural diff (ADR-024), and consumer adapters (ADR-025) are subsumed by NED
- The REPL gains mutation capabilities alongside queries
