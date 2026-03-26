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

## Current milestone — M2: "Cross-content references and site configuration"

**Goal:** make content items aware of each other at render time, so templates can pull data from
linked content (e.g. show an author's name from the author page, not hardcoded in the article).
Introduce a site configuration file and basic collection query support.

**Success gate:** a template can render `author.name` by following a content reference from an
article to its author page automatically, without any workarounds in the content files.

**Deliverables:**
- [ ] Cross-content reference resolution — templates can pull data from linked content items (e.g. render author name from author page, not hardcoded in article)
- [ ] Site configuration file (`site.yaml` or similar) — declare entry points, site metadata available as `site.name`, `site.url` etc.
- [ ] Collection queries — filter/sort collections (currently `site.posts` returns all posts unsorted)
- [ ] Improve template composition — calling one template from another cleanly (currently done via index page's `data-each`, but no explicit template include mechanism)

---

## Backlog

**M3 — "Content as a separate concern"**
- Introduce the content system as a local service (not remote yet)
- Content stored separately from templates and design — the key architectural separation from git
- `presemble serve` pulls content from the local content store
- Basic browser UI: view content, edit markdown, save back to the content store
- Schema validation on save (the rust-analyzer side of the analogy — real-time guidance)

**M4 — "Time enters the picture"**
- Publish timestamps on content items
- `presemble build --at <datetime>` — render the site as it will appear at a given moment
- Timeline scrubber in the `presemble serve` UI
- Publisher maintains a timetable of future publish events

---

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
