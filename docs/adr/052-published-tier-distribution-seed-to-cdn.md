# ADR-052: Published-tier distribution: seed to CDN

## Status
Proposed

## Decision

This ADR answers the **published-tier serving** question that ADR-050 deferred to a Phase-2 spike. It separates the *authored origin* from the *public delivery network*.

**A published site is a "seed."** Publishing produces static output (ADR-007, ADR-049) — the authoritative bytes for a site at a point in time. That seed is the thing distributed; it is not itself the delivery surface.

**Origin and distribution are separate layers:**
- **Origin** — the seed lives on **Bahnhof S3** (the content-addressable archive of ADR-048/050, or a materialized/edge-alias projection of it). Bahnhof is S3-API-compatible with **free/unlimited egress** but is **API-only**: no S3 website endpoint, no CDN, no edge compute (Bahnhof spike, 2026-06-24). So Bahnhof alone cannot serve clean public URLs — a layer in front is required.
- **Distribution** — a **third-party CDN** sits in front of the origin and delivers the public site globally. **Bunny CDN** is the concrete candidate: very low $/GB, global PoPs, TLS, and edge rules capable of the clean-URL → object mapping. The CDN provides the public performance, caching, and clean-URL surface that Bahnhof does not.

**Hashes stay internal (per ADR-050).** End users — authors and the public — only ever see clean URLs, never content hashes. The distribution layer resolves clean URLs to objects; whether that is a **materialize** step (copy bytes to per-site clean keys at publish) or an **edge-alias** map (name→hash rewrite at the CDN edge, keeping dedup) remains the ADR-050 fork and is not re-decided here. Bahnhof's free egress makes even the byte-copying "materialize" option cheap (storage cost only).

**Metered CDN egress is the fair billing lever.** Bahnhof S3 egress is free, but a global CDN's egress is not — and it is naturally **per-site/per-tenant by construction**. Charging on metered CDN egress dissolves the "how do you bill deduped storage-at-rest?" ambiguity from ADR-050: you bill the delivery you can attribute, not the shared bytes you cannot cleanly divide.

**No conductor in the hot path.** The public tier is static: CDN → origin object storage, with the conductor entirely out of the request path (consistent with ADR-048's published stage). The conductor's role ends at producing and placing the seed.

## Why

1. **Bahnhof forces a front layer.** The 2026-06-24 spike established Bahnhof S3 is API-only with free egress — excellent as an origin, unusable as a public website surface. A CDN in front is not optional, it is required.
2. **Seed/origin/distribution is a clean separation.** The authored bytes, where they rest, and how they reach the public are three concerns; conflating them was what made the serving question feel hard.
3. **Free origin egress + cheap CDN is a favourable cost structure.** Bahnhof's free egress means the CDN pulls from origin for free; only CDN-to-user egress costs, and that is exactly the metered, attributable, billable quantity.
4. **Metered egress is fair and unambiguous** where deduped storage-at-rest billing is not (ADR-050).

## Alternatives considered

- **Serve directly from Bahnhof S3** — impossible for clean public URLs; API-only, no website endpoint or CDN.
- **Self-hosted Caddy edge-alias proxy on a Bahnhof VM** (the ADR-050 lean) — viable and keeps everything in-house with free egress, but puts a proxy we operate in the hot path and gives up a global CDN's PoPs and caching. Retained as a fallback / lower-cost origin-fronting option; the Bunny CDN path is preferred for global performance and to move egress into a metered, billable lever.
- **A hyperscaler CDN (Cloudflare/Fastly/CloudFront)** — also viable; Bunny is chosen as the concrete candidate for its low $/GB and simplicity. The decision is "a third-party CDN fronting Bahnhof," not a lock-in to one vendor.
- **Bill deduped storage-at-rest instead of egress** — rejected as ambiguous (logical vs physical bytes); metered egress is cleaner.

## Consequences

- A **publish → place seed on Bahnhof S3 → serve via CDN** pipeline; the conductor produces the seed and is out of the public hot path.
- A **CDN integration** (Bunny candidate): origin pull from Bahnhof, TLS, clean-URL delivery, and (for edge-alias) name→hash rewrite at the edge.
- The **materialize vs edge-alias** choice (ADR-050) is inherited, not resolved here; free Bahnhof egress makes materialize cheaper than feared.
- **Egress metering** per site/tenant becomes the billing substrate — requires attributing CDN egress to sites (a billing/metering mechanism, still unspecified — see ADR-051 open questions).
- **Vendor portability:** because the decision is "a CDN in front of an S3 origin," swapping CDNs is a configuration change, not an architecture change.
- Builds on ADR-050 (published-tier serving open question, shared hash universe), ADR-048 (published stage / content lifecycle), ADR-049 (publish produces the seed), ADR-007 (crawl-based publishing), ADR-009 (clean-url convention). Accounts/tenancy that this bills against are ADR-051.
