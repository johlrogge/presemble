# The Data Graph

Schemas, content, and templates meet in a single data graph.

Every named slot, every content value, and every cross-content reference is a node in the data graph. Templates traverse it by path — `input.title`, `input.author.name`, `item.title`. Every connection is verified before any output is written.

----

### Content links

Relationships between content documents are expressed as typed links, not URL conventions. An author reference in a post is a link node whose target is an author document. The compiler resolves the link, validates that the target exists and matches the expected schema, and renders the relationship however the template requests.

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

The publisher resolves `/author/johlrogge` to a built page, validates that it satisfies the author schema, and makes the full author document available to the template via the `input.author` path. The link is a first-class edge in the graph, not a URL string.

### Reverse references

Links in Presemble are bidirectional. Any page can query which other pages link to it using a `refs-to self` expression. For example, an author page can declare a `posts` slot that is populated at build time with all posts that reference that author:

```markdown
[posts](->> :post (refs-to self))
```

The schema declares the slot with a link type:

```markdown
[<post>](/post/<slug>) {#posts}
type
: link(post)
occurs
: *
```

The publisher evaluates the expression, queries the edge index for all posts whose author link points to this author URL, and populates the slot with the resulting list. The template iterates it the same way as any collection.

The REPL exposes the same queries directly:

```
(refs-to "/author/alice")    ; all edges pointing at /author/alice
(refs-from "/post/hello")    ; all edges originating from /post/hello
```

Each result is a list of edge records with `:source` and `:target` keys.

### Compile-time completeness

Before the compiler emits any output it verifies: every template reference resolves to a slot declared in a schema, every content document satisfies its schema, and every cross-document link points to a document that exists. Completeness is not checked at request time — it is checked once, before publication, when you can still fix it.
