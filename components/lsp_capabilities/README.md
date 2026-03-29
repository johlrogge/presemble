# lsp_capabilities

LSP capability logic for Presemble — completions, diagnostics, hover, and go-to-definition across content, template, and schema files.

This component is pure logic with no I/O or LSP protocol types. It receives source text and grammar values, and returns typed result structs. The `lsp_service` component converts those structs to `tower-lsp` protocol messages.

## Capabilities

### Content files (`content/<schema>/<slug>.md`)

- **Completions** — slot names from the grammar; for link slots, enumerates actual content files and formats them as `[Title](/type/slug)` inserts
- **Diagnostics** — schema violation messages with source positions; capitalization violations include a `CapitalizationFix`; missing slots include a `TemplateFix` with generated snippet
- **Hover** — returns the slot's `hint_text` for the element at the cursor line
- **Go-to-definition** — resolves a link value at the cursor line to the target content file path

### Template files (`templates/<name>.html`)

- **Completions** — data-path completions derived from the schema matching the template file stem; completes `data="…"` attribute values
- **Diagnostics** — flags `data` attribute paths that reference fields not declared in the schema
- **Hover** — returns the field's hint text for a data-path attribute at the cursor line
- **Go-to-definition** — resolves `presemble:include src="…"` references to the target template file; resolves in-file `presemble:define` name references to their definition line

### Schema files (`schemas/<name>.md`)

- **Completions** — element template keywords (heading, paragraph, link, image syntax); constraint key/value completions contextual to the preceding slot type
- **Diagnostics** — parse-error positions from the schema parser

## Used by

`lsp_service`

---

[Back to root README](../../README.md)
