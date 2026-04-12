# ADR-024: Slot-level structural diff for Documents

## Status
Accepted

## Context
Tier 2 of the three-tier code action pipeline (ADR-023). After applying a Transform to a Document, the system needs to determine what changed to produce targeted output for Tier 3 consumer adapters (LSP TextEdits, file writes, browser DOM patches).

## Decision
A `diff(before, after)` function produces a `DocumentDiff` containing `Vec<Change>`. Six change variants: SlotAdded, SlotChanged, SlotRemoved, SeparatorAdded, SeparatorRemoved, BodyChanged. Exploits `im::Vector::ptr_eq()` structural sharing to skip unchanged subtrees. Carries before/after element vectors so adapters have both source spans and new content.

## Alternatives considered
**Element-level fine-grained diff** — deferred; no consumer needs it. SlotChanged carries both vectors so an adapter can compute finer diffs locally if needed.
**Method on Document** — rejected; diff is a peer operation on two documents, not a method on one.

## Consequences
Enables Tier 3 adapters. Diff cost is proportional to changes, not document size. Foundation for LSP targeted TextEdits that fix the lost-error-markers bug.
