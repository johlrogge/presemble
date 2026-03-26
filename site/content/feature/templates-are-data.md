# Templates Are Data

Your templates are trees, not text with holes.

Presemble parses templates as structured DOM trees and transforms them — string manipulation only at the final serialization step. Structural validity is guaranteed by construction: mismatched tags and broken nesting are caught at parse time, not in production. The surface syntax is a parser choice; the underlying tree is format-agnostic.

----

### HTML surface syntax

The default template format uses HTML with `presemble:insert` directives. The publisher parses it into a DOM tree, replaces each directive with a typed node from the data graph, and serialises the result. No string interpolation occurs at any point.

```xml
<main class="feature-grid">
  <template data-each="site.features">
    <article class="feature-card">
      <presemble:insert data="title" as="h3" />
      <presemble:insert data="tagline" />
      <presemble:insert data="link" />
    </article>
  </template>
</main>
```

### Hiccup surface syntax

Because the internal model is a labelled tree, the same transformation logic can consume any format that describes one. Hiccup expresses the same template as a Clojure data literal — useful in editor tooling and REPL-driven workflows:

```clojure
[:main.feature-grid
 [:template {:data-each "site.features"}
  [:article.feature-card
   [:presemble/insert {:data "title" :as "h3"}]
   [:presemble/insert {:data "tagline"}]
   [:presemble/insert {:data "link"}]]]]
```

### Schema-derived semantic classes

Because the schema defines every slot by name, the compiler annotates output elements with semantic CSS classes derived from the slot path. A `feature.title` slot becomes an `h3` carrying the class `feature__title`. Styling follows structure — you never need to guess which element carries which content.

### The output

```html
<main class="feature-grid">
  <article class="feature-card">
    <h3 class="feature__title">Schemas as Contracts</h3>
    <span class="feature__tagline">Your content is data, not text.</span>
    <a href="/feature/schemas-as-contracts" class="feature__link">Schemas as Contracts</a>
  </article>
  <!-- repeated for each feature -->
</main>
```
