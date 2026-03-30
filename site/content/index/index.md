# Content is data. Templates are data. Schemas are the contracts between them.

Presemble is a schema-driven site publisher. You define the structure of your content in plain markdown schemas. The publisher enforces those schemas at build time — every slot, every link, every asset — before a single output file is written. No partial publishes. No 404s waiting in production.

Templates are structured DOM trees, not text with holes. The publisher transforms them node-by-node, inserting typed content from the data graph. String manipulation only happens at serialisation. Mismatched tags and broken nesting are caught at parse time, not in production.

Your editor speaks Presemble. `presemble lsp` starts a single Language Server Protocol server that provides completions, diagnostics, hover, and go-to-definition for content files, template files, and schema files — all at once. Errors surface as you type. A schema violation shows up before you save, not after you deploy.

----

### Content is data, not text

In Hugo, Jekyll, Eleventy, and Astro, content is a text file with an optional frontmatter block. The template receives a loosely typed map. Missing fields silently render as empty strings or crash at request time. There is no contract between the author and the template.

In Presemble, a schema defines the exact sequence of elements a content document must contain — each position named, typed, and constrained. The publisher parses every content file into a typed data graph and validates it against the schema. A missing required slot is a build error with the slot name and the constraint that failed. The template cannot reference a field that is not declared; the compiler verifies this before output is written.

### Templates are data, not text with holes

Go templates, Liquid, and Jinja2 are string interpolation engines. The template is a text file. The engine finds markers and replaces them with values. Structural errors — unclosed tags, invalid nesting, data-path typos — are silent until rendered.

Presemble parses templates into DOM trees and transforms them structurally. `presemble:insert` directives are replaced with typed nodes from the data graph, not with raw strings. The publisher walks the tree to discover every asset reference without regex. Mismatched tags fail at parse time. Unknown data paths fail at compile time.

### Schemas are contracts, not suggestions

Most site generators have no schema system. A frontmatter field is present or it is not. The template handles both cases — or it does not — and the site publishes either way.

Presemble schemas are enforced. If a content file violates its schema, the build stops. Every constraint is checked before any output is written. The error message names the file, the slot path, and the constraint that failed. You cannot accidentally publish a site with a missing author field or a lowercase title that was supposed to be capitalized.

### Your editor knows your content

`presemble lsp` classifies every open file by its path within the site directory. Content files under `content/post/` receive completions for every slot declared in the post schema, diagnostics for every violation, and link completions that enumerate the actual content directory. Template files receive data-path completions derived from the schema. Schema files receive keyword completions and parse-error diagnostics. One server process, one configuration, the whole site.

### The page always renders

When a content slot is missing in serve mode, Presemble renders a warm placeholder node in place of the missing content. The page renders. The placeholder is styled to stand out. You see the layout as it will appear when the content is filled in, not a blank section or a crash. At publish time, missing required slots are still build errors — the placeholders are a development aid, not a production fallback.

### Cross-content references

A link in a content file is not a URL string. It is a typed edge in the data graph. When a post schema declares an author link, the compiler resolves it to the author document, validates that the target satisfies the author schema, and makes the full author document available to the template at `post.author`. The author's name, bio, and URL are all reachable from any template that renders a post. The reference is verified at build time — a link to an author who does not exist is a build error, not a 404.
