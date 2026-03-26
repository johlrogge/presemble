# Templates Are Data

Your templates are trees, not text with holes.

Presemble templates work by DOM transformation, not string interpolation. The template is parsed into a tree of nodes; the compiler walks that tree and replaces `presemble:insert` directives with structured content. The output document is assembled from typed nodes, never by splicing raw strings together.

Structural validity is guaranteed by construction. Because the template is parsed before any content is inserted, mismatched tags are caught at parse time. You cannot produce malformed HTML through a schema violation; the template itself must be valid before the compiler will touch it.

The template vocabulary is small and declarative: insert a slot, iterate over a collection, conditionally show a block, or set an attribute. Every operation is transparent — there is no hidden rendering logic, no implicit escaping policy, and no template language to learn beyond four primitives.

----

### Surface syntax is a parser choice

The `presemble:insert` directives are written in HTML, but the underlying tree is format-agnostic. The same transformation logic could consume a template expressed in EDN, YAML, or any other format that can describe a labelled tree. HTML was chosen because browsers already understand it and authors already write it.

### Schema-derived semantic classes

Because the schema defines every slot by name, the compiler can annotate output elements with semantic CSS classes derived from the slot path. A `feature:title` slot becomes an `h1` carrying the class `feature__title`. Styling follows structure — you never need to guess which element carries which content.

### What a template looks like

```xml
<main class="feature-grid">
  <template data-each="site:features">
    <article class="feature-card">
      <presemble:insert data="title" as="h3" />
      <presemble:insert data="tagline" />
      <presemble:insert data="link" />
    </article>
  </template>
</main>
```

This is valid XML. The publisher parses it into a DOM tree, replaces each
`<presemble:insert>` with a typed node from the data graph, and serialises the result.
No string interpolation occurs at any point.

### The output

```html
<main class="feature-grid">
  <article class="feature-card">
    <h3 class="feature-title">Schemas as Contracts</h3>
    <span class="feature-tagline">Your content is data, not text.</span>
    <a href="/feature/schemas-as-contracts" class="feature-link">Schemas as Contracts</a>
  </article>
  <!-- repeated for each feature -->
</main>
```
