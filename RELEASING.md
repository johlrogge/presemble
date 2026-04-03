# Feature & Release Workflow

Presemble uses git flow with a multi-agent workflow for both feature development and releases.

## Branch Model

- `master` — released code only, always tagged
- `develop` — integration branch, features merge here
- `feature/*` — individual features, branch from develop
- `release/*` — release prep, branch from develop
- `hotfix/*` — urgent fixes, branch from master

## Versioning (semver)

- `feat` commits → minor bump (0.4.0 → 0.5.0)
- `fix` / `chore` commits → patch bump (0.5.0 → 0.5.1)
- Breaking change (`!`) → major bump (0.5.0 → 1.0.0)

## Feature Workflow

New features go through a design-implement-review loop before release.

1. **product-owner** — prioritize and scope the feature on the roadmap
   > "Should we add X? Where does it fit in the roadmap?"

2. **release-manager** — start the feature branch
   > "Start feature <name>"

3. **architect** — design the feature; produce a task breakdown for minions
   > "Design the implementation for <feature>"
   The architect produces clear, scoped tasks — one per minion.

4. **code-minions** — implement in parallel, one task each
   > Spawn multiple minions simultaneously with different task descriptions.
   Each minion: writes a failing test → implements → runs checks → reports back.

5. **architect** — review all minion output
   > "Review the changes from <minion tasks>"
   - If approved: says COMMIT with a suggested message
   - If changes needed: dispatches minions again with specific fix instructions
   - Loop until approved

6. **commit** — commit the approved changes
   > Invoked by the orchestrator when architect approves

7. **devops** — finish the feature branch
   > "Finish feature <name>"

## Release Checklist

Run these agents in order before cutting a release:

1. **MANDATORY hygiene gate** — run `mcp__rust-codebase__hygiene_report` (tests + clippy + coverage). **DO NOT PROCEED if this fails.** CI enforces `clippy -- -D warnings` and will reject the release. A git pre-commit hook also runs clippy on every commit (configured in `devenv.nix`).

2. **architect** — review all changes since last release
   > "Review changes since last release"

3. **product-owner** — confirm the release delivers intended value
   > "Review the planned 0.x.0 release"

4. **documenter** — update README files to reflect the release. Document all missing features as features in the site and as sections in the user-guide. Review the site (/site/). You are looking for outdated information and missing features. Review the homepage elevator pitch and "Why Presemble" section to ensure they reflect current capabilities — present tense only.
   > "Update docs for release 0.x.0"

5. **Update `ROADMAP.md`** — mark any newly completed deliverables as `[x]` and move semantic-types or other explicitly deferred items out of the current milestone so M2/M3/etc. have a clean definition of done.

6. **release-manager** — start and finish the release branch
   > "Start release 0.x.0" → confirm → "Finish release 0.x.0"

7. **Human** — push to remote
   ```
   git push origin master develop --tags
   ```

## Hotfix Checklist

1. **release-manager** — start hotfix
2. **commit** — commit the fix
3. **release-manager** — finish hotfix (confirm before calling)
4. **Human** — push

## Notes

- Agents never push — that always stays with the human
- Always confirm with release-manager before finishing a release or hotfix
- Multiple code-minions can run in parallel on different tasks within the same feature
- The commit agent uses the conventional-commits skill for format
- The architect never writes code — it designs and reviews only
- Version is declared in `[workspace.package]` in `Cargo.toml` — bump it on the release branch before finishing

---

## Release History

### v0.18.0

Polylith interface wiring and in-memory test repository.

**Polylith interface indirection**
Consumers depend on `site_repository` (the interface name). The profile selects which implementation backs it: `live` uses `fs_site_repository` (filesystem), `dev` uses `mem_site_repository` (in-memory for tests). Both implementations are named distinctly — the interface handles the mapping.

**`mem_site_repository` component**
In-memory `SiteRepository` with a builder pattern for programmatic construction and a `from_dir()` method for loading fixtures. Conductor, LSP, template registry, and build tests all migrated to use the builder — no more TempDir fixtures for unit tests.

**`build_site` accepts `&SiteRepository`**
The build pipeline function now takes an explicit repository parameter instead of creating one internally. CLI entry points construct the repo from the site directory; tests pass builder-constructed repos.

**Production build with `--release`**
The devenv binary and CI deploy now build with `--profile live --release`. The presemble binary on PATH is release-optimized.

**Profile cleanup**
Removed redundant `development` profile. `live` profile brought up to date with all components.

---

### v0.17.0

SiteRepository abstraction — filesystem access decoupled from build pipeline.

**`fs_site_repository` component**
New polylith component (interface group: `site_repository`) encapsulating all filesystem access for site sources. `SiteRepository` provides typed methods for reading schemas, content, and templates using `SchemaStem` keys. The build pipeline, template registry, conductor, and LSP capabilities all read through the repository instead of scattered `fs::read_to_string` calls. Foundation for an in-memory implementation (`mem_site_repository`) that enables fast, filesystem-free tests.

