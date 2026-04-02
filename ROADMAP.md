# Roadmap

## Done

**M0 — "It compiles a site"**

Schema format decided (ADR-001), content validation with hard fail and clear errors, DOM template engine (ADR-005), `presemble build` CLI, clean URLs (ADR-009).

Deliverables shipped:
- [x] Schema definition format decided and documented (ADR-001)
- [x] Read markdown from a content directory
- [x] Validate content against schemas — hard fail with clear error messages
- [~] Cross-content reference validation — link validation implemented (pages must exist); automatic name resolution from referenced content deferred to M2
- [x] Template rendering to static HTML (ADR-005)
- [x] `presemble build` CLI command
- [ ] Dogfood test: build a subset of blog.agical.se content (superseded by M0.5)

---

**M0.5 — "Presemble builds its own site"**

site/ contains the presemble.io promotional site with three content types (feature, post, author), six pages, clean URLs, and Link validation: OK. This was the real dogfood test.

Deliverables shipped:
- [x] Build the presemble.io promotional site using Presemble itself
- [x] Three content types (feature, post, author), four feature highlights, six pages
- [x] Hiccup/EDN as second template surface syntax (ADR-011) — proves surface syntax is a parser choice
- [x] Nature-inspired CSS for presemble.io
- [x] `presemble build` produces a deployable presemble.io site with no workarounds

---

**M1 — "It serves and watches"**

`presemble serve`, file watching with 150ms debounce, incremental rebuild with file-level dependency tracking (ADR-008), clean URLs (ADR-009).

Deliverables shipped:
- [x] `presemble serve` — local HTTP server with file watching and live rebuild
- [x] File-level dependency tracking for incremental rebuilds (ADR-008)
- [x] 150ms debounce on file-system events
- [x] Clean URL routing (ADR-009)
- [x] Data-driven asset discovery from template DOM trees (ADR-010)
- [x] Dot-path separator for data graph paths (`article.title` not `article:title`)
- [x] 10 ADRs recorded

---

## Done — M2: "Cross-content references and template composition"

