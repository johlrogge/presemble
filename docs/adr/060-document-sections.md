# ADR-060: Document sections

## Status

Accepted

## Context

Since ADR-001 a document has two parts: a structured preamble of named slots in
schema order, then `----`, then a free body governed by heading rules. ADR-022
made this shape typed, and the code hard-codes it:

- `schema::Grammar { preamble: Vec<Slot>, body: Option<BodyRules> }`
  (`components/schema/src/grammar.rs`). The schema parser switches to body rules
  on a line that is exactly `----` (`components/schema/src/parser.rs`).
- `content::Document { preamble, body, has_separator, separator_span }`
  (`components/content/src/document.rs`).
- The content parser turns **every** CommonMark thematic break (`----`, `---`,
  `***`, `___`, `- - -`) into `ContentElement::Separator`
  (`components/content/src/parser.rs`, `Event::Rule`).
- `assign_slots` ends the preamble at the first `Separator`
  (`components/content/src/slot_assignment.rs`). Later `Separator`s stay in
  `body` and render as a hidden `<span data-presemble-slot="body" hidden>`
  (`components/template/src/data.rs`).
- Preamble elements that fit no slot are **dropped**: the scan forward to the
  separator discards them (`slot_assignment.rs`). With no separator, everything
  after the last filled slot is lost. Bare `{#name}` paragraphs are also
  skipped silently.
- The serializer writes slots in order, then `----`, then the body
  (`components/content/src/serializer.rs`).
- The node-store frame is `document > preamble > slot*` plus `document > body`,
  with a `has-separator` attribute
  (`components/node_store_bridge/src/content_bridge.rs`). ADR-057 §3 treats
  `document`, `preamble` and `body` as frame.
- Templates read `input.<slot>` and `input.body`; the template validator accepts
  a preamble slot name or `body` (`components/validation/src/validation.rs`).
  The data graph puts slots and `body` in one namespace (`data.rs`).
- NED: `ned/slot doc "body"` is special-cased, other names are looked up under
  `preamble`, and `ned/body-at` indexes into the body
  (`components/evaluator/src/ned.clj`).
- Separator detection is inconsistent: the schema accepts only `----`, content
  accepts any thematic break, and `find_separator_span` uses a third,
  hand-picked set (`validation.rs`).

The author's mental model is more general: "a `----` is a divider between
sections, and a section can be addressable by index or name; a `----` could
have a `{#name}`, which makes everything under it reachable as `name::xxx`."
Today's document is one instance of that model.

Two standing principles apply:
- **Invalid is saveable** (ADR-057, memory `feedback_invalid_is_saveable`).
  Content the schema doesn't allow is kept, edited and saved; it only blocks
  publishing.
- **Every reference counts** (ADR-058 §8; author decision 2026-10-02).
  References from every page count in `refs-to`, publishable or not; the
  publish gate handles the rest.

## Decision

### 1. A document is a sequence of sections

A document is an ordered sequence of **sections** separated by **dividers**.
Section 0 starts at the top of the file. Section *k* starts after the *k*-th
top-level divider. Dividers are frame, not content: there is no `Separator`
element inside a section.

- **Unnamed divider:** any top-level CommonMark thematic break (`----`, `---`,
  `***`, `___`, `- - -`). This keeps today's reading, so nothing that parses
  today changes meaning. The canonical written form is `----`.
- **Named divider:** a top-level paragraph whose whole source is one line
  matching:

      named-divider := '-'{4,} [ \t]+ '{#' keyword '}' [ \t]*
      keyword       := [A-Za-z0-9_-]+   (the slot-marker charset, `Keyword::is_valid`)

  pulldown-cmark does not see this as a thematic break, so Presemble recognises
  it from the paragraph source (as ADR-058 §4 does for link expressions). The
  canonical form is `---- {#name}`.
