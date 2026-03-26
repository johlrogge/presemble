# ADR-003: Markdown Parser Choice

## Status

Accepted

## Context

Presemble's content model treats markdown as a structural language, not a presentation language.
The content parser extracts a semantic element sequence (headings, paragraphs, images, links,
tables, separators) and passes everything else through as prose. This is fundamentally different
from the "parse markdown → generate HTML" use case that most markdown parsers are designed for.

Three options were considered:

1. **pulldown-cmark** — CommonMark compliant, event-based, already in use
2. **comrak** — CommonMark + full GFM extensions, AST-based
3. **Presemble Markdown** — a purpose-built owned grammar

The core tension: options 1 and 2 are "generous" parsers designed to accept as much as possible
and generate HTML. Presemble needs a parser that makes structure explicit and passes everything
else through as prose. These are opposite philosophies.

## Decision

Continue with `pulldown-cmark`, with the table extension enabled (`Options::ENABLE_TABLES`).
This is a pragmatic choice, not a permanent one.

Rationale:
- Already in use; zero migration cost at this stage
- Tables (the primary structural addition needed for ADR-002 constraint syntax) are available via
  a single flag
- The event-based API maps well to the existing content parser implementation
- The project is not yet at the scale where parser generosity causes real problems

### What "not married to it" means

We are aware that pulldown-cmark is designed to be generous — it accepts inline HTML, unknown
constructs, and anything that can produce valid HTML output. Presemble's content model actively
works against this: we want a restrictive, predictable subset.

The threshold for revisiting this decision is when we find ourselves **fighting the parser's
generosity** — writing significant post-processing logic to reject, strip, or reinterpret things
the parser accepted but Presemble should not allow. Signs this is happening:

- Inline HTML requires active filtering rather than being naturally absent
- The "unknown = prose fallthrough" behaviour requires non-trivial workarounds
- Table semantics from GFM conflict with Presemble's table-as-constraint model
- Significant effort goes into working around pulldown-cmark rather than building Presemble

At that point, either comrak (for a richer but still generous parser) or a purpose-built
Presemble Markdown grammar (for full control) should be evaluated.

## Alternatives considered

**comrak** — Full GFM including definition lists, AST-based output. More extension surface than
needed right now. Still HTML-biased; would face the same generosity problem, just with more knobs
to turn. Deferred.

**Presemble Markdown** — A purpose-built grammar where "structural vs prose" is a first-class
design decision, not post-processing. Inline HTML rejected at the grammar level. The parser *is*
the content model with no impedance mismatch. High cost — months of investment, zero tooling,
no editor support. The right long-term answer if generosity becomes a sustained problem.
Deferred.

## Consequences

**Positive:**
- No migration work; existing parser implementation unchanged
- Table support is one flag away
- Decision is explicitly provisional — revisiting it is expected, not a failure

**Negative / watch points:**
- Inline HTML will pass through unless explicitly filtered — track where this matters
- pulldown-cmark has no definition list support; if ADR-002's table syntax proves awkward and
  definition lists are reconsidered, a parser switch becomes necessary
- The event-based API makes complex inline structure (inline link patterns, inline semantics)
  harder to extract than an AST would be
