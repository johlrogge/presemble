# ADR-021: Persistent DOM trees with im crate

## Status

Proposed

## Context

The M3.5 code action transformation model requires comparing before/after DOM trees after
applying transforms. Code actions (InsertSlot, Capitalize, InsertSeparator) transform a
Document and the system must determine what changed to produce targeted LSP TextEdits,
file writes, and browser DOM patches.

With `Vec`-backed collections, cloning a document before applying a transform is O(n),
and detecting which elements changed requires a full walk comparing every element.

## Decision

Adopt the `im` crate (v15) for the content Document's element collection.
`Document.elements` changes from `Vec<Spanned<ContentElement>>` to
`im::Vector<Spanned<ContentElement>>`.

Template DOM trees remain as `Vec<Node>` — they are produced fresh by the parser and
consumed by the transformer. There is no before/after comparison for templates in M3.5.

### Why im::Vector

- Cloning a document before applying a transform is O(1) — structural sharing via Arc
- Structural diff can use Arc pointer equality to skip unchanged subtrees
- Slot-level semantic diffing becomes efficient: only walk nodes that actually changed
- `im::Vector` supports indexed access, iteration, push_back, insert, split_off,
  truncate — all operations the slot_editor needs

## Alternatives considered

**Full deep clone + tree diff** — O(n) clone cost on every transform, no way to skip
unchanged subtrees during comparison. Does not scale as documents grow.

**Arc<Vec<...>> manually** — gives O(1) clone but no structural sharing at the element
level. Cannot detect which elements changed without walking the entire tree.

**Adopt im for both content and template DOM** — Template DOM trees are produced fresh
by the parser and consumed linearly. There is no before/after comparison for templates in
M3.5. Deferring template adoption avoids unnecessary churn. Can revisit in M5 if
browser-side template editing needs structural diff.

## Consequences

**Positive:**

- Document cloning becomes O(1), enabling cheap before/after snapshots around every
  transform.
- Foundation is laid for Tier 2 structural diff (SlotAdded, SlotChanged, SlotRemoved).
- Structural diff can use Arc pointer equality to skip unchanged subtrees, keeping diff
  cost proportional to the size of the change rather than the size of the document.

**Negative / open questions:**

- `content` and `template` components gain a dependency on `im` v15.
- `slot_editor.rs` splice operations change to use `im::Vector` split/append.
- Functions accepting `&[Spanned<ContentElement>]` slices need adjustment to accept
  `&im::Vector<Spanned<ContentElement>>` or iterators.
- `im::Vector` is a persistent data structure with higher constant-factor overhead than
  `Vec` for purely sequential workloads. Build-only pipelines that never snapshot may
  pay a small performance cost.
