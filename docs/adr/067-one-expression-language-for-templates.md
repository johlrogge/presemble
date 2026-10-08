# ADR-067: One expression language for templates

## Status

Proposed

## Context

- Principle (user, 2026-10-08): templates are pure functions. Their output
  depends only on their immutable input; they reference it and set nothing.
  See ADR-005, ADR-030, ADR-064.
- Templates read their input in three different ways today:
  1. path strings read by directives (`:data "item.link"`, `:data-each
     "input.post"`, `:data-slot`), split on `.`;
  2. EDN expressions: `:apply` and calls in child position
     (`((juxt header self/body footer) input)`);
  3. string mini-languages: `presemble:class`, evaluated by a separate `|`
     pipe parser.
- Attribute values cannot read the input. `data-href` was documented for that
  and never implemented.
- Templates are included in two ways: `presemble:include`/`presemble:apply`
  and calls in child position.
- Principle (user): exceptions and special cases indicate a missing
  abstraction. This repository holds every template, so no compatibility is
  kept.

## Decision

1. **One expression language.** A template expression is an EDN form over the
   immutable input:
   - `(ref p)` references the input: `p` is a dotted path
     (`item.link.href`) whose root is `input`, the name `data-each` binds
     (default `item`), or a collection. `ref` is the identity: of a
     literal, it gives the literal.
   - A list is a call of a pure function or of a template: `(text
     input.title)`, `(-> input.title text to_upper)`, `((juxt header
     self/body) input)`. A template is a function of its input. The
     functions are `ref`, `text`, `to_lower`, `to_upper`, `capitalize`,
     `truncate`, `juxt` and `->`; others are added when a template needs
     them.
   - A string, integer or keyword is a literal.

   There is one evaluator and one resolver.
2. **Hiccup shorthands, the same EDN.**
   - A bare symbol in an expression position is `(ref symbol)`:
     `{:data item.link}` means `{:data (ref item.link)}`.
   - A list in child position is `[:presemble/insert {:data (…)}]` without
     `:as`.
   - A string attribute value is literal text.
3. **Every position that reads the input takes an expression:**
   - `presemble:insert`'s `:data`: it places the value in content position,
     with `:as` naming the wrapping tag. `:apply` is removed; functions are
     called in the expression;
   - `data-each`, which binds each element under `:item` (a name, default
     `item`);
   - `data-slot`;
   - any attribute value.
4. **Placing a value is one mechanism.** A template's output is a value
   (node trees); a template is included by inserting a call of it.
   `presemble:include` and `presemble:apply` are removed.
5. **Removed:** `presemble:class`, the `|` pipe parser (`Expr`,
   `Transform`), path strings in directives, `:apply`, `data-href`, and the
   old design fixtures under `fixtures/blog-site/templates/`.
6. **Values.**
   - In content position a value renders as today: inline nodes, node trees
     and records, with editor attributes in editorial output.
   - In attribute position:
     - text, inline and node values give their plain text (AsPlainText,
       ADR-059 §8);
     - integers give their decimal text;
     - a record or a list is a template error: name the field
       (`item.link.href`, `item.cover.src`).
   - An expression that resolves to nothing produces nothing: no content,
     and the attribute is left out rather than written empty.
7. **Escaping.** Values are text or node trees, never markup. The serializer
   escapes attribute values and text exactly as it does literals.
8. **HTML templates** write the same EDN.
   - An attribute value that reads as an EDN list form is an expression:
     `<a href="(ref item.link.href)">`,
     `<presemble:insert data="(text input.title)" />`,
     `<template data-each="(ref input.post)">`,
     `<presemble:insert data="((juxt header self/body footer) input)" />`.
   - Any other value is literal text. A literal that would read as a list
     is written as an expression: `title='(ref "(draft)")'`.
   - `presemble convert` maps the two syntaxes, writing the hiccup
     shorthand when it outputs hiccup.
9. **Subject.** Every expression has a subject: the path its `ref` reads
   (the first `ref` argument of a call), or none. The semantic CSS class and
   the editor attributes of an insert come from the subject, as they came
   from the path string before.
10. **Checking.** The template checker and the LSP walk every expression in
    every position, in both syntaxes:
    - errors:
      - an unknown path, template or function;
      - a wrong number of arguments;
      - a record or a list in attribute position;
    - warning: an HTML attribute value that starts with `(` but does not
      read as a form.

## Alternatives considered

- **Setting or binding attributes from data:** rejected as framing; a
  template sets nothing.
- **Path strings for directives and expressions elsewhere:** rejected. Two
  syntaxes for one concept.
- **Braces in HTML (`{…}`):** rejected. Braces are EDN's map literal, so it
  would be a special case.
- **A `ref:` attribute prefix in HTML:** rejected. Directive attributes would
  be the exception.
- **String interpolation:** rejected. Composing a value is a function call.

## Consequences

- Every template in the repository is migrated in the implementing package.
  Published output is unchanged.
- `presemble init` writes working listings and "Read more" links.
- One expression evaluator, one placement mechanism (insert), one checker.

## Related

ADR-005, ADR-011, ADR-013, ADR-030, ADR-059 §8, ADR-064.
