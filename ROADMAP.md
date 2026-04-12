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

## Done — "Template unification, SiteGraph, and SiteRepository"

Shipped in v0.14.0 through v0.18.3.

**Template format unification (v0.14.0)**
- [x] Bidirectional HTML↔EDN template conversion (`presemble convert --to edn/html`)
- [x] Hiccup parser fix: attribute namespace separator uses `:` (was `/`)
- [x] Hiccup line comment support (`;`)
- [x] presemble.io site dogfoods EDN templates exclusively

**Unified directory-based naming (v0.14.0)**
- [x] `schemas/{stem}/item.md` for item schemas, `templates/{stem}/item.hiccup` for item templates
- [x] `schemas/{stem}/index.md` for collection schemas, `templates/{stem}/index.hiccup` for collection templates
- [x] `content/index.md` flat at root (no directory named `index`)
- [x] `resolve_template_file` chain-of-parsers (tries hiccup then HTML)

**Collection pages (v0.15.0)**
- [x] Per-type collection page building: `content/{stem}/index.md` + schema + template → `/{stem}/index.html`
- [x] Collection content requires a schema (consistent with items)
- [x] Curated homepage features via multi-occurrence resolved link slots

**SiteGraph (v0.16.0, ADR-026)**
- [x] Unified `SiteGraph` as single source of truth for all site data
- [x] Three-phase build: build all entries → resolve all references once → render all entries
- [x] `SchemaStem` and `UrlPath` newtypes eliminate stringly-typed HashMap keys
- [x] Reference resolution covers items, collections, and site index uniformly

**SiteRepository abstraction (v0.17.0–v0.18.x)**
- [x] `fs_site_repository` component: filesystem-backed SiteRepository
- [x] `mem_site_repository` component: in-memory builder for tests
- [x] Polylith interface wiring: `live` profile uses fs, `dev` profile uses mem for tests
- [x] `build_site` accepts `&SiteRepository` — testable without filesystem
- [x] Production binary built with `--profile live --release`

---

## Done — v0.19.0: "M5 Phase A and Form type system"

Shipped in v0.19.0.

**M5 Phase A: Interactive suggestion nodes**
- [x] Suggestion nodes interactive in edit mode — click, type, save
- [x] CSS polish: hint pseudo-elements hidden during editing, suggestion styling overrides
- [x] Empty element cursor placement fix
- [x] Synthesized record editing — `_source_slot` provenance maps browser edits to real grammar slots
- [x] `synthesize_link()` shared function eliminates duplication between publisher and conductor
- [x] Anchor wrapping — link records with `as` override produce proper `<a><inner>` HTML

**Form type system (EDN reader)**
- [x] `Form` enum replaces `String` in `Element.attrs` — typed attribute values
- [x] Backwards-compatible `attr()` bridge — zero disruption to existing code
- [x] Extended hiccup parser reads symbols, lists, sets, integers, keywords in attribute values
- [x] `parse_edn_form()` for re-parsing HTML string attributes as EDN
- [x] Hiccup serializer round-trips all Form variants

**`:apply text` (Display rendering)**
- [x] `Value::display_text()` — universal text representation for all value types
- [x] `presemble:insert` accepts `:apply text` to render Display instead of structural form
- [x] Works in both hiccup (`:apply text`) and HTML (`apply="text"`) templates
- [x] Preserves `_source_slot` for browser editing

**Bug fixes**
- [x] File watcher now triggers rebuild for `.hiccup` and `.css` files
- [x] ADR-029 (stylesheets) and ADR-005 (templates are data) promoted to Accepted
- [x] M6 (CSS asset tracking) marked as shipped in roadmap
- [x] Generated polylith profile directories added to .gitignore

---

## Done — v0.20.0: "List fields and pipe expressions"

Shipped in v0.20.0.

**List/set fields in schemas**
- [x] `Element::List` — new schema element type for multi-value fields
- [x] Schema syntax: `- hint text {#name}` with `occurs: *` for unbounded lists
- [x] Content syntax: standard markdown lists (`- item`)
- [x] Validation: item count checked against `occurs` constraint
- [x] Data graph: list items wrapped as Records for `data-each` compatibility
- [x] LSP completions and snippets for list slots

**Pipe expression evaluation (Layer 2)**
- [x] `(-> text to_lower capitalize)` — threading macro for transform chains
- [x] String functions: `text`, `to_lower`, `to_upper`, `capitalize`, `truncate`
- [x] Build-time error on unknown functions
- [x] Works in both hiccup (`:apply (-> text to_lower)`) and HTML (`apply="(-> text to_lower)"`)

**Documentation**
- [x] User guide updated with :apply expressions, list fields, and all v0.19.0 features
- [x] Component README files updated

---

