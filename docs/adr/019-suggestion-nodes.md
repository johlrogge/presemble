# ADR-019: Suggestion nodes replace error pages

## Status

Accepted

## Context

When content failed schema validation, the publisher skipped the page entirely. In serve mode, a
red error page was shown. This is developer UX, not author UX. Content authors working in serve
mode should see their page with helpful "fill this in" placeholders — not a wall of red error text
that blocks them from seeing the layout they are filling in.

## Decision

Pages always render. Missing required content appears as warm, inviting placeholder nodes
(suggestion nodes) instead of causing the page to be skipped.

Implementation:

- `Value::Suggestion` variant in the data model carries hint text from the schema
- `build_article_graph` fills missing or empty slots with `Suggestion` values derived from schema
  hints
- Template transformer renders suggestions as styled HTML elements with the
  `presemble-suggestion` class and `data-presemble-hint` attribute
- CSS `::before` pseudo-element displays the hint text as grey placeholder (like an HTML input
  `placeholder`)
- Build pipeline prints `SUGGESTIONS` instead of `FAIL` for validation issues
- `BuildOutcome` tracks `files_with_suggestions` separately from `files_failed`
- Error pages are gated to parse errors only — malformed markdown that cannot produce a `Document`
- At publish time (`presemble build`), missing required slots remain validation errors. Suggestion
  nodes are a development aid, not a way to publish incomplete content.

## Alternatives considered

**Keep skipping invalid pages** — blocks the editorial workflow. An author who has not yet filled
in a required slot cannot see the page at all, making it impossible to evaluate the layout while
writing.

**Show error pages for all validation failures** — too aggressive for a content authoring tool.
A missing slot is not a malformed file; it is an expected state during authoring.

**Render with suggestions and emit errors** — this is what the implementation does. Suggestion
nodes appear in the browser; validation issues are printed to stdout. Both signals are present
without one blocking the other.

## Consequences

**Positive:**

- The page always renders in serve mode. Authors see the layout with placeholders and can fill
  slots in incrementally.
- The schema becomes the scaffold for new content — hint text from the schema appears directly in
  the browser where the content should go.
- `files_with_suggestions` in `BuildOutcome` gives CI/CD pipelines a clean signal for strict
  validation without coupling it to the serve-mode experience.

**Negative:**

- `Value::Suggestion` must be handled in all `match` arms on `Value` across the codebase. Adding
  the variant is a breaking change to any exhaustive match.
- CI/CD pipelines that want strict validation must explicitly check
  `files_with_suggestions > 0`. A pipeline that only checks for a non-zero exit code will not
  catch pages with missing required slots when running in serve mode.
