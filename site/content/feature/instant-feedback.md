# Instant Feedback

Edit a content file, see the result in under a second.

`presemble serve` builds your site, starts a local HTTP server, and watches your
schemas, content, and templates for changes. When a file is saved, only the affected
pages rebuild — not the whole site. Edit an article, only that article and its index
rebuild. Change a template, only the pages using that template rebuild.

The same validation that runs at publish time runs on every save. There is no "dev
mode" that skips checks — broken content fails fast in development, not at deploy time.

----

### How it works

Start the server:

```
presemble serve site/
```

Then edit any file under `content/`, `schemas/`, or `templates/`. The terminal shows
what rebuilt:

```
Rebuilding 2 page(s)...
  → site/output/post/building-presemble/index.html
  → site/output/index.html
Rebuild complete (2 file(s))
```

### Incremental rebuild

The publisher tracks a dependency graph: each output page records which schema, content
file, and template it was built from. When a file changes, only the pages that depend
on it rebuild. A site with 500 articles rebuilds 1 article (plus the index) when you
fix a typo — not all 500.
