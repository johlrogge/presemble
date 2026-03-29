# lsp_service

LSP server implementation for Presemble, built on `tower-lsp`.

Hosts the `PresembleLsp` struct that implements the `LanguageServer` trait. Receives LSP protocol messages from the editor, classifies each open file by path (content / template / schema / unknown), delegates to `lsp_capabilities` for the actual logic, and publishes results back to the editor client.

## File-type dispatch

A single server process handles all three Presemble file types:

| Path prefix | File kind | Capabilities |
|---|---|---|
| `content/` | Content | Completions, diagnostics, hover, go-to-definition, code actions |
| `templates/` | Template | Completions, diagnostics, hover, go-to-definition |
| `schemas/` | Schema | Completions, diagnostics |

Files outside these prefixes receive no diagnostics.

## In-memory document store

The server keeps the latest source text for each open document in memory so that completions and hover work correctly as the author types, before the file is saved.

## Code actions

For content files, the server stores `CapitalizationFix` and `TemplateFix` metadata alongside each diagnostic. When the editor requests code actions, it returns:

- **Capitalize first letter** — applies the capitalization fix as a `TextEdit`
- **Insert `<slot>` template** — inserts the generated slot snippet at the separator line

## Starting the server

The server is started via `presemble lsp <site-dir>` (wired in `publisher_cli`). It communicates over stdio.

## Used by

`publisher_cli`

---

[Back to root README](../../README.md)
