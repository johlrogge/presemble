# template

DOM template renderer for Presemble.

Parses template files (`.html` or `.hiccup`) into an internal DOM tree, binds data-graph values at named paths, and serialises the result to HTML. All transformation happens on the tree; string manipulation only occurs at the final serialisation step.

## Responsibilities

- Parse HTML and Hiccup/EDN surface syntaxes into a shared DOM tree (ADR-004, ADR-011)
- Evaluate `presemble:insert`, `data-each`, `presemble:include`, `data-href`, `presemble:class` directives
- Evaluate `:apply` expressions on `presemble:insert`: bare functions (`text`, `to_lower`, `to_upper`, `capitalize`, `truncate`) and pipe expressions `(-> text to_lower capitalize)`
- Wrap link-record inserts with an `<a>` element when `as` is specified
- Resolve `presemble:define` / `presemble:apply` template composition (ADR-013)
- Extract asset references (`<link href>`, `<img src>`, `<script src>`) for build-time verification (ADR-010)
- Serialise the final tree to HTML with deployment URL rewriting (ADR-014)

## Used by

`lsp_capabilities`, `publisher_cli`

---

[Back to root README](../../README.md)
