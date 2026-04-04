# Content is data. Templates are data. Schemas are the data is the contract between them.

Presemble is a site publisher that knows what your content should look like — and helps you get there. You describe each page type once, and Presemble guides you through filling it in: completions in your editor, friendly nudges when something is missing, and warm placeholders where content still needs to go. When everything is ready, it publishes. Not before.

You never wonder "did I forget something?" Presemble tells you — in the editor while you type, in the browser while you preview, and at build time before anything goes live. Missing fields, broken links, incomplete pages — they all surface as helpful suggestions, not cryptic errors.

Start a new page and Presemble shows you exactly what it needs. Fill in the blanks, save, and watch the site update in your browser. That is the workflow. No surprises, no guesswork, no broken deploys.

[Schemas As Contracts](/feature/schemas-as-contracts)

[Templates Are Data](/feature/templates-are-data)

[Instant Feedback](/feature/instant-feedback)

[Editor LSP Support](/feature/editor-lsp-support)

----

### Content is data, not text

In Hugo, Jekyll, Eleventy, and Astro, content is a text file with an optional frontmatter block. The template receives a loosely typed map. Missing fields silently render as empty strings or crash at request time. There is no contract between the author and the template.

In Presemble, a schema defines the exact sequence of elements a content document must contain — each position named, typed, and constrained. The publisher parses every content file into a typed data graph and validates it against the schema. A missing required slot is a build error with the slot name and the constraint that failed. The template cannot reference a field that is not declared; the compiler verifies this before output is written.

### Templates are data, not text with holes

Go templates, Liquid, and Jinja2 are string interpolation engines. The template is a text file. The engine finds markers and replaces them with values. Structural errors — unclosed tags, invalid nesting, data-path typos — are silent until rendered.

Presemble parses templates into DOM trees and transforms them structurally. presemble:insert directives are replaced with typed nodes from the data graph, not with raw strings. The publisher walks the tree to discover every asset reference without regex. Mismatched tags fail at parse time. Unknown data paths fail at compile time.

### Schemas are contracts, not suggestions

Most site generators have no schema system. A frontmatter field is present or it is not. The template handles both cases — or it does not — and the site publishes either way.

Presemble schemas are enforced. If a content file violates its schema, the build stops. Every constraint is checked before any output is written. The error message names the file, the slot path, and the constraint that failed. You cannot accidentally publish a site with a missing author field or a lowercase title that was supposed to be capitalized.

### Your editor knows your content

presemble lsp classifies every open file by its path within the site directory. Content files under content/post/ receive completions for every slot declared in the post schema, diagnostics for every violation, and link completions that enumerate the actual content directory. Template files receive data-path completions derived from the schema. Schema files receive keyword completions and parse-error diagnostics. One server process, one configuration, the whole site.

### The page always renders

When a content slot is missing in serve mode, Presemble renders a warm placeholder node in place of the missing content. The page renders. The placeholder is styled to stand out. You see the layout as it will appear when the content is filled in, not a blank section or a crash. At publish time, missing required slots are still build errors — the placeholders are a development aid, not a production fallback.

### Cross-content references

A link in a content file is not a URL string. It is a typed edge in the data graph. When a post schema declares an author link, the compiler resolves it to the author document, validates that the target satisfies the author schema, and makes the full author document available to the template at `input.author`. The author's name, bio, and URL are all reachable from any template that renders a post. The reference is verified at build time — a link to an author who does not exist is a build error, not a 404.
