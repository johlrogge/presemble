# Schemas as Contracts

Your content is data, not text.

Schemas define document grammars in plain markdown. Each heading, paragraph, and list item has a named slot, and the schema declares exactly which slots exist, how many times they may appear, and what constraints they carry.

The publisher enforces schemas at build time. If a content file is missing a required slot or contains disallowed content, the build fails before any output is written. This is a compile-time guarantee, not a runtime surprise.

The schema is a contract between authors and templates. Authors know precisely what to write because the schema names every slot. Templates know precisely what they will receive because the schema guarantees completeness. Neither side can surprise the other.

----

### Why markdown?

Markdown is the lowest-friction structured text format that non-programmers will actually use. By embedding slot annotations as inline attributes — `{#title}`, `{#tagline}` — Presemble extends markdown without replacing it. Authors write in their familiar tool; the compiler reads the structure.

### Named slots

Every piece of content in a schema-validated document is reachable by a dotted path: `feature:title`, `feature:description`, `post:author`. Templates traverse these paths to build output. Because the schema defines which paths exist, the template vocabulary is finite and verifiable.
