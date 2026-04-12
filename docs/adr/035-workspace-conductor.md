# ADR-035: Workspace-level conductor managing multiple sites
## Status
Proposed
## Decision

The conductor manages a workspace containing one or more sites, not a single site. All conductor commands accept a site parameter.

**One conductor per workspace.** Socket derived from workspace path. All clients connect to the same conductor.

**Site as a parameter:** Commands include a site identifier:
```clojure
(list-content :site "demo/")
(edit-slot :site "site/" :file "content/post/hello.md" :slot "title" :value "New")
```

**Site discovery.** The conductor discovers sites by the `schemas/` + `content/` + `templates/` directory convention or a marker file.

**LSP dispatch.** One LSP process classifies files by site membership (path prefix) and dispatches to the appropriate site context.

**nREPL alignment.** `.nrepl-port` at workspace root. One REPL session accesses all sites.

## Why

1. **Helix works at workspace level.** Multiple sites in one workspace should get LSP support without reconfiguration.
2. **nREPL convention.** `.nrepl-port` belongs at workspace root.
3. **Path resolution.** Site-relative paths need a site context to resolve.
4. **Multiplayer scaling.** Per-site conductors don't scale for hosted multi-site scenarios.

## Alternatives considered

- **One conductor per site (current)** — requires reconfiguration when switching sites, multiple processes.
- **Environment variable** — rejected in ADR-031.

## Consequences

- Conductor socket URL derived from workspace path
- All commands gain optional site parameter
- LSP discovers sites and dispatches per-file
- Supersedes ADR-020 and ADR-031's "one conductor per site"
