# ADR-061: Publish gate — a site with validation errors is not published

## Status

Accepted

## Context

- ADR-057 §5 makes schema validity a reporting concern for editing: invalid
  documents can be edited and saved, and are `Unpublishable` while they have
  diagnostics. It left "gating builds and publishing" to Wave 4.
- ADR-019 made every page render with suggestion nodes and expected CI to
  check `BuildOutcome::files_with_suggestions`. That counter has been 0 since
  the conductor took over rendering, so `presemble build` published invalid
  content and exited 0.
- ADR-060 §5 makes content that is not allowed where it sits an Error that
  cannot be published.
- The conductor parsed every page but never validated it, and silently skipped
  schemas and content that failed to parse.

## Decision

1. **All or nothing.** `presemble build` validates every schema and content
   file before writing anything. The whole site is published, or nothing is.
   Output from an earlier build is left untouched. There is no
   `--allow-invalid` flag.
2. **Errors gate; warnings do not.** Any Error makes the build exit 1 with
   `N validation error(s) in M file(s); site not published`. Warnings are
   printed and the build continues.
3. **What is an Error:**
   - a schema that fails to parse;
   - a content file that fails to parse;
   - a content file with no usable schema ("not built: schema `<path>` does
     not parse" or "not built: no schema for this page");
   - every Error diagnostic from validating a parsed document against its
     schema.

   The "no schema" error covers every stem the site has a schema for: a
   `schemas/<stem>/` directory (with an `item.md`) or the root
   `schemas/index.md`. A broken collection schema never falls back to the
   item schema.
4. **`serve` stays lenient.** It is the editorial preview: it renders every
   page and prints the same diagnostics with a summary line.
5. **Diagnostic format.** One line per diagnostic, site-relative path, 1-based
   line and column (columns count characters):
   `content/post/a.md:3:1: error: message`, or `content/post/a.md: error: message`
   when there is no position. Sorted by file, then position.
6. **`validation::ValidationReport`** is the gate. The conductor builds it while
   populating the node store; `Conductor::load` validates without rendering.
   Any future hosted publish path must reuse this report rather than build its
   own check.

## Consequences

- CI deploys fail on invalid content instead of publishing it.
- ADR-019's `files_with_suggestions` and `page_suggestions` are retired;
  suggestion nodes remain a serve-mode aid.
- The conductor (serve) is an editorial tool. In hosted deployments it sits
  behind a login wall that admits only redaktion members; restricting traffic
  is the deployment layer's job, not the conductor's. Anonymous readers see
  only the published site, which this gate produces.
- Not covered here: positions for parse errors, per-rebuild reporting and a
  browser overlay in serve, and content directories that have no schema.

## Amendments

### UM-1 (2026-10): markup that looks unfinished is an Error

- **What is reported:** a backtick attached to a word, `](`, `![` at the
  start of a word, or a one-sided `**`/`__` run, left as literal text in a
  paragraph, heading, table cell or link text, at any depth.
- **One Error per occurrence:** it carries the block's location and, with the
  source, the span of the flagged characters.
- **The way out:** a code span. An escape doesn't help, because the model
  holds the same literal text.
- **Overlap with link expressions:** a complete link expression written
  inside text is left to the link-expression check (ADR-058); an unclosed one
  is unfinished markup.
- **Before it was turned on:** the rule was checked against every content
  file with `just census-markup` (0 hits).
- **Related:** ADR-059's canonical form now escapes minimally, so unpaired
  runs are saved as typed.

## Related

ADR-019, ADR-057, ADR-058, ADR-059, ADR-060.
