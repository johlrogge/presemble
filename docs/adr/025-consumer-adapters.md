# ADR-025: Three-tier consumer adapters for content transforms

## Status
Accepted

## Context
The three-tier code action pipeline (Transform -> Diff -> Adapter) needs consumer-specific output formats. The LSP needs targeted TextEdits to fix the lost-error-markers bug. The file writer needs to serialize documents to disk. The browser client (M5) needs DOM patches.

## Decision
Three consumer adapters convert DocumentDiff into format-specific output:

1. **LSP adapter**: `diff_to_source_edits(src, before, after, diff) -> Vec<SourceEdit>` produces byte-range edits that the LSP service maps to TextEdits. Falls back to full-document replacement for complex structural changes (SlotAdded, SlotRemoved).

2. **File writer**: `FullDocumentWriter` serializes the entire document. The interface exists for future partial-write optimization.

3. **Browser adapter**: Stub returning empty Vec. Implemented in M5.

`SourceEdit { span: Span, new_text: String }` is the LSP-independent intermediate representation. It carries byte ranges, not LSP line/column positions — the mapping to `TextEdit` happens in `lsp_service`.

## Alternatives considered
**Adapters in separate components** — would need access to content internals (serialize_element, byte ranges). Keeping them in content avoids exposing private helpers.

**Element-level diff for fine-grained edits** — deferred. Slot-level granularity is sufficient for current transforms (InsertSlot, Capitalize, InsertSeparator).

## Consequences
The LSP code action path produces targeted TextEdits instead of full-document replacement, fixing the lost-error-markers bug. The file writer and browser adapter interfaces are ready for M4 and M5.
