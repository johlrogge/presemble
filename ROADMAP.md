# Roadmap

## Current milestone — M0: "It compiles a site"

**Goal:** prove that schema-driven content validation integrated into a publish pipeline is worth
the overhead. The publisher must act as a hard gate — refusing to build a site with invalid content,
with clear, actionable errors.

**Success gate:** `presemble build` against a migrated subset of blog.agical.se produces a working
static site. Schema violations in existing content surface real problems, proving the safety model
has teeth.

**Deliverables:**
- [ ] Schema definition format decided and documented
- [ ] Read markdown + frontmatter from a content directory
- [ ] Validate content against schemas — hard fail with clear error messages
- [ ] Cross-content reference validation (e.g. article references author who needs a bio)
- [ ] Template rendering to static HTML
- [ ] `presemble build` CLI command
- [ ] Dogfood test: build a subset of blog.agical.se content

## Backlog

**M1 — "It serves and watches"**
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
