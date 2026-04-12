# ADR-022: Slots as named children in Document

## Status

Accepted

## Context

The content `Document` type holds a flat `im::Vector<Spanned<ContentElement>>`. Five consumers independently re-derive slot membership by duplicating the same cursor-walk algorithm: matching elements against schema grammar slots using type checks, annotation paragraph skipping, separator detection, and max-count extraction.

This duplication is a maintenance burden and a source of subtle bugs. More critically, slot-level semantic diffing (M3.5 Tier 2) requires knowing which elements belong to which slot. Without this structure in the type, every diff consumer would need to re-derive slot boundaries yet again.

## Decision

Restructure `Document` to carry named slots as children:

```rust
pub struct DocumentSlot {
    pub name: SlotName,
    pub elements: im::Vector<Spanned<ContentElement>>,
}

pub struct Document {
    pub preamble: im::Vector<DocumentSlot>,
    pub body: im::Vector<Spanned<ContentElement>>,
    pub has_separator: bool,
}
```

Parsing becomes two phases:
1. `parse_document` performs pure markdown parsing, returning a flat `FlatDocument`
2. `assign_slots(flat_doc, grammar)` groups elements into named slots, producing the structured `Document`

This separates concerns: the markdown parser does not need to know about schemas, and slot assignment is performed exactly once.

## Alternatives considered

**Overlay view on flat document** — a `SlottedView` struct wrapping `&Document` that lazily computes slot boundaries. Avoids changing the core type but does not eliminate the cursor walk and adds API surface. Consumers must choose which view to use, and the flat representation remains the source of truth.

**Grammar-aware parser** — `parse_document` takes a grammar and produces the slotted structure directly. Mixes markdown parsing with schema concerns, making the parser harder to test and reuse.

## Consequences

Enables slot-level semantic diffing for M3.5 Tier 2 (SlotAdded, SlotChanged, SlotRemoved) by making slot membership explicit in the type.

Eliminates five instances of duplicated cursor-walk code across validator, slot_editor, data graph builder, and validation component.

Requires a grammar at document construction time. Call sites that only need body elements can use the flat `FlatDocument` from phase 1 parsing.

A `flat_elements()` convenience method provides backward compatibility during migration.
