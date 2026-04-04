# schema

Grammar types and schema parser for Presemble.

Parses `schemas/<name>.md` files into a `Grammar` struct describing the sequence of named slots, their element types (heading, paragraph, link, image), and their constraints (occurrence counts, content rules, heading level ranges).

## Responsibilities

- Parse schema markdown into `Grammar` / `Slot` / `Element` / `Constraint` types
- Represent occurrence constraints (`exactly once`, `1..3`, `at least once`, `*` for unbounded)
- Represent content constraints (`capitalized`, heading level ranges, image orientation)
- Represent list/set slots (markdown list items with `{#name}` anchor and `occurs: *`)
- Provide the shared vocabulary used by `content`, `template`, and `lsp_capabilities`

## Used by

`content`, `template`, `lsp_capabilities`, `lsp_service`, `publisher_cli`

---

[Back to root README](../../README.md)
