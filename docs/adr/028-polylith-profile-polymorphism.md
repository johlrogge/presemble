# ADR-028: Polylith profile-based build polymorphism

## Status
Accepted

## Context

The build pipeline reads site sources through a `SiteRepository` abstraction. Tests need fast, filesystem-free repos. Production needs the real filesystem. The project uses cargo-polylith for workspace management.

Rust's standard approach to polymorphism (traits + dynamic dispatch) adds runtime overhead and complexity. cargo-polylith provides build-time polymorphism via profiles: different profiles can wire different component implementations for the same interface.

## Decision

Use polylith's traitless structural polymorphism for the SiteRepository interface. Two components share the same public API but different implementations:

- `fs_site_repository`: reads from the filesystem (production)
- `mem_site_repository`: backed by HashMaps with a builder pattern (testing)

Both export `pub struct SiteRepository` with identical method signatures. The `live` profile wires `site_repository = components/fs_site_repository`. The `dev` profile wires `site_repository = components/mem_site_repository` and also includes `fs_site_repository` under its own name for its own tests.

No trait. No dynamic dispatch. The compiler only sees one implementation at a time. Consumers depend on `site_repository` (the interface name); the profile selects which component backs it.

The `builder().from_dir(path)` method on both implementations provides cross-implementation compatibility: production code and integration tests that need filesystem access use it, while unit tests use `builder().schema(...).content(...).build()`.

## Consequences

Unit tests run without filesystem I/O. No TempDir fixtures needed for component-level tests. The polylith profile system handles implementation selection at build time with zero runtime cost. The same pattern can be applied to the asset store interface (ADR-027) and future pluggable components.