## Done — v0.21.0: "Data context redesign and mascot overlay"

Shipped in v0.21.0.

**Data context redesign (BREAKING)**
- [x] Page data bound as `input` — `input.title` replaces stem-prefixed `article.title`
- [x] Loop items bound as `item` — `item.title` replaces bare `title` in data-each
- [x] Optional naming: `:input "article"` and `:item "p"` directives
- [x] Collections by singular stem name — `data-each="post"` not `data-each="posts"`
- [x] All pages see all collections — cross-type access (docs page can list guides)
- [x] Loops extend parent context — input, collections, outer loops accessible inside

**Mascot overlay (M5 Phase B)**
- [x] Floating mascot replaces simple edit toggle button
- [x] Contextual icons: 🤗+badge (suggestions), 👍 (all clear), ✏️ (edit mode)
- [x] Popover menu with View/Edit/Suggest modes
- [x] Suggest mode visible but disabled (Phase C)
- [x] Suggestion count badge from `.presemble-suggestion` elements

**LSP alignment**
- [x] Completions offer `input.field` paths (not stem-prefixed)
- [x] Validation checks `input.*` paths against schema

**Documentation**
- [x] User guide, feature content, README updated for input/item model

---

## Done — M4: "The conductor" (success gate met v0.32.5, ADR-031 complete)

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
- [x] ADR for conductor architecture and REPL protocol (ADR-031, shipped v0.25.0+)
- [x] S-expression command protocol: EDN parsing, dispatch to transforms and queries (shipped v0.26.0)
- [x] nng IPC layer with PUB/SUB and REQ/REP (shipped v0.25.0)
- [x] Conductor process: dep_graph, schema cache, file watcher, in-memory content (shipped v0.25.0)
- [x] `presemble lsp` as thin nng client (shipped v0.32.0 — ADR-031 Phase 5)
- [x] MCP server as conductor client with per-call site parameter (shipped v0.32.0)
- [x] `presemble serve` as thin conductor client — no local DependencyGraph, no local build_errors, no local ContentSnapshot; all handlers delegate to conductor commands (shipped v0.32.5 — ADR-031 complete)
- [x] Deduplicate link resolution into shared `expressions` component — conductor calls `expressions::*`, no stringly-typed copies (shipped v0.32.5)
- [x] Conductor rebuilds affected pages on `FileChanged` — was a no-op before (shipped v0.32.5)
- [x] Conductor tracks build errors and returns them via `GetBuildErrors` (shipped v0.32.5)
- [x] DependencyGraph removed from publisher_cli public API (shipped v0.32.5)
- [ ] Version counter and conflict detection — carried forward to M11/M13
- [ ] Conductor edge index caching — carried forward to M11
- [ ] Conductor `RefsTo` wiring — carried forward to M11
- [ ] IPC socket lifecycle: SocketGuard cleanup, signal handling, sweep_stale_sockets — carried forward to M11

**Success gate:** `presemble lsp` and `presemble serve` share state through the conductor.
Editing a file in Helix updates the browser preview before save. ✓ (v0.32.5)

---

## Done — M5: "Browser editing" (demo April 11 2026 — all deliverables shipped)

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
- Suggestions live in `.presemble/suggestions/` as files — carried forward to M11 (critical path)
- Survive conductor restarts — conductor reads on startup, re-emits as LSP diagnostics
- Committed to git if the site uses git (part of the editorial record)
- Not git-dependent — works with plain folders too

**Deliverables:**
- [x] Presemble mascot overlay with mode toggle (shipped v0.21.0)
- [x] Edit mode: inline editing of simple content fields (Phase A: suggestion nodes interactive)
- [x] Edit mode: header folding — collapse/expand sections under headings (shipped v0.29.0)
- [x] Guided site wizard — 6-step browser creation with font/color/palette customization and live CSS preview (shipped v0.30.0)
- [x] Navigation — shared nav partials, breadcrumbs, collection index pages (shipped v0.30.0)
- [x] Content creation — publisher rebuild after creation, event-driven navigation (shipped v0.30.0)
- [x] Edge-first graph architecture — edges extracted from link expressions, indexed by target URL for reverse lookup (shipped v0.31.0)
- [x] `(refs-to self)` link expressions — reverse references declared in schemas, populated via content (shipped v0.31.0)
- [x] REPL edge query builtins — `(refs-to "/url")` and `(refs-from "/url")` (shipped v0.31.0)
- [x] Suggest mode: slot-scoped search/replace (SlotEdit), suggest buttons, suggest-mode-only editing (shipped v0.32.0)
- [ ] Suggestion persistence in `.presemble/suggestions/` — moved to M11 critical path
- [ ] Conductor integration: browser edits are transforms sent over the conductor's EDN protocol — moved to M11

