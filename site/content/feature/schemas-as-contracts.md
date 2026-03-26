# Schemas as Contracts

Your content is data, not text.

A Presemble schema is a document grammar written in plain markdown — it defines the exact sequence of elements a document must contain, with each position named, typed, and constrained. The publisher enforces it at build time. No runtime surprises.

----

### What a schema looks like

```markdown
# Your post title {#title}
occurs
: exactly once
content
: capitalized

Your article summary. {#summary}
occurs
: 1..3

[<name>](/author/<name>) {#author}
occurs
: exactly once

----

Body content. Headings H3–H6 only.
headings
: h3..h6
```

### What a valid content file looks like

Just plain markdown — no annotations, no schema syntax leaking into the author's document:

```markdown
# Building Presemble With Presemble

This site is built with Presemble. Every page you are reading was validated at build time.

[Joakim Ohlrogge](/author/johlrogge)

----

### Why this matters

Content that violates its schema fails the build. The author gets a clear error
pointing to the exact constraint that was violated.
```

### Named slots

Every piece of content in a schema-validated document is reachable by a dotted path: `feature.title`, `feature.description`, `post.author`. Templates traverse these paths to build output. Because the schema defines which paths exist, the template vocabulary is finite and verifiable.
