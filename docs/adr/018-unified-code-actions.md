# ADR-018: Unified code actions for LSP and browser

## Status

Accepted

## Context

Browser editing and LSP code actions were separate code paths for the same underlying operations.
Browser editing used `modify_slot` + `serialize_document` (the Document pipeline from ADR-017).
LSP code actions used `TemplateFix` and `CapitalizationFix` structs with byte-offset `TextEdit`
insertions. Two paths for the same operations meant they could diverge and produce different
results.

## Decision

LSP code actions and browser editing use the same pipeline. Both go through:

1. `SlotAction` enum (`InsertSlot`, `Capitalize`, `InsertSeparator`)
2. `apply_action(src, grammar, action)` — parse → modify → serialize
3. Full-document replacement as the edit mechanism

The LSP `code_action` handler calls `apply_action` and returns a `WorkspaceEdit` that replaces
the entire file content. The browser edit endpoint calls `modify_slot` + `serialize_document`
directly. Both use the same underlying `Document` operations.

`TemplateFix`, `CapitalizationFix`, and all byte-offset helpers have been removed.

## Alternatives considered

**Keep byte-offset fixes, share helpers** — still two paths, still fragile. The byte-offset model
cannot represent the same operations as cleanly as the Document model, so convergence without full
unification would be partial at best.

**Incremental `TextEdit`s from a Document diff** — unnecessary. The LSP transport model is already
`textDocumentSync: FULL`, so full-document replacement is the correct mechanism. Synthesizing
incremental edits would add complexity without benefit.

## Consequences

**Positive:**

- One code path for all content mutations. Browser editing and LSP code actions produce identical
  results by construction.
- New edit operations are added once (to `SlotAction` + `apply_action`) and become available in
  both the browser and the LSP without further wiring.
- `TemplateFix` and `CapitalizationFix` and their byte-offset helpers are gone. Less code, fewer
  places to maintain.

**Negative:**

- Code actions lose LSP snippet placeholders — they insert real text rather than `${1:...}` tab
  stops. This is acceptable because code actions fix known, diagnosed problems; they are not
  interactive completion items. Completions continue to use snippets.
