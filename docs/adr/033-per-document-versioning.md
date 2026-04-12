# ADR-033: Per-document versioning with demand-driven snapshots
## Status
Proposed
## Decision

Each content document tracks its own version history independently of git. Versions are demand-driven: they only exist when suggestions reference them.

**What is a document version?** A content hash of the node tree (AST). Same content = same hash. The hash is computed from a compact EDN representation of the node tree.

**Two-tier version references:**
- `GitRef(commit_hash, file_path)` — when the workspace is clean at suggestion time. Free storage — git already has the content.
- `LocalHash(node_tree_hash)` — for uncommitted changes. Snapshot stored in `.presemble/snapshots/{hash}.edn`.

**Suggestion pinning:** Each suggestion records its base document version. This answers: "what did the document look like when this suggestion was made?"

**Three rebase scenarios:**

1. **Same node, same version.** Two suggestions reference the same node and the same document version. No rebase needed — both are valid. When one is applied, the other is rebased per scenario 2.

2. **Document changed in workspace.** A snapshot is stored for the new version. The conductor tries to advance all existing suggestions to the new version. Suggestions whose target node is unchanged advance silently. Suggestions whose target node changed stay pinned to their original version (potential conflict). The new suggestion is added against the new version.

3. **Subtree relationship.** Suggestion B references a subtree of suggestion A's target. When A is applied, check if B's node still resolves in the result — if yes, no conflict. B advances to the post-A version. This enables independent suggestions at different depths of the same tree.

**Staleness rule:** A suggestion is stale if its path no longer resolves. The user decides when to delete stale suggestions.

**Conflict resolution:** Suggestions are merges — base (version A), current (version B), proposed (A + suggestion). Presentation adapts to the client:
- Helix: conflict markers in text + LSP code actions ("pick X's suggestion" | "use your content")
- Browser: side-by-side node comparison, pick at node level

**Garbage collection:** Snapshots without referencing suggestions are dropped.

**Storage:**
```
.presemble/
  snapshots/
    abc123.edn    # content-addressable node tree snapshot (compact EDN)
  suggestions/
    sug-xxx.json  # references base: abc123
```

## Why

1. **Git versions workspaces, not documents.** Editorial work happens per-document. Git commits bundle unrelated changes.
2. **Suggestion validity.** Without version pinning, suggestions degrade silently. With it, staleness and conflicts are detected automatically.
3. **Demand-driven = zero overhead.** Most documents have no pending suggestions.
4. **Git coexistence.** GitRef means no extra storage when the workspace is clean.

## Alternatives considered

- **Event sourcing (operation log)** — rejected. Git proved snapshots beat deltas.
- **Sequential version counter only** — doesn't capture content state. Still need snapshots.
- **Always store snapshots** — wasteful for documents with no suggestions.

## Consequences

- The conductor gains snapshot management responsibility
- `.presemble/snapshots/` becomes a content-addressable store
- Suggestions gain a `base_version` field
- Conflict resolution becomes a first-class editorial workflow
