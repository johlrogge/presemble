# Template Language Experiment Notes

These notes record honest observations from writing the blog-site fixture templates against
the ADR-004 proposed syntax. The goal is to surface friction before committing to a parser.

---

## Delimiter ergonomics: `{{ }}`

The `{{ }}` delimiter is readable and did not cause parsing ambiguity in the HTML written here.
However, it is already used by Jinja2/Tera/Handlebars in HTML contexts, which creates two
problems in practice:

1. **Editor confusion.** Most editors associate `{{ }}` in `.html` files with Jinja2 or
   Handlebars. Syntax highlighting and autocompletion will misfire. This is a tooling friction
   cost, not a language correctness cost, but it affects daily use.

2. **Mental model collision.** Authors who know Jinja2 will expect `{% for %}` and `{% if %}` to
   work. They will write them by reflex. The first time they try it and get an error, the
   mismatch becomes apparent — but the initial confusion is real.

`[[ ]]` would avoid both issues. It has no dominant prior art in HTML templates and is visually
distinct from `{{ }}`. The cost is that it looks slightly less "standard." On balance, `[[ ]]`
deserves serious consideration over `{{ }}` — but the fixtures were written with `{{ }}` as
specified to make the evaluation concrete.

---

## Cases where the pipe model felt strained

### Summary: multi-occurrence slot

The article schema declares `summary` with `occurs: 1..3` — up to three paragraphs. In
`article.html`, rendering all of them as a block with just `{{ article.summary }}` works if the
renderer emits each paragraph wrapped in `<p>` tags. But the template cannot control wrapping,
add classes to individual paragraphs, or interleave other HTML between paragraphs. If the
designer wants:

```html
<div class="summary">
  <p class="summary__lead">First paragraph</p>
  <p class="summary__continuation">Second paragraph</p>
</div>
```

there is no way to express this. The pipe model only has `each` (map over collection) and `maybe`
(zero-or-one). There is no way to say "take the second element" or "wrap each occurrence
differently based on its index."

The `first` filter used in `article_card.html` (`{{ article.summary | first }}`) is a useful
escape hatch when you only need one item. But it is the inverse problem: you cannot say "all
except the first" or "the last."

### Cover image context-dependence

`article_cover.html` and `cover_thumbnail.html` both reference `article.cover.path` and
`article.cover.alt`. This means both fragment templates are implicitly scoped to an article
context. If the same cover fragment were needed for a different content type (say, `event:cover`),
you would need duplicate templates. The pipe model has no mechanism for abstracting over the
source of a value — the path is always hardcoded into the fragment.

A possible fix would be to pass the cover value itself as the context for the fragment, so the
fragment could say `{{ cover.path }}` and `{{ cover.alt }}` rather than `{{ article.cover.path }}`.
But ADR-004 does not specify how `maybe` establishes the context for the fragment it calls.

This is a genuine underspecification: when `{{ article.cover | maybe(template:article_cover) }}`
invokes `article_cover.html`, what is the context? The full `article`, or just `article.cover`?
If the latter, the inner template cannot access `article.title` anymore — but cover fragments
probably should not need it. The ADR is silent on this.

---

## Cases that seemed to need conditionals

### Landscape vs portrait layout

The article cover schema specifies `orientation: landscape`. If orientation could vary, a template
might want to apply different CSS classes or layouts:

- landscape: `<figure class="cover cover--landscape">`
- portrait: `<figure class="cover cover--portrait">`

There is no way to express this in the current model. The ADR explicitly defers this case and
says to find the failure mode empirically. Here it is: any attribute value that depends on a
content field value requires branching. CSS classes are a frequent target.

A `when` or `match` pipe transform could handle this:

```
{{ article.cover.orientation | match(landscape => "cover--landscape", portrait => "cover--portrait") }}
```

That would fit the pipe model without introducing block directives. Whether it is sufficient
depends on how many discriminants are needed and whether the result is always a string.

### Author link presence

The article schema says `author` occurs exactly once and is a link. But if a different content
type had an optional author, the template would need to conditionally render the `<a>` tag
including its `href` attribute. `maybe` can conditionally include a fragment, but it cannot
conditionally include an attribute value inside an existing HTML element. An `href` attribute
that is sometimes empty and sometimes a URL is not the same as an element that is sometimes
absent.

---

## Cases where the syntax was unclear

### What does `{{ article.author.text }}` mean?

The schema defines author as a link: `[<name>](/authors/<name>)`. The sub-fields `.text` and
`.href` are not declared in the schema — they are implied by the link structure. It is not
obvious to a template author reading the schema that `.text` and `.href` are valid slot paths,
or whether the field names are conventional (`.text`/`.href`), structural (`.label`/`.url`), or
implementation-defined. The data graph needs a way to communicate its shape to template authors.

### `site.articles` provenance

`{{ site.articles | each(template:article_card) }}` assumes there is a `site` object with an
`articles` collection. Where does `site` come from, and how is it scoped into a template? The
`article.html` template uses both `site` (for header/footer) and `article` (for content). The
ADR does not specify how template context is established — whether it is a single root value or
a named map of values. If it is a single root, then `article.html` and `article_list.html`
cannot both work, because one needs an `article` root and the other needs a `site` root. If it
is a named map, the composition syntax `{{ site | template:header }}` needs clarification:
`site` is looked up from the map, then piped into `template:header` — that reading is coherent,
but it should be stated explicitly.

### Fragment file naming convention

There is no naming convention for fragment templates vs full-page templates. `article_cover.html`
and `cover_thumbnail.html` are fragments, while `article.html` and `article_list.html` are full
pages. Both live in the same directory. In practice you would want a way to distinguish them to
prevent fragments from being treated as page roots.

---

## What felt natural and ergonomic

The basic lookup and pipe chaining is genuinely clean. `{{ article.title }}` in a heading and
`{{ article.body }}` in a div read exactly like the output they produce. There is no boilerplate,
no escaping, no noise.

The `each` pattern for lists is a good idea. It keeps the list template (`article_list.html`)
minimal and moves the card layout to `article_card.html` where it belongs. The indirection is
low-cost because the file names are self-explanatory.

`maybe` for optional sections works well when the optional thing is a self-contained block (like
a cover image). It maps cleanly to the schema's optional slots.

The composition syntax `{{ site | template:header }}` reads well: "pipe the site value into
the header template." It is more uniform than Jinja2's `{% include %}` and makes the data
dependency explicit.

Overall, the model is coherent and covers the common cases without block directives. The gaps
identified above are real but bounded: context scoping for fragments, attribute-level
conditionals, and index-based access to multi-occurrence slots. These are solvable within the
pipe model with well-chosen transforms — they do not require abandoning the design.
