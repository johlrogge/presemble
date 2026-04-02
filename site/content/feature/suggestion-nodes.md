# Suggestion Nodes

The page always renders, even when content is missing.

When a content slot is absent or invalid, Presemble replaces the error page with an inline placeholder rendered from the schema itself. Hint text, field names, and example values from the schema become soft, visually distinct suggestions directly on the page. Nothing breaks. The page is a scaffolded guide to what belongs there.

----

### No error pages

A missing required slot in a content file does not crash the preview. Instead, the served page renders the slot as a suggestion node: a clearly styled element showing the hint text from the schema. The author sees exactly what is expected and where it goes.

### Schema-driven placeholders

Every suggestion node is derived from the schema declaration for that slot. If the schema says:

```markdown
A one-sentence summary of the post. {#summary}
occurs
: exactly once
```

Then a missing `summary` slot renders as a placeholder carrying that hint text. The page is always browsable — a brand new content file with no fields filled in renders as a fully scaffolded page.

### Visually distinct

Suggestion nodes use a soft, neutral style that makes them immediately recognizable as placeholders, not real content. They never appear in a published build — `presemble build` still fails on missing required slots. Suggestion nodes are a `presemble serve` authoring aid.
