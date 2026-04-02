# ADR-023: Immutable Transform trait for content operations

## Status

Proposed

## Context

Content operations (insert slot, capitalize, insert separator) are implemented as mutable functions taking `&mut Document`. The LSP dispatches them via a `SlotAction` enum matched in `apply_action`. This works but has limitations:

- Operations are not composable — chaining requires sequential mutation
- The mutable interface prevents cheap before/after comparison (needed for Tier 2 structural diff)
- Parameters are passed as function arguments rather than bound at construction, mixing "what to do" with "do it now"
- The conductor (M4) needs transforms as first-class protocol primitives that can be serialized, queued, and replayed

With `im::Vector` structural sharing (ADR-021), cloning a Document is O(1). This makes an immutable transform interface practical: each transform takes a Document by value and returns a new one.

## Decision

Define a `Transform` trait in the `content` component:

```rust
pub trait Transform: std::fmt::Debug {
    fn description(&self) -> String;
    fn apply(&self, doc: Document) -> Result<Document, TransformError>;
}
```

Each operation becomes a struct with parameters bound at construction:
- `InsertSlot { grammar: Arc<Grammar>, slot_name: SlotName, value: String }`
- `Capitalize { grammar: Arc<Grammar>, slot_name: SlotName }`
- `InsertSeparator` (no fields)
- `CompositeTransform { transforms: Vec<Box<dyn Transform>> }`

Constructors validate against the grammar eagerly — invalid transforms are unrepresentable. The existing `modify_slot` and `capitalize_slot` functions become `pub(crate)` implementation details.

## Alternatives considered

**Keep mutable functions, add a wrapper layer** — adds API surface without removing the mutable interface. Consumers must choose between two APIs. The Transform trait replaces the mutable API cleanly.

**Put the trait in a new component** — would create circular dependencies (`transform` needs `content::Document`, `content` needs `transform` types). Keeping it in `content` avoids this.

**Include grammar in the trait rather than at construction** — makes transforms context-dependent and harder to serialize for the conductor protocol.

## Consequences

The `Transform` trait becomes the public API for content mutations. `modify_slot` and `capitalize_slot` become internal. `lsp_capabilities::apply_action` converts `SlotAction` to a `Transform`, applies it, and serializes.

Transforms are composable via `CompositeTransform`. Each transform is a self-contained operation suitable for the conductor's REPL protocol (M4).

Foundation for Tier 2 structural diff: `let before = doc.clone(); let after = transform.apply(doc)?;` — both clones are O(1).
