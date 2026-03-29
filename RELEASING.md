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

1. **architect** — review all changes since last release
   > "Review changes since last release"

2. **product-owner** — confirm the release delivers intended value
   > "Review the planned 0.x.0 release"

3. **documenter** — update README files to reflect the release. Document all missing features as features in the site and as sections in the user-guide. Review the site (/site/). You are looking for outdated information and missing features.
   > "Update docs for release 0.x.0"

4. **Update `ROADMAP.md`** — mark any newly completed deliverables as `[x]` and move semantic-types or other explicitly deferred items out of the current milestone so M2/M3/etc. have a clean definition of done.

5. **release-manager** — start and finish the release branch
   > "Start release 0.x.0" → confirm → "Finish release 0.x.0"

6. **Human** — push to remote
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
- Always confirm with devops before finishing a release or hotfix
- Multiple code-minions can run in parallel on different tasks within the same feature
- The commit agent uses the conventional-commits skill for format
- The architect never writes code — it designs and reviews only
- Version is declared in `[workspace.package]` in `Cargo.toml` — bump it on the release branch before finishing

---

## Release History

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