**Success gate:** A content author can open a served page, click the mascot to enter edit mode,
fill in missing content via suggestion nodes, and see the page update live. An editor in Helix
sees browser suggestions as diagnostics with quickfixes. ✓ (April 11 2026 demo)

---

## Done — M6: "CSS asset tracking"

Shipped in v0.18.3+ (ADR-029).

**Goal:** Close the asset-discovery gap for CSS. Stylesheets become first-class nodes in the
SiteGraph with typed `@import` and `url()` dependency edges, symmetric with content nodes.

**Deliverables:**
- [x] New `stylesheet` polylith component — CSS parsing with `cssparser` crate
- [x] Parse CSS files and discover `url()` references (fonts, images, cursors)
- [x] Recursive `@import` walking — follow import chains to discover all referenced assets
- [x] Feed discovered CSS assets into dep_graph alongside template-discovered assets
- [x] Error on missing assets referenced from CSS (same behavior as template asset references)
- [x] Copy only what is used — no blind copying of the asset directory
- [x] `FileKind::Stylesheet` and `FileKind::Asset` classification in site_index
- [x] Incremental rebuild: changing an imported CSS file triggers rebuild of all importing stylesheets

**Success gate:** A site with fonts or images referenced only from CSS `url()` builds correctly.
Missing CSS-referenced assets produce clear build errors. No assets are copied that are not
referenced. ✓

---

## M11 — "Hosted multiplayer demo"

**Target:** mid-May 2026

**Goal:** Carry the April 11 demo to a hosted, shareable state. Multiple browser clients can
collaborate live. Suggestion persistence closes the editorial loop between browser and Helix.
Conductor hardening items carried from M4 land here.

**Deliverables:**

*Suggestion persistence (M5 carry-forward, critical path):*
- [ ] Suggestions written to `.presemble/suggestions/` as files on submission
- [ ] Conductor reads suggestion files on startup and re-emits as LSP diagnostics
- [ ] Suggestions committed to git as part of the editorial record

*Conductor hardening (M4 carry-forward):*
- [ ] Conductor edge index caching — currently rebuilds per query; cache invalidated on FileChanged
- [ ] Conductor `RefsTo` wiring — currently a no-op; conductor must populate reverse-reference edges
- [ ] IPC socket lifecycle: `SocketGuard` cleanup, signal handling, `sweep_stale_sockets`

*Multiplayer:*
- [ ] PUB/SUB broadcast to multiple browser WebSocket clients — a change from one browser updates all
- [ ] Conductor tracks connected browser clients; broadcasts page invalidation on rebuild

*Distribution:*
- [ ] Linux binary distribution via CI (GitHub Actions → GitHub Releases)
- [ ] Install one-liner for Linux in README

*Hosting and auth:*
- [ ] Basic deployment story: single binary + site directory on a VPS
- [ ] Minimal auth layer — shared secret or invite token for edit/suggest access
- [ ] Serve mode works behind a reverse proxy (X-Forwarded-For, base-path support)

*Wizard UX:*
- [ ] Single-step wizard UX fix — currently multi-step flow has friction; collapse or streamline

**Success gate:** A hosted Presemble instance is reachable from a public URL. Two browser
clients can suggest edits simultaneously. Suggestions survive a conductor restart. A Helix user
sees browser suggestions as LSP diagnostics with quickfixes.

---

## M12 — "NED — Node Editor"

**Goal:** Replace ad-hoc DOM mutation with a principled RISC instruction set for DAG transforms.
NED (Node EDitor) is a selection-first model: select nodes, then apply an instruction. All
existing transforms migrate to NED primitives. The conductor speaks NED natively.

**Why now:** M3.5 introduced the Transform trait as a vocabulary. NED formalizes that vocabulary
into a minimal, composable instruction set that can be driven from the browser, Helix, and the
REPL without separate code paths.

**Core instructions:**
- `select` — address nodes by path, attribute, or schema slot
- `insert` — add a node at a position
- `replace` — swap a node's content or tag
- `remove` — delete selected nodes
- `wrap` — surround selected nodes in a new parent
- `unwrap` — hoist children of selected node, discard wrapper
- `move` — relocate selected nodes to a new position in the tree

**Deliverables:**
- [ ] NED instruction set ADR
- [ ] `ned` polylith component — instruction parser and evaluator
- [ ] Selection model: path expressions that address DOM nodes
- [ ] Core instructions implemented and tested
- [ ] Conductor dispatches NED instructions from the EDN protocol
- [ ] Existing transforms (InsertSlot, Capitalize, InsertSeparator, SlotEdit) migrated to NED primitives
- [ ] Browser edit submissions send NED instructions instead of raw field values

