# ADR-016: Unified validation and site-index components

## Status

Proposed

## Context

The LSP and publisher had divergent validation paths. Template field validation was reimplemented
in `lsp_capabilities` using raw string scanning of `data="..."` attributes, while the publisher
validated templates through the parsed template AST and render pipeline. File discovery conventions
(mapping schemas to content directories and template files) were duplicated in three locations:
`publisher_cli::build_site`, `lsp_service::grammar_for_uri` / `grammar_for_template_uri`, and
`lsp_service::revalidate_dependents`.

This divergence created two concrete problems:

1. **Validation drift** — a new schema constraint or template attribute added to one path could be
   missed in the other, causing the publisher and LSP to disagree on what constitutes valid content.

2. **Incomplete cross-file revalidation** — the LSP only revalidated files currently open in the
   editor when a schema changed. Content and template files on disk that depended on the schema
   were silently skipped, while the publisher would catch them.

## Decision

Extract two new polylith components:

### `site_index` — site directory layout

Encapsulates the naming conventions that map schemas to content and templates. Provides file
classification (`FileKind`), schema stem discovery, content file enumeration, template lookup,
and dependency enumeration (`dependents_of_schema`). Both the publisher build pipeline and the
LSP use this single implementation.

Dependencies: `schema`.

### `validation` — shared validation logic

Provides `validate_content`, `validate_schema`, and `validate_template` functions that return
diagnostics with byte-range spans. Both the publisher and LSP call these functions. The LSP
layer adds position mapping and quickfixes on top; the publisher uses the message text.

Dependencies: `schema`, `content`, `template`.

### Refactored layers

`lsp_capabilities` retains LSP-specific presentation logic (position mapping, quickfix
computation, completions, hover, go-to-definition) but delegates all validation decisions to
the `validation` component.

`lsp_service::revalidate_dependents` uses `site_index::dependents_of_schema` to discover all
dependent files (open in the editor and on disk), not just currently-open editor buffers. For
open files, in-memory text is used; for closed files, the source is read from disk.

`publisher_cli::build_site` uses `site_index` for schema, content, and template discovery
instead of inline `read_dir` and path manipulation.

## Alternatives considered

**Inline the publisher's `build_site` into the LSP** — too heavy. The LSP does not need to
render HTML or write output files. The validation logic needs to be extracted from the build
pipeline, not the pipeline itself.

**Make `lsp_capabilities` call `content::validate` and `template::parse` directly** — this is
approximately what was done in Phase 4. The problem is that "validate a template's data paths
against a grammar" is domain logic that belongs in a shared component, not in an LSP
presentation layer.

**Single "site" component for everything** — too broad. `site_index` (static structure) and
`validation` (checking correctness) are orthogonal concerns with different dependency sets.

## Consequences

**Positive:**

- Single source of truth for all validation rules. A new schema constraint is implemented once
  in `validation` and surfaces in both publisher and LSP.
- Single source of truth for site layout conventions. Adding a new file type requires updating
  one component.
- LSP cross-file revalidation covers all files, not just open ones.
- `lsp_capabilities` shrinks — it keeps completions, hover, go-to-definition, and quickfix
  computation, but loses validation logic.

**Negative / open questions:**

- Two new components add to the workspace size. Both are small and focused.
- `validate_template` currently uses raw attribute scanning (matching the Phase 4 implementation)
  rather than walking the parsed template AST. A future improvement could use the template DOM
  parser for attribute extraction, but the current approach validates the same paths as the
  renderer and is adequate for field existence checking.
- The publisher's template validation still happens implicitly during render (a missing field
  causes a render failure). Pre-render validation via `validation::validate_template` is
  available but not required for the publisher's use case.
