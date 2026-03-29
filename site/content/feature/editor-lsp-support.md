# Editor LSP Support

Your editor knows Presemble's schema.

`presemble lsp` starts a Language Server Protocol server that provides completions,
diagnostics, hover, and go-to-definition for content, template, and schema files —
all in a single server process, dispatched by file path.

A single LSP server understands all three Presemble file types: content files get slot
completions and schema-violation diagnostics; template files get data-path completions
and field-existence diagnostics; schema files get keyword completions and parse-error
diagnostics.

----

### Content files

Open a content file in `content/<schema>/`. The LSP offers completions for every slot
name declared in the matching schema. For link slots, it enumerates the actual content
directory and inserts `[Title](/type/slug)` directly. Hover over any element to see
the schema's hint text. Go-to-definition on a link jumps to the linked content file.

Diagnostics mirror the build: missing required slots, wrong occurrence counts,
capitalization violations, and broken link references all appear inline as you type.
Capitalization violations include a quickfix action — one keypress to fix.

### Template files

Open a template in `templates/`. The LSP reads the schema that matches the template's
file stem and offers completions for `data="…"` attribute values. If a data path
references a field that does not exist in the schema, the attribute is flagged as an
error. Hover on a data-path attribute shows the field's hint text. Go-to-definition on
a `presemble:include` jumps to the referenced template file or to the `presemble:define`
block within the same file.

### Schema files

Open a schema in `schemas/`. The LSP offers element keyword completions (heading
syntax, paragraph syntax, link pattern, image glob) and constraint completions
contextual to the slot type just declared. Parse errors in the schema surface as
diagnostics at the exact failing line.

### One binary, all file types

The server classifies each open document by its path within the site directory. No
separate language server instances, no per-file-type configuration. Start it once and
it covers the whole site.
