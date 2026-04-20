# ADR-041: NED-or-escape-hatch for slot edits

## Status

Accepted

## Decision

`apply_slot_edit` dispatches on a `SlotShape` discriminator:

- `TextOneChild` (slot has exactly one child of kind `heading` or `paragraph`) — lower to a NED program (`ned/set-text` on the text descendants). This is the fast path and the canonical direction.
- `Empty | Multi | NonText | Missing` — delegate to a grammar-aware escape hatch `node_store_bridge::modify_slot_in_store`, which reuses the proven `content::slot_editor::build_element` logic and wires the resulting element directly into the NodeStore.

Phase C will widen NED's coverage (grammar-aware element construction, link/image/list mutations) and reduce the escape-hatch surface. The dispatch list in `conductor.rs` is the authoritative catalogue of shapes still delegated.

## Why

Phase B3 routed browser edits through NED (ADR-032) with the NodeStore as the source of truth (ADR-040). To protect against silent wrong behaviour during the migration, B3.2 narrowed the NED slot-edit lowering to the text-only 1-child case and returned errors for everything else.

That narrowing regressed the most common editing path: first edit of a scaffolded content file. Scaffolding creates empty slots (`# {#title}` with no body); users then populate them. Returning `"empty slot 'X' not supported — Phase C"` broke the UX.

The invariant we actually want is stronger than "error on anything NED can't do":

> **NED is the primary path. Where NED can't yet express an edit, delegate to the proven `slot_editor` logic rather than erroring.**

This preserves correctness while keeping NED as the default and making the divergence explicit (and tracked).

## Alternatives considered

- **Keep the narrowing, defer users to Phase C** — unacceptable UX. The scaffold→edit flow is daily-use territory.
- **Extend NED now to cover all slot shapes** — requires grammar-aware primitives (`ned/slot-element-kind`), int-attr support in `NodeTree`, and new prelude helpers. Real Phase C work; not on the critical path for "Phase B ships."
- **Hybrid: Rust builds `NodeTree` from grammar, passes to NED** — collapses to the escape hatch in practice, with extra indirection.

## Consequences

- **Dual path.** Two code paths produce slot-edit results. For `TextOneChild` they are semantically equivalent (spot-checked via tests); for other shapes only the escape hatch runs. The shapes routed through the escape hatch are enumerated in an inline comment at the dispatch site in `conductor.rs` — treat that comment as the authoritative Phase C checklist.
- **Temporary divergence risk.** If future changes update the NED path without updating the escape-hatch path (or vice versa), behaviour can drift. Mitigation: property-based tests comparing paths when Phase C converges them; until then, the narrow text-only NED surface keeps the overlap small.
- **Minimum public-surface cost.** `content::slot_editor::build_element` and the module itself are promoted to `pub`. `node_store_bridge::add_text_attr` is promoted to `pub(crate)`. No new cross-crate dependencies were added.
- **Smoketest restored.** `tools/smoketest.sh` passes 21/21 (was 19/21).
- **Convergence criteria.** Phase C is done when every shape currently routed through the escape hatch can be expressed as a NED program, the escape hatch's callers in `conductor.rs` dispatch unconditionally to the NED path, and the `SlotShape` discriminator is collapsed.

## Related

- [ADR-032](032-ned-node-editor.md) — ned-node-editor (the RISC instruction set)
- [ADR-040](040-node-store.md) — node-store (the persistent DAG)
