# Building Presemble With Presemble

This site is built with Presemble. Every page you are reading was authored in a schema-validated markdown file and assembled by the same publisher that Presemble ships as its primary deliverable.

 site proves the philosophy: content is data, templates are data, and schemas are the contracts between them. Nothing on these pages was produced by string interpolation. Every heading, paragraph, and link travelled through a named slot from author to template to output.

[Joakim Ohlrogge](/author/johlrogge)

----

### Eating our own cooking

Using Presemble to publish the Presemble site is the earliest possible dogfooding. It forces the tool to support a real multi-content-type site — authors, features, and posts — before any external user encounters those content types. Gaps in the schema system, the template vocabulary, or the build pipeline surface here, where they can be fixed without breaking anyone else.

### What the fixture proves

The presemble-site fixture demonstrates three things. First, schemas work for promotional content just as well as for editorial content — the feature and post schemas are as strict as any blog article schema. Second, the template vocabulary covers a site with multiple content types and an index page that aggregates them both. Third, Presemble can describe itself: the tool is coherent enough to serve as its own editorial platform.

### The development loop

Building this site was done with presemble serve site/ running in a terminal. Every content edit triggered a partial rebuild in under a second. The feature pages went through many drafts — each save rebuilt only the changed feature page and the index, not the whole site.

The same validation that runs at publish time runs on every save. Typos in schema constraints, broken links between pages, missing asset files — all caught immediately, not at deploy. There is no separate "dev mode" that skips checks.

This is what "hard gate" means in practice: the feedback loop is fast enough that you find out about problems before you leave the file.