**Success gate:** All existing code actions execute via NED primitives. A browser edit round-trips
as a NED instruction through the conductor to disk and back to all connected clients.

---

## M13 — "Per-document versioning"

**Goal:** Add a version counter to each document so concurrent edits can be detected and
reconciled. This is the conflict model sketched in M4, now implemented.

**Why now:** Multiplayer (M11) exposes concurrent edit races. NED (M12) gives a clean primitive
layer to attach versions to. Without versioning, two clients editing the same slot silently
overwrite each other.

**Deliverables:**
- [ ] Version counter per document — monotonically increasing integer, stored in conductor state
- [ ] Demand-driven snapshots in `.presemble/snapshots/` — snapshot written when a client checks out a version
- [ ] NED instructions carry base version — conductor rejects edits against stale versions
- [ ] Suggestion pinning to base version — suggestions record which version they were made against
- [ ] Conflict detection: same slot edited from different base versions → conflict diagnostic
- [ ] Conflict resolution UI in browser — "your version / their version / merged" panel
- [ ] LSP diagnostic for conflicts with quickfix: accept mine / accept theirs / open merge view

**Success gate:** Two browser clients editing the same slot simultaneously receive a conflict
diagnostic. Each client can resolve the conflict independently. The resolved state is consistent
on disk.

---

## M14 — "Assets as a concept" (supersedes M7)

**Goal:** Assets (images, video, fonts) become a first-class concept in Presemble. The schema
declares asset slots with constraints; the publisher validates and gates; the browser lets authors
pick assets from a content browser. Remote asset stores are a later phase.

**Why supersedes M7:** M7 was designed before the conductor and NED existed. M14 is the same
vision rearchitected to fit the current model — asset operations are NED instructions, the asset
store is a conductor-owned service, and the schema drives validation.

**Phase 1 — Asset service (this milestone):**
- [ ] Asset store trait: `store(bytes, path) → url`, `resolve(path) → url`, `list(prefix) → paths`
- [ ] `fs_asset_store` — local filesystem default implementation
- [ ] Schema-driven asset variants: orientation, format, alt text constraints declared in schema
- [ ] Fragment syntax in templates: `asset.src`, `asset.alt`, `asset.width`
- [ ] Publish gate: missing or invalid assets → hard build error

**Phase 2 — Browser integration (this milestone):**
- [ ] Click image slot in edit mode → asset picker panel opens
- [ ] Browse local asset store from browser
- [ ] Select asset → NED `replace` instruction updates slot
- [ ] Alt text prompt on insert

**Phase 3 — Remote stores (deferred):**
- [ ] `s3_asset_store` and `cdn_asset_store` implementations
- [ ] Unsplash content browser with server-side API proxy

**Success gate:** A schema declares an image slot with orientation and alt text constraints. The
publisher hard-errors on missing or non-conforming assets. In serve mode, clicking the image slot
opens a picker, selecting an asset inserts it with correct metadata.

---

## M15 — "Graphical profile"

**Goal:** Visual design becomes data. A graphical profile schema captures colors, fonts, and
spacing as structured content. The wizard edits it. The browser offers a live visual panel.
CSS custom properties bridge profile data to stylesheets.

**Why now:** The wizard (M5) already lets authors pick fonts and palette at creation time. M15
makes that choice persistent, editable, and schema-validated — not buried in a CSS file.

**Deliverables:**
- [ ] Graphical profile schema — color palette, font families, spacing scale, border radius declared as schema slots
- [ ] Garden EDN for CSS — CSS generated from profile data via a Garden-inspired EDN→CSS transform
- [ ] Profile content file — `content/profile.md` (or `content/site.md` extension) holds profile values
- [ ] Wizard edits profile — creation wizard writes a valid profile content file
- [ ] Browser visual panel — floating panel in serve mode shows color swatches and font previews; clicking opens an inline editor
- [ ] Live CSS preview — editing a profile value regenerates CSS and hot-reloads in the browser without a full rebuild

**Success gate:** A site's primary color is changed via the browser visual panel. The CSS
regenerates and the browser updates live. The profile change is written to disk as a content
file edit. The schema validates the color format.

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

**M7 — "Asset store and content browsers" (superseded by M14):**
- Original M7 design predated the conductor and NED
- The vision is preserved and rearchitected in M14
- `s3_asset_store`, `cdn_asset_store`, and Unsplash browser remain on the long-term horizon (M14 Phase 3)

**Other deferred items:**
- Real-time multiplayer editing
- Comments, suggestions, track changes (beyond M5 suggest mode)
- Remote content system (cloud hosting)
- Security, OAuth, role-based access
- Data-shaped content (typed records, e.g. product catalog)
- Event-driven publish triggers (content-save → republish)
- Local/cloud profile split in polylith
