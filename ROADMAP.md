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

## Current milestone — M3: "Live editorial feedback loop"

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

*Phase 4: Template and schema LSP*
- Extend LSP support to template files: completions for data-paths, `presemble:apply` references, callable template names
- Schema-driven diagnostics in templates: flag references to fields that do not exist in the schema
- Schema file LSP: completions for field types, occurrence markers, reference targets
- Structural field existence checking (Layer 1 of template type system): the access path IS the constraint

*Phase 5: Suggestion nodes (error UX)*
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

## M4 — "The conductor"

**Goal:** Unify all Presemble processes under a single shared-state conductor. Today `presemble lsp`
and `presemble serve` are separate processes with separate state. The conductor makes them thin
clients of one authoritative process.

**Conductor owns:**
- dep_graph (single source of truth)
- Schema cache
- In-memory working copies of content files (for live-update-while-typing)
- File watcher

**Clients connect via nng (nanomsg-next-gen) IPC:**
- `presemble lsp site/` — thin bridge: Helix stdio to nng socket to conductor
- `presemble serve site/` — thin HTTP/WebSocket client of conductor
- `presemble repl site/` — REPL client (future, see Deferred)

**nng topology:**
- PUB/SUB for broadcasts (file changed, rebuild done)
- REQ/REP for commands (validate, apply edit)
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
- [ ] ADR for conductor architecture
- [ ] nng IPC layer with PUB/SUB and REQ/REP
- [ ] Conductor process: dep_graph, schema cache, file watcher, in-memory content
- [ ] `presemble lsp` as thin nng client
- [ ] `presemble serve` as thin nng client
- [ ] Version counter and conflict detection

**Success gate:** `presemble lsp` and `presemble serve` share state through the conductor.
Editing a file in Helix updates the browser preview before save.

---

## M5 — "Browser editing"

**Goal:** The served page IS the editor. Content authors who never touch a terminal can create
and edit content directly in the browser.

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
- [ ] Conductor integration: browser edits flow through the standard edit pipeline

**Success gate:** A content author can open a served page, click the mascot to enter edit mode,
fill in missing content via suggestion nodes, and see the page update live. An editor in Helix
sees browser suggestions as diagnostics with quickfixes.

---

## M6 — "Time enters the picture"

**Goal:** Time becomes a first-class dimension of the site. Authors can schedule content and
preview the future state of the site.

- Publish timestamps on content items
- `presemble build --at <datetime>` — render the site as it will appear at a given moment
- Timeline scrubber in the `presemble serve` UI
- Publisher maintains a timetable of future publish events
- Content saves push events into the timetable
- Dual publish triggers: clock-driven (timetable) and event-driven (git push or content save)

**Success gate:** An author can set a publish date on a blog post, use the timeline scrubber to
preview the site at that date, and the publisher automatically publishes when the time arrives.

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

**Cider-compatible REPL:**
- Interactive REPL for exploring schemas, templates, and the data graph from an editor
- Emacs/Cider first-class; extensible to other editor REPL clients
- Query the data graph, test template fragments, inspect schema validation — without leaving the editor
- Requires the conductor's nng IPC backbone

**Other deferred items:**
- Real-time multiplayer editing
- Comments, suggestions, track changes (beyond M5 suggest mode)
- Remote content system (cloud hosting)
- Security, OAuth, role-based access
- Data-shaped content (typed records, e.g. product catalog)
- Event-driven publish triggers (content-save → republish)
- Local/cloud profile split in polylith
