# content

Document parser and schema validator for Presemble content files.

Parses `content/<schema>/<slug>.md` files into typed `ContentElement` sequences and validates them against a `Grammar` from the `schema` component. Reports structured validation errors with source positions (used by both the build pipeline and the LSP).

## Responsibilities

- Parse content markdown into a typed element sequence
- Validate element sequence against a grammar: occurrence counts, content constraints, heading levels, link patterns, image glob patterns
- Map byte offsets to `(line, character)` positions for LSP diagnostic ranges
- Cross-content reference parsing: extract link targets for resolution

## Used by

`lsp_capabilities`, `publisher_cli`

---

[Back to root README](../../README.md)
