# ADR-044: Schema mode uses canonical schema URLs

## Status

Accepted

## Context

ADR-042 placed all four editorial modes (view, edit, suggest, structure) in URL
fragments on the content URL: `#_edit`, `#_suggest`, `#_schema`. The framing was
"same URL, different lens."

Manual testing of structure mode in Phase D Slice 1 surfaced an architectural
mismatch:

- **View, edit, and suggest** truly are lenses on the same content resource —
  same page, different affordances.
- **Structure** is fundamentally a different resource. The schema is not the
  content; it is the schema. Multiple content URLs (`/post/foo/`, `/post/bar/`,
  `/post/baz/`) all share the same schema (`schemas/post/item.md`).

Furthermore, URL → schema is ambiguous in nested layouts. `/post/category/`
could mean "category sub-collection of post" or "post item with slug 'category'".
The previous heuristic (segment depth, trailing-slash) could not disambiguate
without filesystem awareness.

But **page → schema is deterministic**. The NodeStore document root for any page
has `stem` and `page-kind` attributes set during ingestion. Given a page, the
schema is unambiguous; given a schema, multiple pages may match — a one-to-many
relationship that demands its own URL space.

## Decision

Schema mode uses **canonical schema URLs under `/_schema/`**:

| Schema URL | Meaning |
|---|---|
| `/_schema/index` | Root-level schema (stem = "") |
| `/_schema/<stem>/index` | Collection schema for `<stem>` |
| `/_schema/<stem>/item` | Item schema for `<stem>` |

Routing is **NodeStore-backed**, not URL-string-parsed:

- `Command::SchemaUrlForPage { page_url }` looks up the page in `url_to_root`,
  reads its `stem` and `page-kind` attributes, and returns the canonical schema
  URL.
- `Command::PageUrlForSchema { schema_url }` parses the schema URL, finds a
  best-effort representative content page (collection if exists; first item
  sorted by URL; `None` for unknown stem).

Mode toggle is a **full navigation** (`location.href = ...`), not an in-page DOM
swap. Deep-linking to a schema URL just works — the server's path handler
recognises the `/_schema/` prefix and dispatches to schema rendering directly,
with no JavaScript round-trip.

**Edit and suggest modes keep their hash form** (`#_edit`, `#_suggest`) on
content URLs. They are lenses on the content resource — same URL, different
presentation. That semantics did not change.

`_schema` is a **reserved path segment**. Content URLs cannot contain it (clean
URL convention as per ADR-009). No collision risk.

## Why

URL-to-schema mapping via `#_schema` on content URLs is inherently ambiguous for
nested collections. NodeStore attributes make it deterministic in the opposite
direction: a page always knows its `stem` and `page-kind`. The schema URL
therefore flows naturally from NodeStore state rather than from URL parsing.

Giving schemas their own URL space matches their actual nature: schemas are not
views of content; they are separate documents that describe the structure of many
content items. A canonical address makes schemas bookmarkable, linkable, and
extensible into a future schema browser (Phase D+).

The asymmetry in mode encoding — edit/suggest as URL fragments, structure as URL
path — reflects genuine semantic asymmetry in the resources, not an inconsistency.

## Alternatives Considered

1. **Stick with hash on content URL** (`#_schema`). Rejected — URL → schema is
   ambiguous in nested layouts; the one-to-many relationship of schema to pages
   deserves its own URL space.

2. **`.html` suffix as schema marker** (e.g., `/post/item.html`). Rejected —
   inconsistent with the clean URL convention; looks old-web.

3. **`_schema` segment after the stem** (e.g., `/post/_schema/item`). Rejected
   in favour of root-prefix `/_schema/post/item` — simpler discovery, single
   landing point for all schemas, easier to add a schema browser later.

## Consequences

**Positive:**

- Schema URLs are first-class addresses. Bookmarking `/_schema/post/item`
  bookmarks the schema kind, not an instance.
- NodeStore is the sole source of truth for routing. Filesystem awareness is not
  required for any URL decision.
- `/_schema/` becomes a natural future landing page for a schema browser
  (Phase D+ feature).
- No flash on deep-link to schema URLs: the server renders directly, no JS
  round-trip required.
- Sub-collections can be supported (Slice 2+) without changing URL semantics.

**Trade-offs:**

- Asymmetry between modes: edit/suggest are URL-fragment-bookmarkable; structure
  is URL-path-bookmarkable. This reflects the actual semantics (lens vs separate
  resource) but readers must understand the distinction.
- Old `#_schema`-on-content-URL bookmarks from Phase D Slice 1 break. **Drop
  legacy** — no backward compatibility. Slice 1 was on a feature branch and not
  yet merged to master, so blast radius is minimal.

## Cross-references

- `docs/adr/042-url-fragment-mode-encoding.md` (superseded by this ADR)
- `docs/adr/031-conductor-as-sole-authority.md` (NodeStore as truth for routing)
- `docs/adr/009-clean-url-convention.md` (reserved segments, clean URL rule)
- Future remote-conductor (jack-in) — `SchemaUrlForPage`/`PageUrlForSchema`
  commands are the seam for protocol-level URL resolution

## Out of Scope (tracked for future)

- Sub-collection schemas (`/_schema/post/category/index`) — Slice 2.
- `/_schema/` browser landing page — Phase D+.
