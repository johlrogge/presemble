# template

DOM template renderer for Presemble.

Parses template files (`.html` or `.hiccup`) into an internal DOM tree, binds data-graph values at named paths, and serialises the result to HTML. All transformation happens on the tree; string manipulation only occurs at the final serialisation step.

## Responsibilities

- Parse HTML and Hiccup/EDN surface syntaxes into a shared DOM tree (ADR-004, ADR-011)
- Evaluate the one expression language (ADR-067) wherever a template reads its input: the `data` of `presemble:insert`, `data-each`, `data-slot`, any attribute value, and a call in child position. Expressions are `(ref path)` (hiccup shorthand: a bare symbol), calls of the pure functions `ref`, `text`, `to_lower`, `to_upper`, `capitalize`, `truncate`, `juxt` and `->`, calls of templates (`(card item)`, `((juxt header self/body footer) input)`), and literals
- Place values: an insert without `as` places the value as it is; with `as` it is an element carrying the semantic class and, in editorial output, the editor attributes
- Wrap link-record inserts with an `<a>` element when `as` is specified
- Resolve `presemble:define` local template definitions and template calls (ADR-013, ADR-067)
- Check every expression of a template against the schema, the site's templates and its collections (`check::check_template`, `CheckContext`)
- Extract asset references (`<link href>`, `<img src>`, `<script src>`) for build-time verification (ADR-010)
- Serialise the final tree to HTML with deployment URL rewriting (ADR-014)

## Used by

`lsp_capabilities`, `publisher_cli`

---

[Back to root README](../../README.md)
