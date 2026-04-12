# ADR-034: Asset lifecycle and publish gate
## Status
Proposed
## Decision

Assets (images, fonts, videos) become a first-class concept with a managed lifecycle. An asset server processes assets according to a schema-driven contract. A publish gate prevents pages from going live until their assets are ready.

**Asset schema as shared contract.** `schemas/assets/index.md` defines variant specs. Shared between content authors, templates, and the asset server. The conductor registers the schema with the asset server.

**Fragment syntax for variants:**
```markdown
![cover](/assets/cover.jpg#thumb)
```

**Size tables in schemas:**
```markdown
|size   |width|height|
|thumb  |  320|  200 |
|preview|  800|  600 |
|full   | 1920| 1024 |#{sizes}
```

Tables become data graph entries: `site.sizes.thumb` → `{width: 320, height: 200}`.

**Three asset-server states:** MISSING → STORED → READY. Publisher owns READY → PUBLISHED (CDN push).

**Publish gate:** Publisher refuses if assets aren't READY. `presemble status [-v]` reports readiness.

**Local development:** `presemble serve` proxies asset URLs to the asset server. No local copies needed.

## Why

1. **Schema as contract.** Same pattern as content — schema defines structure, asset server fulfills it.
2. **No local copies.** Hosted sites shouldn't download every asset.
3. **Publish safety.** Pages with unprocessed assets must not go live.
4. **Separation of concerns.** Asset processing belongs in a dedicated service.

## Alternatives considered

- **Copy files from directory (current)** — breaks for large/remote sites.
- **Git LFS** — tricky to work with, hard to avoid checking out the whole repo with assets locally. An asset server could be LFS-compatible as a backend, but LFS alone doesn't provide processing, lifecycle, or variant management.

## Consequences

- New `asset_store` component with build-time polymorphism
- Content validation includes asset and variant checks
- Publisher gains `presemble status` readiness check
- Supersedes ADR-027 (asset store and browser separation)
