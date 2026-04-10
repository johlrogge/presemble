# Content is data. Templates are data. Schemas are the contract between them.

Presemble is a site publisher focused on editorial collaboration and semantic content safety. You describe each page type once, and Presemble guides you through filling it in: completions in your editor, friendly nudges when something is missing, warm placeholders where content still needs to go, and Claude as a collaborator who can suggest improvements and have them appear as LSP diagnostics. When everything is ready, it publishes. Not before.

You never wonder "did I forget something?" Presemble tells you — in the editor while you type, in the browser while you preview, and at build time before anything goes live. Missing fields, broken links, incomplete pages — they all surface as helpful suggestions, not cryptic errors.

Start a new site with the browser wizard: point `presemble serve` at an empty directory and a six-step wizard walks you through site type, font mood, color palette, and template syntax. The wizard generates a custom stylesheet and scaffolds a complete working site in seconds. Or open an existing content file and let Claude push improvements as structured suggestions while you accept or reject each one.

[Schemas As Contracts](/feature/schemas-as-contracts)

[Templates Are Data](/feature/templates-are-data)

[Instant Feedback](/feature/instant-feedback)

[Editor LSP Support](/feature/editor-lsp-support)

----

### Content is data, not text

In Hugo, Jekyll, Eleventy, and Astro, content is a text file with an optional frontmatter block. The template receives a loosely typed map. Missing fields silently render as empty strings or crash at request time. There is no contract between the author and the template.

In Presemble, a schema defines the exact sequence of elements a content document must contain — each position named, typed, and constrained. The publisher parses every content file into a typed data graph and validates it against the schema. A missing required slot is a build error with the slot name and the constraint that failed. The template cannot reference a field that is not declared; the compiler verifies this before output is written.

### Templates are pure functions

Go templates, Liquid, and Jinja2 are string interpolation engines. The template is a text file. The engine finds markers and replaces them with values. Structural errors — unclosed tags, invalid nesting, data-path typos — are silent until rendered.

Presemble parses templates into DOM trees and transforms them structurally. Templates are written in Hiccup (EDN) as the primary format and compose using `juxt` and pipe combinators. `((juxt header self/body footer) input)` fans the same content tree to three template functions and concatenates their DOM outputs. No string interpolation occurs at any point.

### Schemas are contracts, not suggestions

Most site generators have no schema system. A frontmatter field is present or it is not. The template handles both cases — or it does not — and the site publishes either way.

Presemble schemas are enforced. If a content file violates its schema, the build stops. Every constraint is checked before any output is written. The error message names the file, the slot path, and the constraint that failed. You cannot accidentally publish a site with a missing author field or a lowercase title that was supposed to be capitalized.

### Your editor knows your content

`presemble lsp` classifies every open file by its path within the site directory. Content files under `content/post/` receive completions for every slot declared in the post schema, diagnostics for every violation, and link completions that enumerate the actual content directory. Template files receive data-path completions derived from the schema. Schema files receive keyword completions and parse-error diagnostics. One server process, one configuration, the whole site.

The LSP routes classify, grammar, completions, and document text through the conductor daemon. If `presemble serve` is already running for the site, the LSP connects to that conductor. Otherwise the LSP starts one automatically on the first request.

### Claude as editorial collaborator

`presemble mcp site/` exposes the site to Claude Code. Claude reads your schemas to understand your content model, reads your content files to understand what exists, and pushes targeted suggestions to specific slots with a rationale. Each suggestion appears as an LSP diagnostic in your editor with an accept/reject code action. Claude uses the same suggestion protocol as a human editor — there is no special AI path.

Each MCP tool call accepts a `site` parameter, so a single MCP server instance can work across multiple Presemble sites without restart. Content enumeration (`list_content`) goes through the conductor — the MCP server never reads the filesystem directly.

The same suggestions appear as inline diffs in the browser preview with a toolbar to accept or reject them individually or in bulk. The diff is minimal — only the changed text is highlighted. Slot-scoped suggestions (`SuggestSlotEdit`) let a collaborator target a phrase within a slot rather than replacing the whole value.

### Content assembles itself

Content files include Presemble Lisp expressions that assemble collections at build time:

```markdown
[]((->> :post (sort-by :published :desc) (take 5)))
```

The expression evaluates against the site graph and produces a validated list. The homepage decides what appears and in what order; the template decides how it looks. Neither can override the other's responsibility.

Expressions also support reverse references. An author page can declare a slot that populates itself with all posts that link to that author:

```markdown
[posts](->> :post (refs-to self))
```

The publisher queries the site's edge index at build time. No manual maintenance of reverse-reference lists is required.

### The page always renders

When a content slot is missing in serve mode, Presemble renders a warm placeholder node in place of the missing content. The page renders. The placeholder is styled to stand out. You see the layout as it will appear when the content is filled in, not a blank section or a crash. At publish time, missing required slots are still build errors — the placeholders are a development aid, not a production fallback.

### Cross-content references

A link in a content file is not a URL string. It is a typed edge in the data graph. When a post schema declares an author link, the compiler resolves it to the author document, validates that the target satisfies the author schema, and makes the full author document available to the template at `input.author`. The author's name, bio, and URL are all reachable from any template that renders a post. The reference is verified at build time — a link to an author who does not exist is a build error, not a 404.