**Goal:** make content items aware of each other at render time, so templates can pull data from
linked content (e.g. show an author's name from the author page, not hardcoded in the article).
Introduce proper template composition and collection query support.

**Success gate:** a template can render `author.name` by following a content reference from an
article to its author page automatically, without any workarounds in the content files.

**Deliverables:**
- [x] Cross-content reference resolution — templates can pull data from linked content items (e.g. render author name from author page, not hardcoded in article) (ADR-012, shipped v0.2.0)
- [x] `site.*` as ordinary content — `site.md` is a normal content file with a schema, no special-casing. Any user can declare `schemas/site.md` and it works like any other content type.
- [x] Collection queries — collections live at the **root level** of the data graph: `data-each="features"` not `data-each="site.features"`.
- [x] Template composition — `presemble:define` declares named callable fragments; `presemble:apply` invokes them with explicit data context. File-qualified references (`templates/common::header`). `presemble.self` carries the passed context. Duck-typing validation deferred.
- [ ] Semantic types with display defaults — deferred to a future milestone.

---

---

## Done — M3: "Live editorial feedback loop"

**Goal:** Close the feedback loop between editing and seeing results. The author should never
wonder "did my change work?" — the system tells them immediately, whether they are in a browser
or an editor.

The dependency graph (ADR-008) already tracks which outputs depend on which sources — it doubles
as the subscription/notification system. No separate pub/sub needed.

*Phase 1: WebSocket + live reload* -- shipped v0.4.0
- Inject a small script into served pages during `presemble serve`
- Push reload events over WebSocket when the dep_graph detects changed outputs
- The browser reloads only affected pages, not the whole site

*Phase 1.1: Smart navigation* -- shipped v0.4.1
- Server sends changed page URL(s) in WebSocket message (JSON: `{type, pages, primary}`)
- If current page changed → reload in place; otherwise navigate to the first changed page
- Protocol is forward-compatible: `anchor` field added in Phase 2 for element focus
- Full-rebuild path falls back to reload-in-place

*Phase 2: Source map annotations + focus on changed element* -- shipped v0.4.2
- Annotate rendered DOM elements with source-file provenance (which content/template produced this node)
- In-memory rebuild fast path: DOM diffs served directly to the browser, no disk write required for preview
- On navigate, scroll to and highlight the changed element using the source map anchor
- Enables "I changed this line, I see the result instantly" without a full page reload

*Phase 3: Content LSP* -- shipped v0.5.3
- `presemble lsp` exposes an LSP server for content files
- Content-aware completions: slot names, cross-content references
- Template quick fixes for common errors
- Go-to-definition for content references and template paths
- Shared between browser clients and editor clients (Helix, VSCode)

*Phase 4: Template and schema LSP* -- shipped v0.6.0
- Extend LSP support to template files: completions for data-paths, `presemble:apply` references, callable template names
- Schema-driven diagnostics in templates: flag references to fields that do not exist in the schema
- Schema file LSP: completions for field types, occurrence markers, reference targets
- Structural field existence checking (Layer 1 of template type system): the access path IS the constraint

*Phase 5: Suggestion nodes (error UX)* -- shipped v0.7.0
- No error pages. Errors become inline suggestion nodes in the rendered page
- Missing content renders as schema-driven placeholders (hint_text, examples from schema)
- Visually distinct — soft, inviting, clearly not real content
- The page always renders — a page with no content renders as a fully scaffolded guide
- Warm, helpful tone — not compiler output
- Pure rendering improvement: no interactive editing yet (that is M5)

**Success gate:** An author using Helix gets completions and diagnostics for content, templates,
and schemas. An author viewing a served page sees inline suggestion nodes instead of error pages
for missing content.

---

## Done — M3.5: "Code action transformation model"

**Goal:** Unify all content transformations (LSP code actions, browser edits, auto-format) under
a single pure-functional pipeline. Fix the lost-error-markers bug in the LSP. Build the
foundation that M5 browser editing will use for its edit pipeline.

**Why now:** M5 browser editing needs a well-defined transformation pipeline — browser edits
are content transforms that must flow to disk and back to the browser. Building that pipeline
also fixes the lost-markers bug in the shipped LSP (M3), which currently replaces the entire
document buffer on every code action, wiping all other diagnostics.

**Prerequisites (infrastructure):**

- [x] Parser source span tracking — parsers carry byte-range source positions; unified content parser eliminates ~250 lines of duplication (ADR-017)
- [x] `im` crate adoption for persistent DOM trees — structural sharing makes before/after comparison efficient and O(1) document cloning (ADR-021)
- [x] Deeper DOM tree structure — slots as named children via `DocumentSlot`; two-phase parsing with `assign_slots` eliminates duplicated cursor-walk from five consumers (ADR-022)

**Tier 1: Transform trait**

- [x] Code actions as structs implementing `Transform` trait: `fn apply(&self, doc: Document) -> Result<Document, TransformError>` (ADR-023)
- [x] All parameters bound at construction (slot name, value, `Arc<Grammar>`) — no ambient context
- [x] `CompositeTransform` holding `Vec<Box<dyn Transform>>` for chaining via `try_fold`
- [x] Existing code actions (InsertSlot, Capitalize, InsertSeparator) migrated to the new model; `modify_slot`/`capitalize_slot` became `pub(crate)` internals

**Tier 2: Structural diff**

- [x] Compare before/after DOM trees exploiting `im` structural sharing (`im::Vector::ptr_eq` skips unchanged subtrees) (ADR-024)
- [x] Slot-level semantic diff: SlotAdded, SlotChanged, SlotRemoved, SeparatorAdded, SeparatorRemoved, BodyChanged

**Tier 3: Consumer adapters**

- [x] LSP adapter: `diff_to_source_edits` produces targeted byte-range edits; `lsp_service` maps to LSP TextEdits using source spans (ADR-025)
- [x] File writer adapter: `FileWriter` trait with `FullDocumentWriter` (full serialization; partial write optimization deferred)
- [x] Browser adapter: `DomPatch` enum and `diff_to_dom_patches` stub — ready for M5

The Transform trait also defines the primitive operation vocabulary for M4's conductor protocol —
transforms are the REPL's built-in functions.

**Success gate:** Existing LSP code actions (InsertSlot, Capitalize, InsertSeparator) work
through the new pipeline. Applying a code action no longer wipes other diagnostics. The browser
adapter interface exists as a stub ready for M5 to implement.

---

## M5: "Browser editing"

**Goal:** The served page IS the editor. Content authors who never touch a terminal can create
and edit content directly in the browser.

**Depends on:** M3.5 code action transformation model — browser edits use the Transform pipeline,
structural diff, and browser adapter defined there.

**Why now:** Suggestion nodes shipped in M3 Phase 5 are the direct foundation. `presemble serve` already has WebSocket and content in memory — no conductor needed to start.

**The Presemble mascot:**
- Floating overlay in the corner of every served page
- View mode: hugging face / peace sign — shows validation count badge
- Edit mode: pen icon — content nodes become inline-editable
- Suggest mode: speech bubble — annotations and suggestions for other editors
- All clear: thumbs up — page is ready to publish

**Three interaction modes:**

*View mode* — just browsing, no editing possible. Default.

*Edit mode* — content nodes are inline-editable:
- Click suggestion nodes (from M3 Phase 5) to fill in missing content
- Simple fields: contenteditable, what you type is what gets stored
- Link fields with bounded options: select/dropdown
- Basic inline markdown (`*bold*`, `_italic_`) in text content
- Submit writes through conductor to disk, live reload rebuilds, page updates

*Suggest mode* — for editorial review:
- Mark for correction: click a node to flag it (optional comment). Flag becomes a diagnostic in Helix.
- Suggest changes: propose alternative text. Arrives in Helix as a quickfix action.
- Browser is just another author in the conductor pipeline.

**Suggestion persistence:**
- Suggestions live in `.presemble/suggestions/` as files
- Survive conductor restarts — conductor reads on startup, re-emits as LSP diagnostics
- Committed to git if the site uses git (part of the editorial record)
- Not git-dependent — works with plain folders too

**Deliverables:**
- [ ] Presemble mascot overlay with mode toggle
- [ ] Edit mode: inline editing of simple content fields
- [ ] Suggest mode: mark-for-correction and suggest-changes
- [ ] Suggestion persistence in `.presemble/suggestions/`
- [ ] Conductor integration: browser edits are transforms sent over the conductor's EDN protocol

**Success gate:** A content author can open a served page, click the mascot to enter edit mode,
fill in missing content via suggestion nodes, and see the page update live. An editor in Helix
sees browser suggestions as diagnostics with quickfixes.

---

## M6 — "CSS asset tracking"

**Goal:** Close the asset-discovery gap for CSS. Today, Presemble only copies assets referenced
from template DOM trees (ADR-010). Assets referenced via CSS `url()` — fonts, background images,
cursors — are invisible to the build. This breaks the "clean output + complete site" guarantee.

**Why now:** This is a correctness gap in shipped functionality (M1 asset discovery). A site
with CSS-referenced fonts or images produces either broken output or requires workarounds.
The fix is small, self-contained, and load-bearing.

**Deliverables:**
- [ ] New `css` polylith component — CSS parsing is its own concern
- [ ] Parse CSS files and discover `url()` references (fonts, images, cursors)
- [ ] Recursive `@import` walking — follow import chains to discover all referenced assets
- [ ] Feed discovered CSS assets into dep_graph alongside template-discovered assets
- [ ] Error on missing assets referenced from CSS (same behavior as template asset references)
- [ ] Copy only what is used — no blind copying of the asset directory

**Success gate:** A site with fonts or images referenced only from CSS `url()` builds correctly.
Missing CSS-referenced assets produce clear build errors. No assets are copied that are not
referenced.

---

## M4 — "The conductor"

**Goal:** Unify all Presemble processes under a single shared-state conductor. Today `presemble lsp`
and `presemble serve` are separate processes with separate state. The conductor makes them thin
clients of one authoritative process.

The conductor is a REPL runtime. Clients jack in and interact with live site state via an
S-expression protocol. Transforms (from M3.5) are write operations; queries drive rendering.
The protocol form is designed so a future expression language evaluates the same syntax — no
intermediate command format to translate later. This needs further design work (see
conductor-as-repl brainstorm note).

**Conductor owns:**
- dep_graph (single source of truth)
- Schema cache
- In-memory working copies of content files (for live-update-while-typing)
- File watcher

**Clients connect via nng (nanomsg-next-gen) IPC:**
- `presemble lsp site/` — thin bridge: Helix stdio to nng socket to conductor
- `presemble serve site/` — query client: renders pages by querying conductor state, not by serving files
- `presemble repl site/` — CLI REPL client (future, see Deferred — the protocol already exists)

**Protocol — S-expressions over nng:**
```clj
;; Transforms (write operations — M3.5 primitives)
(insert-slot "title" "Hello")
(capitalize "title")

;; Queries (read operations — serve client uses these)
(render :page "/blog/my-post" :at :now)
(query :from "/blog/my-post" :path "author.name")
(deps :page "/blog/my-post")
```

**nng topology:**
- PUB/SUB for broadcasts (file changed, rebuild done)
- REQ/REP for commands (transforms and queries)
- Version counter per file for sync/conflict detection

**Startup model (Kakoune-inspired):**
- First process to connect starts the conductor automatically
- Socket at `$XDG_RUNTIME_DIR/presemble/<site-hash>`
- Conductor shuts down after idle timeout when no clients remain
- `presemble lsp site/ --start-serve` flag starts serve at the same time
- No LSP restart required when serve connects later — they just find each other

**Conflict model:**
- Filesystem always wins — disk is the source of truth
- Optimistic concurrency: each edit carries the version it's based on
- Edits to different nodes on same version = auto-merge
- Edits to same node = conflict — second writer gets diagnostic notification
- Node-level semantic diffing (not line-level): prose normalized, code blocks exact
- Conflicts live in editor memory only as LSP diagnostics with quickfixes

**Deliverables:**
- [ ] ADR for conductor architecture and REPL protocol
- [ ] S-expression command protocol: EDN parsing, dispatch to transforms and queries
- [ ] nng IPC layer with PUB/SUB and REQ/REP
- [ ] Conductor process: dep_graph, schema cache, file watcher, in-memory content
- [ ] `presemble lsp` as thin nng client
- [ ] `presemble serve` as query client of conductor
- [ ] Version counter and conflict detection

**Success gate:** `presemble lsp` and `presemble serve` share state through the conductor.
Editing a file in Helix updates the browser preview before save.

---

## M7 — "Asset discovery"

**Goal:** Authors discover and insert images from the browser without leaving the page. Unsplash
integration surfaces relevant images directly from image content slots.

**Why now:** Must be ready before demo videos. Browser editing (M5) is the natural insertion
surface — asset discovery extends it without requiring new infrastructure.

**Deliverables:**
- [ ] Unsplash API integration — search by keyword, browse results in browser
- [ ] Schema-aware insertion — respects orientation, alt text requirements, and other schema constraints on image slots
- [ ] Browser editing integration — clicking an image slot opens the asset search UI
- [ ] Downloaded images stored under site assets with schema-correct metadata

**Success gate:** Author clicks an image slot in edit mode, searches Unsplash, selects an image,
and it is inserted with correct metadata — without leaving the browser.

---

## M8 — "Time enters the picture"

**Goal:** Time becomes a first-class dimension of the site. Authors can schedule content and
preview the future state of the site.

- Publish timestamps on content items
- `presemble build --at <datetime>` — render the site as it will appear at a given moment
- Timeline scrubber in the `presemble serve` UI — time-travel is a query parameter: `(render :page ... :at "2026-05-01")`
- Publisher maintains a timetable of future publish events
- Content saves push events into the timetable
- Dual publish triggers: clock-driven (timetable) and event-driven (git push or content save)

**Success gate:** An author can set a publish date on a blog post, use the timeline scrubber to
preview the site at that date, and the publisher automatically publishes when the time arrives.

---

## M9 — "CSS as a first-class language"

**Goal:** CSS participates in the knowledge graph. Authors and designers get class/ID completions
across CSS and templates, and schema types produce discoverable CSS class names.

**Why now:** CSS is a consistent pain point. With CSS asset tracking shipped (M6) and LSP
infrastructure mature (M3), the foundation exists to make CSS a peer of content, templates,
and schemas in the knowledge graph.

**Deliverables:**
- [ ] dep_graph supports heterogeneous node types (classes, IDs, selectors alongside files and assets)
- [ ] Templates register their class/ID vocabulary into the knowledge graph
- [ ] CSS selectors register their class/ID vocabulary into the knowledge graph
- [ ] Bidirectional LSP completions: CSS editor suggests classes/IDs from templates; template editor suggests classes/IDs from CSS
- [ ] Schema-driven `presemble-` class generation — schema types produce predictable class names stamped on rendered nodes (e.g. `presemble-article`)
- [ ] `presemble-` prefix as natural LSP completion trigger in CSS files
- [ ] Live CSS hot reload in `presemble serve` — style changes apply without full page rebuild
- [ ] Diagnostics for unused CSS selectors and missing styles for schema-defined slots

**Success gate:** Editing a CSS file shows completions for classes used in templates. Editing
a template shows completions for classes defined in CSS. Schema types produce `presemble-`
prefixed classes that are discoverable via CSS completions.

---

## M10 — "Distribution"

**Goal:** Anyone can install and run Presemble with a single command. No Rust toolchain required.

**Why now:** Must ship before promotion. A tool that requires `cargo install` from source is not
ready for general use.

**Deliverables:**
- [ ] Cross-platform binaries (Linux, macOS, Windows) via CI
- [ ] Nix flake for declarative installation
- [ ] GitHub Releases with binary downloads
- [ ] Install guide in README: one-liner for each platform

**Success gate:** A user with no Rust installed can install Presemble and build a site in under
five minutes following the install guide.

---

## Deferred (post-MVP)

These are real parts of the vision, not cut — just not needed to prove the core value:

**Type system layers 2-3:**
- Semantic types with display defaults (iso-date with human-readable formatting)
- Behavioral constraints on callable templates: `publish-time:Date+Ord+Eq`
- Constraint inference through callable template chains
- Arithmetic/algebraic types: product types, sum types, destructuring
- Schema-validated template expressions (type-check data paths at build time)
- Collection pipe transforms (`sort`, `take`, `filter`) with schema-aware validation

**Structural template editing:**
- Template tree editing in the browser (drag-and-drop DOM tree manipulation)
- Schema-to-template scaffolding (drag schema into template, auto-populate insert nodes)
- Homoiconic editing: the edit surface and the content model are the same structure

**REPL CLI client:**
- The conductor (M4) already speaks an S-expression protocol — the REPL runtime exists
- This deferred item is adding a CLI/editor client that jacks into the conductor
- Calva and CIDER integration is possible because the protocol is nREPL-shaped
- Evolution path: fixed vocabulary (M4) → composition and conditionals → embedded Lisp engine

**Named body sections (brainstorm):**
- The `----` separator could be extended with named sections: `---- {#named-section}`
- Multiple body sections with different constraints per section
- Schema could specify: `## Introduction {#intro}` section allows h3..h4, `## Details {#details}` allows h2..h6
- Enables structured body content without losing the free-form feel

**Expressive link patterns in schemas (brainstorm):**
- Current: `[<name>](/author/<name>) {#author}` — placeholder syntax, display text is literal
- Proposed: `[author.name](/author/*) {#author}` — graph-aware link patterns
  - Display text is a graph path reference (`author.name` resolves to the linked author's name field)
  - URL uses wildcard `*` for the slug instead of repeated `<name>` placeholders
  - The schema declares the relationship explicitly: this link displays a field FROM the referenced content
  - Enables the template to know what to render without hardcoding: `post.author` link shows the author's actual name
  - Validation can check: does the referenced content type have a `name` field?
  - Completions can show: author names from the content, not just slugs

**Link validation and quickfixes (brainstorm):**
- Build and LSP should validate content references (e.g., `[Author](/author/johlrogge)` → does the author exist?)
- Different diagnostic levels:
  - Schema exists but content file missing → offer quickfix: "Create /author/johlrogge" (opens new buffer with suggestion placeholders)
  - Schema exists, close match found → offer quickfix: "Did you mean /author/johlrogge?" (fuzzy match)
  - Schema doesn't exist → hard error
- LSP link completions already enumerate existing content — extend to validate references at build time
- "Create" quickfix would scaffold a new content file from the schema and open it in the editor

**LSP code action robustness:** → Promoted to M3.5 (code action transformation model).

**Live nodes — backend-backed template regions (brainstorm):**
- Some template nodes could be backed by a live data source in production (database, API, etc.)
- Example: a product catalog region pulls from a database instead of static content files
- The template declares the node as "live" — at build time it renders a placeholder or static snapshot, at serve/production time it fetches from the backend
- Enables hybrid static/dynamic sites without leaving the Presemble model
- Schema still validates the shape of the data — the source just changes from file to backend

**Full-text search with FST indexes (brainstorm):**
- Use finite state transducers (https://burntsushi.net/transducers/) to produce compact search indexes at build time
- Each content type could have its own index (search posts, search authors, search features separately)
- The index is a static artifact — no server-side search needed, works with any hosting
- Could power an in-browser search UI or a `/_presemble/search` endpoint in serve mode
- The `fst` crate (Rust) implements this — small dependency, battle-tested

**Other deferred items:**
- Real-time multiplayer editing
- Comments, suggestions, track changes (beyond M5 suggest mode)
- Remote content system (cloud hosting)
- Security, OAuth, role-based access
- Data-shaped content (typed records, e.g. product catalog)
- Event-driven publish triggers (content-save → republish)
- Local/cloud profile split in polylith
