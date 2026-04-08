# Site Wizard

Start a new site from a template — no manual file creation.

Point `presemble serve` at an empty directory and the browser opens a welcome page. Pick a starter template — blog, personal site, or portfolio — and choose your preferred template syntax: Hiccup or HTML. The wizard scaffolds a complete working site and redirects the browser to the homepage.

----

### Starting from an empty directory

```
mkdir my-site
presemble serve my-site/
```

If `my-site/` contains no Presemble files, the server detects the empty site and serves a welcome page instead of an error. The welcome page explains the site model and offers a list of starter templates to choose from.

### Starter templates

| Template | Contents |
|---|---|
| Blog | Post schema, author schema, homepage with recent posts listing |
| Personal | About page, project schema, homepage |
| Portfolio | Project schema with image slots, contact page, homepage |

Each starter includes schemas, example content, templates, and a stylesheet. The generated site builds and serves immediately after scaffolding.

### Template syntax choice

The wizard asks whether you prefer Hiccup (EDN) or HTML templates. Both are fully supported; the choice affects only the surface syntax of the generated template files. Hiccup is the primary format; HTML is the secondary. You can convert between them at any time with `presemble convert`.

### After scaffolding

The wizard writes the scaffold files to disk and the conductor picks them up via the file watcher. The browser navigates to the new site's homepage. From there, the normal serve workflow applies: edit content files, see changes in the browser, use the LSP for completions and diagnostics.

The `presemble init` command produces the same hello-world scaffold as before for scripted or non-interactive setups.
