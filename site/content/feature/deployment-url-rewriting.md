# Deployment URL Rewriting

URLs are a hosting decision, not a content decision.

Authors write root-relative paths — `/author/johlrogge`, `/assets/style.css` — and the publisher transforms them at serialization time. Relative by default, root-relative with a base path, or fully absolute. Content and templates never contain deployment details.

No template changes are required when you move a site from a subdirectory to a custom domain. Configure once, build everywhere.

----

### Relative by default

The default output style converts every root-relative URL to a path relative to the output file. A link to `/assets/style.css` from a page at `/guide/getting-started/index.html` becomes `../../assets/style.css`. The site works at any root with zero configuration.

### Root-relative with base path

For GitHub Pages or a staging subdirectory, set a `base-path` in `.presemble/config.json`. The publisher prefixes every URL with the base path, producing `/my-project/assets/style.css`. Useful when the site is mounted under a non-root path.

### Absolute URLs

For RSS feeds, Open Graph tags, and canonical `<link>` elements, configure `base-url` in `.presemble/config.json`. The publisher emits fully qualified URLs such as `https://example.com/guide/getting-started`. Content authors write the same root-relative paths regardless.

### Multiple deployment targets

One source tree, several named config files. Keep `.presemble/github-pages.json` for the preview environment and `.presemble/production.json` for the live site. Select at build time with the `--config` flag:

```
presemble build site/ --config .presemble/production.json
```
