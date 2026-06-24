# ADR-050: Multi-tenancy via a shared content-addressable hash universe

## Status
Proposed

## Decision

Hosted Presemble is multi-tenant over a **single shared content-addressable hash universe** (the CAS of ADR-048), not per-tenant silos.

**A tenant is a namespace of refs.** A tenant ≈ a per-tenant **name→hash manifest** (clean names / URLs → content hashes). The manifest *is* the tenant's identity — it is where clean URLs and authorization live; the hash store is just bytes. (This is literally git: objects = hashes, refs/trees = names.) A tenant maps cleanly onto a workspace (ADR-035).

**Dedup is free, multitenancy is cheap.** Identical content is stored once and referenced by many tenants (the GitHub object-store model; assume no sha256 collisions). Many tenants share very little CPU/memory; the economics fit Bahnhof's ~2 TB object-storage minimum. **Exclusive (un-shared) storage is a paid upgrade.**

**Hashes are internal; the conductor is the trust boundary.** The conductor holds the storage credentials and is the only thing that turns a clean URL into a hash. **End users — authors *and* the public — only ever see clean URLs, never hashes.** The byte-server **never answers a bare "give me hash X"**; it only resolves hashes a tenant holds a ref to. A hash is therefore a lookup key behind an authz check, **not a capability**.

**Why the isolation holds (security reasoning).** A content hash is a one-way function of the content: you cannot derive a hash without already holding the bytes, and brute-forcing sha256 is infeasible — so there is **no fishing** for unknown content. The only residual is a *confirmation oracle* (someone who already has content X could learn the system stores it / that two tenants share an asset), and that closes once the byte-server resolves only owned refs. **One place it can re-appear: write-time dedup** — if uploading already-present content responds differently/faster, the timing leaks existence (the Dropbox / convergent-encryption side-channel). **Mitigation:** dedup invisibly server-side (always accept the write, never reveal "already existed"), or simply do not cross-tenant-dedup *unpublished* content (published content is public anyway).

**Published-tier serving — OPEN QUESTION (pending a Bahnhof S3/CDN spike).** The public published tier must serve clean URLs from object storage / CDN with **no conductor in the hot path**, while objects are hash-keyed and hashes must stay hidden. Two shapes:
- **Materialize** — at publish, copy bytes into a per-tenant published bucket keyed by clean path. Simple, fully static, no hash exposure; **loses serve-layer dedup** (bytes copied per tenant — bites hardest on large shared assets).
- **Edge-alias** — keep one deduped object per hash plus a per-tenant name→hash map at the edge (S3 routing / CDN rewrite / a reverse proxy with an internal URL→hash mapping). Keeps dedup, hashes stay hidden; **requires an edge rewrite capability**.

A **CDN sits in front of either origin** (orthogonal to the choice). **Billing on metered CDN egress** is a fair model where a paid CDN is used (per-tenant by construction; dissolves the "bill deduped storage-at-rest?" ambiguity).

**Bahnhof spike (done, 2026-06-24).** Bahnhof S3 is S3-API-compatible at **230 SEK/TB (10 TB minimum) with free/unlimited egress**, but it is **API-only** — no S3 website endpoint, no CDN, no edge compute ([bahnhof.cloud/s3](https://bahnhof.cloud/s3/)). So clean-URL public serving **cannot** be done by Bahnhof alone; a layer in front is required. Two viable shapes:
- **(a) Self-hosted reverse proxy** — Caddy on a Bahnhof VM mapping clean URL → S3 object (edge-alias, or serving materialized clean keys). Cheap, we already run Caddy, **free S3 egress**, full control; downside is a proxy in the hot path (mitigated by caching).
- **(b) Bring-your-own-CDN** — e.g. Cloudflare, whose edge Workers can do the URL→hash rewrite + TLS + global cache. Metered-egress billing applies (the CDN's egress, not Bahnhof's).

Free Bahnhof egress also makes **materialize** (duplicated bytes) cheaper than feared — storage cost only. **Leaning:** start with a self-hosted Caddy edge-alias proxy (keeps dedup, free egress, full control), offer a third-party CDN as a global-performance upgrade. **Final serving choice deferred to Phase 2** but the capability question is now answered.

## Why

1. **It falls out of the CAS choice.** Once the archive is content-addressable (ADR-048), a tenant is just refs over a shared object store — dedup, cheap multitenancy, and immortal content come along for free.
2. **Isolation without silos.** Internal hashes + conductor-as-trust-boundary give per-tenant privacy without a separate store per tenant.
3. **Fair, simple billing.** Metered egress is unambiguous where deduped storage-at-rest is not.

## Alternatives considered

- **Per-tenant isolated stores** — loses dedup and the shared-storage economics; retained only as a paid *exclusivity* upgrade.
- **Hash as a public capability** (serve any hash to anyone) — rejected; reintroduces the confirmation oracle and a leak surface.
- **Bill deduped storage-at-rest** — ambiguous (logical vs physical bytes); prefer metered egress.

## Consequences

- A **shared CAS backend** + a per-tenant **ref/manifest store**; the conductor enforces tenant-scoped resolution and holds storage credentials.
- **Cross-tenant GC / refcounting** — an object cannot be dropped while any tenant references it. "Once-published is permanent" largely moots this for the archive; the churny workbench may still need GC (open).
- **Security note:** cross-tenant dedup must be *invisible* (or published-only) to avoid the write-time timing oracle.
- **Billing** is on metered CDN egress, not storage.
- **Published-tier serving** (materialize vs edge-alias vs CDN) — Bahnhof capability spike DONE (S3 is API-only, free egress, no native website/CDN/edge → a front layer is required). Concrete serving choice deferred to Phase 2; current lean: a self-hosted Caddy edge-alias proxy, with a third-party CDN (e.g. Cloudflare Workers) as a global-performance upgrade.
- Builds on ADR-035 (workspace conductor), ADR-048 (content-addressable archive), ADR-049 (publish model), ADR-034 (assets).
