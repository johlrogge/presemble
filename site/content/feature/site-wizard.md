# Site Wizard

Start a new site from a template — pick a style, scaffold a working site in seconds.

Point `presemble serve` at an empty directory and the browser opens a guided setup wizard. Six steps take you from zero to a fully styled, working site. No manual file creation.

----

### Starting from an empty directory

```
mkdir my-site
presemble serve my-site/
```

If `my-site/` contains no Presemble files, the server serves the wizard instead of an error page.

### Six-step wizard

| Step | What you choose |
|---|---|
| Site type | Blog, personal site, or portfolio |
| Font mood | One of 7 curated type pairings (expressive, minimal, editorial, and more) |
| Color seed | A hue in degrees (0–360) — the base color for the theme |
| Palette type | Analogous, complementary, or triadic |
| Complexity | How dense the layout feels — sparse to rich |
| Template format | Hiccup (EDN) or HTML |

A live CSS preview panel updates on every step. The color step includes a light/dark theme toggle so you can see how the palette holds up in both modes.

### Generated stylesheet

The wizard passes your choices to the CSS generator, which produces a complete custom-property stylesheet using HSL color math. The stylesheet covers:

- Typography — font families and sizes for the chosen mood
- Color system — primary, secondary, and accent palettes derived from the seed and palette type
- Spacing and layout — adjusted for the complexity level
- Light/dark theme — a `prefers-color-scheme` block and a toggle class

The generated stylesheet is written to `assets/style.css` in the scaffolded site.

### Starter templates

| Template | Contents |
|---|---|
| Blog | Post schema, author schema, collection index, homepage |
| Personal | About page, page schema, homepage |
| Portfolio | Project schema with image slots, projects index, homepage |

Each starter includes schemas, seed content, Hiccup templates, navigation partials, and the generated stylesheet. The site builds and serves immediately after scaffolding.

### Navigation and collection index pages

Every starter includes a shared navigation partial via `presemble:include` and breadcrumb navigation on item pages. Collection index pages (e.g. `/post/`) list all items of that type. No page is a dead end.

### Seed content

Scaffolded sites include real starter content — real titles, summaries, and body text that illustrate the content model. Editing one of the seed posts and seeing the live reload is the intended first step.

### Template syntax choice

The wizard asks whether you prefer Hiccup (EDN) or HTML templates. The source of truth is always Hiccup; choosing HTML produces HTML files converted from the Hiccup originals. You can convert between formats at any time with `presemble convert`.

### After scaffolding

The wizard writes scaffold files to disk and the conductor picks them up via the file watcher. The browser navigates to the new homepage. From there, the normal serve workflow applies: edit content files, see changes in the browser, use the LSP for completions and diagnostics.

`presemble init <dir>` is still available for scripted or non-interactive setups — it produces a minimal hello-world site without the browser wizard.
