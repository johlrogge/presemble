# Validated at Every Level

A broken link fails the build — in content, in templates, and in output.

Because templates are XML DOM trees, the publisher can extract every asset reference
and internal link by walking the tree — no string scanning, no guessing. If a
`<link href="/assets/style.css">` references a file that does not exist, the build
fails before any output is written.

The same applies to content: if an article links to an author page that has not been
built, the link validator catches it. Every internal reference is verified against the
set of built pages.

----

### What string templates cannot do

A Jinja or Handlebars template is a text file. To find links, you scan strings — fragile
and easy to miss. With structured templates, link discovery is a tree walk:

```xml
<link rel="stylesheet" href="/assets/style.css" />
<img src="/images/hero.jpg" alt="Hero" />
```

The publisher extracts `/assets/style.css` and `/images/hero.jpg` from the DOM, verifies
each file exists, and copies only referenced assets to output.

### Content links are validated too

An article can link to an author:

```markdown
[Joakim Ohlrogge](/author/johlrogge)
```

If the author page does not exist, the build reports a broken link — not a 404 at runtime.
The data graph knows which pages were built.