---

### v0.16.0

Unified SiteGraph architecture and curated homepage features.

**SiteGraph as single source of truth (ADR-026)**
The build pipeline is rewritten around a unified `SiteGraph` that holds all site data — items, collection pages, and the site index — in one structure. The three-phase build (build all entries → resolve all references once → render all entries) replaces the previous ad-hoc assembly with three separate code paths. Reference resolution now covers all content kinds uniformly. `SchemaStem` and `UrlPath` newtypes eliminate stringly-typed HashMap keys.

**Curated homepage features via resolved link slots**
The homepage now links to 4 specific features (via link slots in the index schema) instead of iterating all features with `data-each`. Cross-content reference resolution enriches the link records with full feature data (title, tagline, description). Reference resolution now also walks into list items for multi-occurrence link slots.

**Incremental rebuild with filtered output**
`rebuild_affected` performs a full build for correctness (SiteGraph reference resolution requires the complete graph) but filters the returned site graph to only affected entries. The serve loop only sends browser-reload notifications for pages that actually changed.

---

### v0.15.0

Collection pages and complete publishing model.

**Collection page building**
The publisher now builds per-type listing pages. If a content type has `content/{stem}/index.md`, `schemas/{stem}/index.md`, and `templates/{stem}/index.hiccup`, a collection page is generated at `/{stem}/index.html`. Collection content without a schema is an error (consistent with item pages). The collection template has access to the full site context, so `data-each="posts"` iterates all items. The collection's own content (title, description) is available under the stem key.

**presemble.io site listing pages**
The presemble.io site now has `/post/` and `/feature/` listing pages with titles and item summaries.

---

### v0.14.0

Template format unification and directory-based naming.

**Bidirectional HTML/EDN template conversion**
`presemble convert --to edn templates/post/item.html` converts any template between HTML and hiccup/EDN formats. The converter uses a chain-of-parsers pattern (`resolve_template_file`) that tries hiccup then HTML, returning a clear error listing all tried paths on failure. The presemble.io site now runs entirely on hiccup/EDN templates.

**Hiccup parser fixes**
Fixed attribute namespace separator: `:presemble/class` in hiccup now correctly produces `presemble:class` in the internal DOM (was producing `presemble/class`). Added `;` line comment support to the hiccup tokenizer (standard EDN convention).

**Unified directory-based naming**
Schemas and templates follow a consistent directory convention per content type. `schemas/{stem}/item.md` defines the item schema; `templates/{stem}/item.hiccup` renders each item. Flat files (`schemas/index.md`, `templates/index.hiccup`) are used for singular pages like the homepage. The rule: no directory should ever be named `index`.

---

### v0.13.0

M3.5 complete — code action transformation model and content authoring improvements.

**Code action transformation model (ADR-023, ADR-024, ADR-025)**
Code actions are now pure-functional structs implementing a `Transform` trait. A structural differ computes slot-level semantic diffs using `im::Vector` structural sharing (ADR-024). Consumer adapters translate diffs into targeted LSP `TextEdit` arrays, file writes, or browser updates (ADR-025). This fixes the lost-error-markers bug that previously caused diagnostics to disappear after a code action was applied.

**Inline markdown rendering in body**
Bold, italic, blockquotes, and lists are parsed and rendered in the body section. Blockquote and list syntax is now recognised by the content parser.

**Inline link completions**
Typing `[` in a content body triggers a completions list of all content pages in the site, formatted as `[Title](/type/slug)`. The list is narrowable as you continue typing.

**Body heading completions**
The LSP offers H3–H6 heading completions in content body sections. Completions are only offered at the start of a line, matching the heading constraint of the body slot.

**Code action fix: heading `InsertSlot` prefix**
The quickfix that inserts a missing heading slot no longer doubles the `#` prefix in the inserted text.

**Completion fix: content completions use `text_edit`**
Content completions now replace the whole current line using `text_edit`, preventing partial overwrites when a line already has a prefix.

**Save fix: auto-format preserves body text**
Auto-format on save no longer replaces body text with rendered HTML tags. Body content is preserved verbatim on round-trip.

**Save fix: no spurious "modified on disk" warning**
Auto-format no longer rewrites the file in a way that triggers the editor's external-modification warning.

**Browser preview: cursor-follow scroll**
Hovering over an element in Helix scrolls the browser preview to the matching body element using source map annotations. Programmatic scroll no longer blocks subsequent cursor-follow events.

**Infrastructure**
- `im` crate for persistent DOM trees (ADR-021)
- Slotted document structure with named slots (ADR-022)
- `nng` socket timeouts prevent LSP shutdown hang
- ADRs 021–025 added

---

### v0.6.0

M3 Phase 4: Template and schema LSP support.

**Template file LSP**
`presemble lsp` now handles template files. Data-path completions are derived from the schema matching the template's file stem. Data paths referencing fields that do not exist in the schema are flagged as errors. Hover on a `data="…"` attribute shows the field hint text. Go-to-definition on `presemble:include src="…"` jumps to the referenced template file; go-to-definition on an in-file `presemble:apply` reference jumps to the `presemble:define` block.