- **Setext pitfall:** a divider must follow a blank line. `text\n----` is a
  level-2 setext heading (today's behaviour, kept), and `text\n---- {#x}` is a
  lazy continuation of the paragraph. The serializer always writes a blank line
  before a divider. If a paragraph's last line matches `named-divider`, a
  diagnostic hints "add a blank line before the divider".
- **No `<hr>` yet:** a thematic break is never rendered as `<hr>` (today it
  renders nothing either). Whether free sections need a visual rule is an open
  question.
- **Section 0:** it has no divider and no name, unless the document starts with
  a named divider (see resolved question 3). Its slots are addressed by bare
  name: `title` means `0::title`.

### 2. Each schema section has one of three content models

```rust
enum ContentModel {
    Structured(Vec<Slot>),        // slots in order; nothing else allowed
    Free(FlowRules),              // prose; today's BodyRules
    Mixed(Vec<Slot>, FlowRules),  // slots in order, then a free "rest"
}
struct SectionDef { name: Option<SectionName>, model: ContentModel }
struct Grammar   { sections: Vec<SectionDef> }  // non-empty; [0] is unnamed
```

`FlowRules` is today's `BodyRules`, renamed. `Grammar::preamble()` and
`Grammar::body()` remain as accessors during the migration.

### 3. Schema syntax builds on today's

- `----` or `---- {#name}` in a schema starts a new section. Section 0 is
  everything before the first divider, as today.
- How a section's content model is decided:
  - no slot lines → **Free**. Section-level terms (`headings` / `: h3..h6`) are
    flow rules, exactly as in today's body;
  - slot lines and no `rest` term → **Structured**;
  - slot lines plus the section-level term `rest` / `: allowed` → **Mixed**.
    Flow-rule terms in that section apply to the rest.

  A section-level term is a term that follows a blank line and is not attached
  to a slot. Today such lines are skipped silently, so this breaks nothing.
- **Schema errors:**
  - a flow rule in a Structured section ("add `rest: allowed`");
  - two sections with the same name;
  - a section name that collides with a section-0 slot name, with `body`, or
    with `rest`;
  - an unnamed section at index ≥ 2.
- **Backward compatibility:**
  - a schema with no divider means `[Structured(preamble)]`;
  - today's schemas, with one bare `----`, mean
    `[Structured(preamble), Free(body rules)]`. The bare `----` at index 1 is
    implicitly named `body`.

  Every existing schema, template (`input.body`) and NED call
  (`ned/slot doc "body"`) keeps its meaning.

### 4. Matching content sections to schema sections

This mirrors the ADR-058 §9 markers: **`{#name}` means the same thing on a
divider as on a link expression. It binds by name, wherever it appears.**

- **Named dividers bind first:** a named content section binds to the schema
  section with that name, regardless of position.
- **Unnamed dividers fill in order:** unnamed content sections fill the
  remaining schema sections (index ≥ 1) in order.
- **Not allowed:** a name that matches no schema section, a second section with
  an already-bound name, and unnamed sections beyond the schema's last section.
  Such a section is kept and saved, never rendered, and reported as an Error.
- **Missing sections:** a schema section absent from the content keeps today's
  behaviour, a Warning. A missing named section that has required slots is an
  Error (resolved question 7).
- **Serialization:** sections are written in schema order. Sections the schema
  names explicitly are written with `---- {#name}`; the implicit `body` is
  written with a bare `----`, so existing files don't churn. Not-allowed
  sections are written last, with their divider as read.

### 5. Validation and publishing for each content model

"Not allowed here" means: **kept, editable, saved, never rendered, reported as
an Error, and therefore unpublishable** (ADR-057 §5).

| Model | Assignment | Not allowed |
|---|---|---|
| Structured | Positional slot fill, plus marker binding (ADR-058 §9) | Any element that fills no slot. This is the author's case "an unslotted paragraph is just a paragraph unless the schema doesn't allow it there". |
| Free | Everything is flow | Nothing structurally. Flow-rule violations (heading range) are diagnostics on the element, as today. |
| Mixed | Positional slot fill; the first element that fits no remaining slot starts the rest, and everything after it is rest | Nothing structurally. Flow rules apply to the rest. A later element of a slot's type counts as rest, not as a slot; use a marker to bind it explicitly. |

Section level: a whole section that isn't allowed (decision 4) is an Error.

References inside not-allowed content still count for `refs-to` (ADR-058 §8;
author decision 2026-10-02). Not rendering content does not stop it referring.

### 6. Addressing

```
address  := [ section '::' ] member
section  := keyword | digits          ; name, or index (0 = first section)
member   := keyword                   ; a slot name, or `rest` in a Mixed section
```

- Section 0 slots: `title` is the same as `0::title`.
- A Free section is addressed by its name alone (`body`, `notes`), as `body` is
  today.
- **Templates (names only):**
  - `input.<slot>` for section 0;
  - `input.<section>` for a Free section's HTML; `input.body` is unchanged;
  - `input.<section>.<slot>` and `input.<section>.rest` for Structured and
    Mixed sections.

  Index paths are not valid in templates, because they are fragile under
  editing. The template validator checks paths against `Grammar::sections`.
- **NED:**
  - new `ned/section doc name-or-index`;
  - `ned/slot doc "name::slot"` parses the address, and `ned/slot doc "body"`
    keeps working;
  - `ned/body-at` becomes `ned/section-at doc section idx`, with the old name
    kept as an alias for `body`.
- **Extending ADR-056:**
  - InsertChild containers are a slot, a Free section, the rest of a Mixed
    section, or a not-allowed container;
  - blocks are the direct children of these;
  - sections themselves are frame: ADR-057 §3 gains `section` (and, in P1,
    `unassigned`).
- Schema sections always exist in the store, empty if absent from the source,
  as slots do today. Editors fill them and never create them.
- Exact addressing is unchanged: a section address that matches zero nodes is
  `Unresolved`, and one that matches several is `Ambiguous`.

### 7. Phasing

**P1 (next package): close the structured preamble. No new syntax.**

- `Document` gains `unassigned: im::Vector<Spanned<ContentElement>>`: section-0
  elements that fill no slot, including marked link expressions whose marker
  does not bind and marker-shaped paragraphs (`MisplacedMarker`: bare
  `{#name}` paragraphs, markers on images, plain links or text, invalid
  markers, markers on their own line or after a multi-line expression).
  Nothing is dropped any more. This replaces the scan forward and the skip in
  `slot_assignment.rs`.
- Unassigned elements are written after the last slot, before the divider, in
  source order. This is a fixed point for slot assignment: elements are
  classified intrinsically (marker, kind), and strictly positional assignment
  places a prefix of the remaining elements, so the unplaced ones are already
  a suffix. Their textual position may change on save, like slot order
  (resolved question 6). Exception: same-kind lists made adjacent by
  reordering merge (limit L10).
- **Node store:** gains a frame element `preamble > unassigned`. ADR-057 §4's
  well-formedness gate covers its children.
- **Each unassigned element:** is an Error ("not allowed in the preamble"), is
  never rendered, and counts for `refs-to`.
- **Not changed in P1:** later `----` in the body stay `Separator` elements in
  `body`.
- In the general model, P1's `unassigned` is section 0's not-allowed container,
  so P2 renames it rather than reworking it.
- Known exception: HTML blocks are not parsed yet (limit L12) and are lost on
  save; a follow-up package makes them elements.

**P2 (later package): sections.**

- **Types:**
  - `Grammar::sections` and `ContentModel`;
  - `Document::sections`, with a not-allowed container per section;
  - `preamble`, `body` and `has_separator` become accessors, then are removed.
- **Parsing:**
  - named-divider recognition;
  - one shared divider predicate used by the schema parser, the content parser
    and the validator; `find_separator_span` is deleted;
  - schema syntax for `rest` and section-level terms.
- **Binding and addressing:**
  - section matching (decision 4);
  - addressing (decision 6);
  - `input.<section>.*` in the data graph.
- **Store, code actions and migration:**
  - store frame `document > section*`;
  - code actions and diff kinds (`InsertSeparator`, `SeparatorAdded`,
    `SeparatorRemoved`) become section operations;
  - a migration check that reports content files with a second top-level
    divider before the switch. Under P2 such a file splits the body instead of
    becoming a hidden span.

## Why

- The author already thinks of dividers as section boundaries. Preamble and
  body are one instance of that, and the general model covers the instance
  without changing existing files.
- The model makes "not allowed here" precise: content is not allowed exactly
  when the section's content model has no place for it. That gives one
  saveable, unpublishable state instead of silent loss.
- `{#name}` means the same thing on slots, link expressions and dividers:
  bind by name, wherever it appears. Authors learn it once.
- Typed content models replace `preamble: Vec<Slot>` plus
  `body: Option<BodyRules>` with a type that only admits valid section shapes.
  A section cannot be half structured.

## Alternatives considered

- **Keep the two-part shape and only add `unassigned`.** This solves P1 but
  blocks named sections and `name::slot`, which the author wants. P1 is a
  strict subset of this ADR, so there is no reason to stop there.
- **Positional-only sections, with no `{#name}` on dividers.** Inserting or
  omitting a section silently shifts every later one, so addresses drift.
  ADR-056 rejects addressing that guesses.
- **Make only `----` a divider and turn other thematic breaks into `<hr>`
  content.** This changes how existing files read, since today `***` is a
  separator. Deferred as an open question, not rejected.
- **Nested sections (`----` inside `----`).** Not needed by any current schema,
  and it conflicts with flat `name::slot` addressing. See the open questions.

## Consequences

- ADR-001's "`----` marks the boundary between the structured preamble and the
  free body" becomes the default two-section case. ADR-001 and ADR-022 are
  **amended, not superseded**.
- P2 updates the doc comments that encode the preamble/body assumption (in
  `slot_assignment.rs`, `serializer.rs`, `grammar.rs`, and the `InBody` state
  in `schema/src/parser.rs`).
- ADR-057 §3 frame elements gain `unassigned` (P1) and `section` (P2).
- ADR-058 §4 ("a link expression must sit in a preamble slot") reads, in P2,
  "in a slot of a Structured or Mixed section". §9's marker rule applies within
  the section the expression sits in.
- The inline-segments ADR (ADR-059, S-A) can extend markers to other elements
  without touching section syntax: sections are block-level frame, segments
  are inline.
- Slot names and section names share the `input.` namespace. That is already
  true for `body`; decision 3 makes a collision a schema error.

## Related

ADR-001, ADR-022, ADR-040, ADR-056, ADR-057, ADR-058, ADR-059 (inline segments);
memory `feedback_invalid_is_saveable`, `project_ned_exact_addressing`.

## Resolved questions (author, 2026-10-02)

1. **Nesting:** no. Sections are flat.
2. **Name scope:** section names are unique per schema.
3. **Naming section 0:** a document (and schema) may start with a named
   divider, e.g. `---- {#preamble}`, which names section 0. Its slots are then
   addressable as `preamble::title`. Bare slot names (`title`) keep working for
   section 0.
4. **Repeating sections:** yes. A schema section may take `occurs`, like a slot
   (e.g. a repeatable `---- {#faq}`). Addressing a repeated section needs an
   instance index; the syntax is still to be designed (see open questions).
6. **Reordering:** the serializer restores schema order.
7. **Missing-section severity:** a missing named section that has required
   slots is an **Error**.

## Open questions

- **(5) Visual rules.** The author answered "`-----`". This is pending
  clarification: possibly exactly `----` is the divider and `-----` (five or
  more dashes) is a visual `<hr>` in free text. If so, how are `***`, `___` and
  `---` treated?
- **(new, from 4) Addressing instances of a repeated section.** For example,
  `faq[2]::question` or `faq::2::question`. Decide this, together with how named
  dividers bind to repeated instances.
- **(follow-up)** Any paragraph ending in `{#name}` is marker misuse (author
  decision 2026-10-02); implemented after P1.
