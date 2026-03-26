# Validated at Every Level

You cannot publish a broken site.

The build fails if any content violates its schema, any internal link points to a page that does not exist, or any template references an asset file that is missing. Every constraint is checked before a single output file is written. There is no partial publish, no silent degradation, no 404 waiting in production.

----

### Asset discovery from the DOM

Because templates are structured trees, the publisher discovers every asset reference by walking the DOM — no string scanning, no fragile regex. Referenced files are verified to exist and copied to output; unreferenced files are not.

```xml
<link rel="stylesheet" href="/assets/style.css" />
<img src="/images/hero.jpg" alt="Hero" />
```

The publisher extracts `/assets/style.css` and `/images/hero.jpg`, verifies each file exists, and fails the build if either is missing.

### Broken links are build errors

```
building-presemble.md: FAIL
  [BROKEN LINK] post/building-presemble/index.html: broken link → /author/unknown
```

The author page `/author/unknown` was not built. The build stops here. Fix the link or add the author document — then try again.

### Schema violations are build errors

```
building-presemble.md: FAIL
  [SCHEMA] post/title: expected capitalized, got lowercase
  [SCHEMA] post/author: expected exactly once, found 0
```

The content file does not satisfy its schema. Each violation is reported with the slot path and the constraint that failed. No guessing, no reading between the lines.
