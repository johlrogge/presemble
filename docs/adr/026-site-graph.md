# ADR-026: Unified SiteGraph as single source of truth

## Status
Accepted

## Context

The build pipeline assembles site data through three separate code paths: item pages go through `build_content_page`, collection pages through an inline loop, and the homepage through hardcoded index handling. Reference resolution runs in two places (items and index separately). Collection pages skip resolution entirely.

This fragmentation means content flows through different pipelines depending on its kind, reference resolution is incomplete (collection content links are not resolved), and the "what data is available to a template" question requires tracing through 200+ lines of build code.

## Decision

Introduce a `SiteGraph` as the single source of truth for all site data. Every piece of content — items, collection indices, and the site index — goes through one build function, is registered in one data structure, and is resolved by one reference pass.

The `SiteGraph` holds `HashMap<UrlPath, SiteEntry>` where `SiteEntry` carries `EntryKind` (Item, Collection, SiteIndex), `SchemaStem`, the page's DataGraph, template/content/schema paths, and dependency set.

Build becomes three phases: (1) build all entries, (2) resolve all references once, (3) render all entries. No special cases by content kind.

`SchemaStem` and `UrlPath` newtypes eliminate stringly-typed HashMap keys, following the existing `SlotName` pattern (ADR-017).

## Consequences

One code path for all content kinds. Reference resolution covers items, collections, and the site index uniformly. The SiteGraph becomes the queryable data structure for the conductor (ADR-020), REPL, and browser editing (M5).

`BuiltPage`, `CollectedPage`, `PageAddress`, and `PageBuildResult` are superseded by `SiteEntry`. `built_pages: HashMap<String, Vec<BuiltPage>>` is superseded by `SiteGraph`.