**Schema file LSP**
`presemble lsp` now handles schema files. Element keyword completions offer correct syntax for headings, paragraphs, links, and images at the right cursor context. Constraint completions are contextual to the slot type above the cursor. Parse errors in the schema surface as diagnostics at the exact failing line.

**Single-server file-type dispatch**
A single `presemble lsp` process classifies open documents by path prefix (`content/`, `templates/`, `schemas/`) and dispatches to the appropriate capability handlers. No separate server instances per file type.

---

### v0.5.3

M3 Phase 3: Content LSP.

**Content-aware completions**
`presemble lsp` provides completions for content files: slot names from the schema, and for link slots, actual content files formatted as `[Title](/type/slug)` insert text.

**Content diagnostics**
Schema violations surface inline as the author types: missing required slots, wrong occurrence counts, capitalization violations, broken link references.

**Hover and go-to-definition**
Hover on a content element shows the schema hint text for that slot. Go-to-definition on a link value navigates to the linked content file.

**Quickfix code actions**
Capitalization violations include a one-step quickfix. Missing slots include a generated snippet quickfix that inserts the slot template at the body separator.

---

### v0.5.0

M3 Phase 2: Source map annotations and focus on changed element.

**Source map annotations**
Rendered DOM elements are annotated with source-file provenance. The browser uses these annotations to scroll to and highlight the element that changed after a live rebuild.

**In-memory rebuild fast path**
DOM diffs can be served directly to the browser without a disk write for the preview step.

---

### v0.4.1

M3 Phase 1.1: Smart navigation.

**Smart navigation on live reload**
The server sends changed page URL(s) in WebSocket messages (`{type, pages, primary}`). If the current page changed the browser reloads in place; otherwise it navigates to the first changed page.

---

### v0.4.0

M3 Phase 1: WebSocket live reload.

**WebSocket live reload**
`presemble serve` injects a small script into served pages that connects over WebSocket and reloads when the dep_graph detects changed outputs. Only affected pages trigger a reload signal.

---

### v0.3.0

M2: Cross-content references and template composition.

**Cross-content reference resolution (ADR-012)**
Templates can traverse content links to render data from linked documents (`post.author.name`, `post.author.bio`). Resolution is verified before any output is written.

**Template composition**
`presemble:define` declares named callable fragments; `presemble:apply` invokes them with an explicit data context (ADR-013). File-qualified references use `::` notation.

**Collection queries at root level**
Collections are accessed at the root of the data graph (`data-each="posts"`, not `data-each="site.posts"`).

---

### v0.2.0 (upcoming)

Covers milestones M0.5 and M1.

**`presemble serve` with live reload**
Local HTTP server with file watching and 150 ms debounce. Changes trigger incremental rebuild; the browser receives a reload signal only for affected pages.

**Incremental rebuild with file-level dependency tracking (ADR-008)**
`build_site()` populates a `DependencyGraph` that maps each output page to the source files it depends on. `rebuild_affected()` consults the reverse index to rebuild only the pages touched by a changed file. Cold start always does a full build; the graph is in-memory only.

**Clean URL routing (ADR-009)**
Pages are written to `/{type}/{slug}/index.html` and served at `/{type}/{slug}`. No `.html` in links or the `url` data-graph field.

**Data-driven asset discovery from template DOM trees (ADR-010)**
`<link href>`, `<img src>`, and `<script src>` references are extracted from parsed template DOM trees at build time. Only referenced assets are copied to output; a missing referenced asset is a build error rather than a silent failure.

**Hiccup/EDN as second template surface syntax (ADR-011)**
`.hiccup` files express the same internal DOM tree as `.html` templates using EDN vectors (`:tag`, optional attribute map, children). Implemented with a hand-written minimal EDN parser; no new dependency. The transformer, serializer, and asset extractor are unmodified — proving that surface syntax and internal model are orthogonal (ADR-005).

**Fenced code block rendering**
Fenced code blocks in content body sections are rendered as `<pre><code>` elements.

**presemble.io dogfood site built with Presemble (M0.5)**
`site/` contains the presemble.io promotional site: three content types (feature, post, author), six pages, nature-inspired CSS, and passing link validation. Built entirely with `presemble build` — no workarounds.

**Dot-path separator for data graph paths**
Data graph references use dot-path notation (`article.title`, `post.author.name`) throughout templates and content files.

**Cross-content reference resolution (ADR-012)**
After all pages are built, a post-build phase merges referenced page data into link records. A template rendering an article can access `post.author.name`, `post.author.bio`, and `post.author.avatar` without duplicating data in the article's content file. Resolution is one level deep; cycles are not possible.

---

### v0.1.0

Initial release. Schema format (ADR-001), content validation with hard fail, DOM template engine (ADR-005), `presemble build` CLI, clean URLs.
