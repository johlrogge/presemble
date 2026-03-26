# Roadmap

## Current milestone — M0: "It compiles a site"

**Goal:** prove that schema-driven content validation integrated into a publish pipeline is worth
the overhead. The publisher must act as a hard gate — refusing to build a site with invalid content,
with clear, actionable errors.

**Success gate:** `presemble build` against a migrated subset of blog.agical.se produces a working
static site. Schema violations in existing content surface real problems, proving the safety model
has teeth.

**Deliverables:**
- [x] Schema definition format decided and documented (ADR-001)
- [x] Read markdown from a content directory
- [x] Validate content against schemas — hard fail with clear error messages
- [~] Cross-content reference validation — link validation implemented (pages must exist); automatic name resolution from referenced content deferred
- [x] Template rendering to static HTML (ADR-004)
- [x] `presemble build` CLI command
- [ ] Dogfood test: build a subset of blog.agical.se content

## Backlog

**M0.5 — "Presemble builds its own site"**

**Status: complete.** site/ contains the presemble.io promotional site with three content types (feature, post, author), six pages, clean URLs, and Link validation: OK.

- Build the presemble.io promotional site using Presemble itself
- Content: what Presemble is, why it exists, how to get started
- This is the real dogfood test — if building the site reveals gaps, they get fixed before M1
- Success gate: `presemble build` produces a deployable presemble.io site with no workarounds
- DOM template engine (ADR-005): proposed and implemented

**M1 — "It serves and watches"**

**Status: implemented on develop.** presemble serve, file watching with 150ms debounce, incremental rebuild with file-level dependency tracking (ADR-008), clean URLs (ADR-009).

- `presemble serve` — local HTTP server with file watching and live rebuild
- Equivalent to `hugo serve` — fast feedback loop for template and design iteration
- No browser editing yet, just viewing

**M2 — "Content as a separate concern"**
- Introduce the content system as a local service (not remote yet)
- Content stored separately from templates and design — the key architectural separation from git
- `presemble serve` pulls content from the local content store
- Basic browser UI: view content, edit markdown, save back to the content store
- Schema validation on save (the rust-analyzer side of the analogy — real-time guidance)

**M3 — "Time enters the picture"**
- Publish timestamps on content items
- `presemble build --at <datetime>` — render the site as it will appear at a given moment
- Timeline scrubber in the `presemble serve` UI
- Publisher maintains a timetable of future publish events

## Deferred (post-MVP)

These are real parts of the vision, not cut — just not needed to prove the core value:

- Real-time multiplayer editing
- Comments, suggestions, track changes
- LSP / Helix integration
- Remote content system (cloud hosting)
- Security, OAuth, role-based access
- Data-shaped content (typed records, e.g. product catalog)
- Event-driven publish triggers (content-save → republish)
- Local/cloud profile split in polylith

## Done

<!-- Completed milestones -->

**M0 — "It compiles a site"** — schema format (ADR-001), content validation, DOM template engine (ADR-005), presemble build CLI, clean URLs (ADR-009).
