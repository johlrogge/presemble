# ADR-049: Temporal publishing: time-parameterized validation, ordered never-broken publish, placeholder unification

## Status
Proposed

## Decision

Publishing is a **query over the archive**, not a separate pipeline of state.

**Publish-time is a node dimension.** Each node carries a publish-time as a dimension/edge. What actually gets published is the result of a **temporal query**: "the nodes relevant at time T." Querying *when* nodes are meant to appear **derives the publishing schedule** — scheduling is not a subsystem, it is a query over publish-time edges. (This is distinct from source-version history, ADR-048/ADR-033 — a different timeline.)

**Validation is time-parameterized.** A publish is simply a **time-slice that type-checks** against the schemas. The same validator that already refuses statically-missing mandatory content is reused; the time dimension only decides which nodes count as *present* at T. If the slice is missing mandatory nodes/edges, the **query succeeds but the publish is refused** — "make illegal states unrepresentable," applied across time. The schedule is the set of times at which the validated slice changes.

**Never-broken ordered publish.** Outputs are written in **dependency order** so that no published page ever references something not yet published. This is a topological sort of the dependency graph (ADR-026); schemas are part of knowing the order. **Assets are graph leaves** (sinks, no outgoing edges) and are therefore always published *first* (ADR-034). The guarantee: the site is never in a half-broken intermediate state for any visitor.

**Placeholder unification.** A **placeholder = "not yet real."** Text placeholders and asset placeholders (MISSING / STORED-but-not-READY, ADR-034) are the same shape: a mandatory slot not yet filled with something real. They gate **publish** (mandatory-and-real to publish) but **never the workbench** — the workbench will happily materialize a whole dummy site from schema with every slot empty. The asset publish gate is this one rule applied to a binary slot, not a special case.

**Deletion and permanent URLs.** Referential integrity forbids deleting something still referenced. But external bookmarks / typed URLs cannot be controlled, so removal is handled with **meta pages** — markdown + front-matter tombstones / permanent redirects ("I moved"). A redirect is itself queryable content. Leaning invariant: **a URL, once published, is a permanent obligation** — removing it demands a successor meta page. (Knowing "what was ever published" is inherent in a content-addressable, append-only archive — ADR-048/ADR-050.) Exact meta-page representation is left open.

## Why

1. **Scheduling for free.** Time-parameterized validation means there is nothing new to build for scheduling — you parameterized the validator you already have.
2. **Never-broken is a differentiator.** Referential-integrity-during-rollout is the static-typing guarantee applied to publishing; few publishers offer it.
3. **One rule, text and binary alike.** Placeholder-must-be-real-to-publish unifies content and asset gating and powers "materialize a dummy site from schema."
4. **Permanent URLs protect the open web.** Tombstone meta pages keep old links alive across moves/removals.

## Alternatives considered

- **A separate scheduling subsystem** — rejected; the schedule falls out of querying publish-time edges.
- **Publish "latest done" only** (overwrite, no time dimension) — loses scheduling and time-travel publish; rejected, publish-time is intrinsic to the model.
- **Auto pull-forward / hold-back** to force a closed reference set — rejected; an invalid temporal slice is a validation error the *author* resolves, not engine magic.

## Consequences

- Nodes carry publish-time edges; a **temporal query engine** over the archive (leans on the node store + EDN/expression evaluation, the conductor-as-REPL).
- The validator runs against a time-slice; the publish gate refuses an invalid slice (extends ADR-016 unified validation).
- **Topological publish ordering** from the dependency graph (ADR-026); assets first (ADR-034).
- A **meta-page / tombstone** node kind; "once-published is permanent" relies on the append-only archive's memory (ADR-048/ADR-050).
- Builds on ADR-007 (crawl-based publishing), ADR-009 (clean-url convention), ADR-016 (unified validation), ADR-026 (site graph), ADR-034 (asset gate), ADR-048 (archive / CAS).
