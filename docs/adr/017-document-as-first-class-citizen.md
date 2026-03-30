# ADR-017: Document as first-class citizen

## Status

Accepted

## Context

Content files on disk are markdown. The system previously treated these files as raw text, editing
them via byte-offset splicing. This was fragile — multi-occurrence slots got misaligned, insertions
landed in the wrong place, and separate code paths diverged for operations that were conceptually
the same.

The insight: a content file is a serialized representation of a `Document` (`Vec<ContentElement>`).
The `Document` is the source of truth for editing; the markdown file on disk is derived from it.

## Decision

All content editing operates at the `Document` level:

1. Parse markdown → `Document` (`Vec<ContentElement>`) via `content::parse_document`
2. Modify the `Document` via `content::modify_slot` or `content::capitalize_slot`
3. Serialize back to canonical markdown via `content::serialize_document`

The serialized form is the canonical format. Auto-format on save normalizes content files through
this pipeline. The LSP's `did_save` handler parses, serializes, and writes back if the output
differs — analogous to `rustfmt` for content files.

Key components:

- `content::parse_document` — markdown → `Document`
- `content::serialize_document` — `Document` → canonical markdown
- `content::modify_slot` — modify a named slot in a `Document`
- `content::capitalize_slot` — capitalize a slot's first character

## Alternatives considered

**Byte-offset splicing (the original approach)** — fragile, duplicated logic across code paths,
and broke on multi-occurrence slots where the same slot name appears more than once in a file.
Offsets shifted after each edit, making sequential modifications unreliable.

**AST diff and incremental edits** — unnecessary complexity. Full serialization produces correct,
deterministic output without needing to track which bytes changed.

## Consequences

**Positive:**

- Single correct code path for all edit operations. Adding a new operation means implementing it
  against the `Document` type once; it is immediately available everywhere.
- Auto-format on save keeps content files in canonical form. Diffs stay meaningful — only content
  changes, never whitespace noise from inconsistent hand-editing.
- Multi-occurrence slots are handled correctly because the `Document` model tracks slot identity,
  not byte positions.

**Negative:**

- Editing any slot reformats the entire file (whitespace normalization). This is intentional and
  acceptable — canonical form is the goal — but it means the first save through the pipeline may
  produce a large diff even when only one slot changed.
