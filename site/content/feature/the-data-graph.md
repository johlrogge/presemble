# The Data Graph

Schemas, content, and templates meet in a single data graph.

Every named slot, every content value, and every cross-content reference is a node in the data graph. Templates traverse it by path — `post:title`, `author:name`, `site:posts`. Every connection is verified before any output is written.

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
