# The Data Graph

Schemas, content, and templates meet in a single data graph.

Everything in a Presemble site is structured data. Schemas define the shapes. Content files fill those shapes with values. Templates describe how to traverse the shapes and emit output. All three are nodes in a single graph connected by named paths.

Templates traverse the graph rather than interpolate strings. A `presemble:insert data="feature:title"` directive is a graph traversal: follow the edge labelled `feature`, then follow the edge labelled `title`, and insert whatever node you find. There is no string manipulation; the path is resolved against a typed data structure.

The path from schema to content to template is traceable and type-safe. The schema declares that `feature:title` is a capitalized heading occurring exactly once. The content file is validated to carry exactly that. The template is checked to reference only paths the schema declares. Every connection is verified before a single byte of output is written.

----

### Content links

Relationships between content documents are expressed as typed links, not URL conventions. An author reference in a post is a link node whose target is an author document. The compiler resolves the link, validates that the target exists and matches the expected schema, and renders the relationship however the template requests. Broken links are build errors, not 404s.

### Compile-time completeness

Before the compiler emits any output, it verifies that every template reference resolves to a slot declared in a schema, every content document satisfies its schema, and every cross-document link points to a document that exists. Completeness is not checked at request time; it is checked once, before publication, when you can still fix it.

### Traversing the graph

When a post schema declares an author link, the publisher knows which author page a post refers to:

```markdown
[<name>](/author/<name>) {#author}
occurs
: exactly once
```

In the content file, the author writes:

```markdown
[Joakim Ohlrogge](/author/johlrogge)
```

The publisher validates that `/author/johlrogge` exists as a built page. If it does not,
the build fails. This is not a URL convention — it is a first-class edge in the data graph
that is verified at compile time.

### What the validator catches

```
building-presemble.md: FAIL
  [BROKEN LINK] post/building-presemble/index.html: broken link → /author/unknown
```

If the author page does not exist, the build fails with a clear error — not a 404 at runtime.
