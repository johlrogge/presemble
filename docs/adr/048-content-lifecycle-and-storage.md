# ADR-048: Content lifecycle and content-addressable storage (workbench, archive, published)

## Status
Proposed

## Decision

Authored content moves through three stages, each a different *kind* of durability with its own storage and guarantees:

- **Workbench** — the active editing surface. Mutable, personal-ish ("mid-thought"). Presemble is **Google-Docs-like, not GitHub-like**: you suggest edits *on documents*, not patches against a directory tree. The workbench clones **all structured content** (schemas, markdown, templates) wholesale — it is inherently small — but only **references** assets, which are large and proxied on demand (see ADR-034). The workbench never blocks: a site can be materialized purely from schema with placeholders standing in for not-yet-real content (see ADR-049).
- **Archive** — where "done" content is recorded. The archive is a **content-addressable store (CAS)**: objects keyed by content hash, **append-only by construction** (nothing ever overwrites, so it cannot forget). It holds the *latest source*; "what is published when" is a separate query over it (ADR-049), and "what did the source look like in editing history" is a separate timeline again (versioning, below).
- **Published** — the public, static output. Served cheaply from object storage / CDN with no conductor in the hot path (serving + tenancy: ADR-050; publish model: ADR-049).

**Two orthogonal timelines.** These are independent, and keeping them separate is the keystone:
1. **Publish-time** — a *dimension (edge) of a node*. Publishing is a **temporal query over the archive** ("the nodes relevant at time T"); the schedule is *derived* by querying when nodes appear (ADR-049).
2. **Source-version history** — a separate timeline answering "what did the *source* look like at a point in editing history" (ADR-033). This is what git provides.

**Storage is pluggable, and git is optional.** Because the two timelines are orthogonal, scheduled/time-travel *publish* needs only the publish-time dimension, never source history — so git is not required for the core model. Deployments choose a backend:
- Hosted Presemble — **S3-backed CAS** as workbench and archive.
- Agical — **git for long-term** storage + **S3 for short/mid-term**.
- Minimal — a single archive overwritten each save (optionally split into stage + repository).

**Git's role is narrow and non-core:** backup, familiarity, and killing lock-in aversion ("keep your whole site in git wherever you want" — a portability escape hatch). It is **not** the collaboration mechanism. Guiding principle: *we should know **why** we require git* — mandate it only where it earns a specific guarantee.

## Why

1. **Three durabilities, named.** Workbench / archive / published each fail differently if lost; modelling them as distinct stores with distinct guarantees is the insight, not an implementation detail.
2. **CAS gives the hard properties for free.** Append-only, "remembers everything," and cross-content dedup all fall out of content-addressing — and the same store underpins multi-tenancy (ADR-050). Git is merely *one* implementation of a CAS+DAG; an S3-CAS is another; pushing to a GitHub upstream is a third.
3. **Orthogonal timelines dissolve the git question.** Publish-time-as-query and source-version-history answer different questions, so requiring git for scheduling was a category error.
4. **Google-Docs-not-GitHub** is a brand keystone: doc-level suggestions, enabled by the live workbench, not branch/merge/PRs.

## Alternatives considered

- **Content lives on the conductor box** (Phase-1 reality) — wiped on reprovision; not durable. Rejected as the model.
- **Mandate git as the core store** — rejected; its guarantees (source history, portability) don't justify forcing it on every deployment when an S3-CAS handles the core.
- **S3 as a raw key/value source of truth** (no CAS semantics) — loses append-only / dedup / remembers-everything.

## Consequences

- A content-addressable **archive abstraction** with pluggable backends (S3-CAS, git, overwrite).
- The workbench materializes structured content wholesale; assets stay referenced (ADR-034).
- Publish-time becomes a node dimension; scheduling is a query, not a subsystem (ADR-049).
- Optional git integration offered as a backup/portability feature.
- Builds on ADR-035 (workspace conductor over multiple sites), ADR-033 (per-document versioning = the source-version timeline), ADR-026 (site graph). Multi-tenancy over the shared CAS is ADR-050.
