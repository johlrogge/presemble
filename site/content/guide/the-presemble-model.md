# The Presemble Model

Presemble organizes a site into four layers — schemas, content, templates, and stylesheets — that compose through a strict contract system, so every page is validated before any HTML is produced.

A static site builder built from first principles: schemas define what content must look like, content fulfils those contracts, templates transform the result into HTML, and the build pipeline refuses to proceed unless every layer checks out.

----

## Four Concepts

Presemble separates four concerns that most static-site tools blur together.

**Schemas** define the shape of content: what fields exist, how many times they appear, what constraints they carry. A schema is a type declaration for a document.

**Content** is Markdown files that satisfy schema contracts. Content is both authored prose and assembled data — editorial text alongside references that pull in other content. A homepage might contain an introduction written by an editor and a link expression that assembles the latest five posts.

**Templates** are pure functions from a validated content tree to a DOM tree. They receive structured data and produce HTML. They never select content; they only present what they receive.

**Stylesheets** are CSS with asset tracking. The site graph knows which templates reference which stylesheets, so only used assets are copied to output.

These four layers have clear boundaries. Content is never responsible for presentation. Templates are never responsible for selecting content. Schemas are never responsible for rendering. The boundaries are the design.

## Trees All The Way Down

Schemas define tree shapes. Content fills trees. Templates transform content trees into DOM trees. The HTML renderer turns DOM trees into byte streams.

Every step is a tree transformation. This uniformity is not accidental — it means every layer can be validated, diffed, transformed, and reasoned about with the same tools.

A schema says: *this document has exactly one title, one-to-three summary sentences, and a body.* The content document either is that shape or it is not. There is no ambiguity. The validator walks both trees in lock-step and reports every mismatch.

A template says: *given this content tree, produce this DOM tree.* The template engine walks the content tree and builds output. If the template asks for a field the content does not have, that is a type error caught at build time.

## Schemas: The Contract

A schema is a contract written in Markdown. It defines the fields a content file must provide, how many times each appears, and what constraints apply.

```markdown
# Blog post title {#title}
occurs
: exactly once
content
: capitalized

A one-sentence summary of the post. {#summary}
occurs
: 1..3

----

Body content.
headings
: h2..h6
```

This schema says: the file must have exactly one title, it must be capitalized, it must have one to three summary paragraphs, and the body may use h2 through h6 headings.

Every schema lives in `schemas/<type>/item.md`. The path convention is the type system: a file at `content/post/hello.md` is validated against `schemas/post/item.md` automatically.

Schema constraints are compile-time. There is no runtime validation, no "if the field exists" branching in templates. Either the content satisfies the schema or the build fails.

## Content: Prose And Assembly

Content files are Markdown validated against schemas. A simple blog post might be entirely editorial prose. A homepage might be something richer.

The key insight is that links are the composition mechanism. The most natural thing in a content system — a link — becomes the universal way to reference, assemble, and relate content.

A link to another content item is a reference. A link with a query expression is a collection: select all posts of a type, sorted by date, limited to the five most recent. The homepage does not need a template to enumerate its sections — the content file assembles them via link expressions, and the template presents whatever it receives.

```markdown
# Welcome

A site about things worth reading.

----

## Recent writing {#recent}

[](-> /post (select :all) (sort .published :desc) (take 5))

## Featured {#featured}

[](-> /feature (select :all) (sort .published :desc) (take 3))
```

This separation matters. The homepage author decides *what* appears: which collections, in what order, with what framing. The template decides *how* it looks. Neither can override the other's responsibility.

Content fulfils what schemas promise. The schema says "a homepage has a title, a summary, and a body." The content file satisfies that contract. The template receives a validated homepage tree and renders it. Every step is checked.

## Templates: Pure Functions

A template is a function: `input → DOM`. It receives a validated content tree and produces a DOM tree. Nothing else.

Templates are written in Hiccup (an EDN-based syntax) as the primary format, with equivalent HTML as a secondary format. Both represent the same DOM tree model.

```clojure
[
  [:template "body"
    [presemble/insert {:data input.title :as :h1}]
    [presemble/insert {:data input.summary :as :p.summary}]
    [presemble/insert {:data input.body}]]

  ((juxt
    /fragments/structure#header
    (apply self/body)
    /fragments/structure#footer) input)
]
```

A template file has two parts: optional local definitions (like `let` bindings) and a composition expression (the return value).

Two combinators compose templates:

**Pipe** (`->`) threads a value through a sequence of transforms. `(-> input.title upcase)` takes the title and uppercases it. The pipe is the universal data transform: extract a field, apply a function, thread the result forward.

**Juxt** fans the same input to multiple templates and concatenates their DOM outputs. In the example above, the header, body, and footer templates all receive the same content tree. Their outputs are assembled in order.

Because templates are pure functions, they are easy to test, easy to compose, and impossible to break in unexpected ways. A template cannot reach outside its input. It cannot query the database. It cannot read the filesystem. It receives a tree and returns a tree.

Template files are function files: optional local definitions at the top, a composition expression at the bottom. The composition expression is the function body.

Templates are checked against the data they receive by duck-typing. If a template accesses `:post/tags` and the content tree has that field, it works. If the field is absent, the build fails. The checking is structural: the template describes what it uses, and the validator confirms it exists.

## The Build Pipeline: Validate Then Render

The build pipeline is itself a function:

```
(schemas, content) → validated trees → DOM trees → HTML
```

The pipeline is strictly ordered. Nothing renders until everything validates.

**Phase 1: Parse.** Read all schemas and content files. Build the type registry. Resolve all content references and link expressions into a directed graph.

**Phase 2: Validate.** Walk every content node against its schema. Check every link expression against the collection schema it targets. Report all violations. If anything fails, stop.

**Phase 3: Render.** Walk the validated content graph. For each page, select the matching template and apply it to the content tree. Collect DOM trees.

**Phase 4: Emit.** Serialize DOM trees to HTML. Copy only the assets referenced by templates. Write output.

No runtime surprises means: if the site builds, it is correct by construction. The output is a pure function of the inputs. Build it twice with the same inputs, get identical output.

Incremental builds are possible because every step is a pure function of known inputs. The dependency graph tracks which content files affect which output pages, so only changed pages need to be re-rendered.

## Links As Composition

Links are the composition mechanism at every level.

A content link is a reference: this document mentions that document. The link is typed — the validator confirms the target exists and matches the expected schema.

A collection link is a query: give me all content of this type, filtered and sorted. The result is a validated list that satisfies the collection schema for that type.

A template composition is a link: this template delegates to that template for a subtree. The pipe and juxt combinators are functional links — they connect a value to a transformation.

The uniformity is intentional. Every relationship in Presemble is expressed as some form of link: references between content items, queries over collections, delegation between templates. The link is the join.

This means the site is a graph, not a directory tree. The filesystem is the storage format; the graph is the semantic model. The builder constructs the graph at the start of every build, validates it, and walks it to produce output.

Links are also the extension point. Want to include a list of related posts on a blog post page? The post content file links to a collection filtered by shared tags. Want a homepage with curated sections? The homepage content file links to whichever collections it chooses to assemble. The editorial structure is in the content, not scattered across templates and configuration files.

The document promises. The template presents. The builder validates in between.
