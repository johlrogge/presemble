# Polylith Architecture in Rust

Polylith is a monorepo architecture for maximum component reuse across multiple deployable artifacts.
Originally from Clojure; this document describes the Rust/Cargo mapping.

## Core Concepts

### Component
A Cargo library crate under `components/<name>/`. Its **interface is the public surface of `lib.rs`** — the `pub` items re-exported from private submodules. Nothing beyond the crate boundary is accessible to other bricks.

```
components/user/
  Cargo.toml
  src/
    lib.rs        ← ONLY pub use re-exports
    user.rs       ← private implementation
```

```rust
// src/lib.rs — the interface: what other bricks can see
mod user;
pub use user::create_user;
pub use user::get_user;
pub use user::User;
```

```rust
// src/user.rs — the implementation: private
pub fn create_user(email: &str) -> Result<User> { ... }
pub fn get_user(id: UserId) -> Result<User> { ... }
```

**No traits required.** The interface is plain named functions, as Joakim Tengstrand (polylith inventor) intends. The crate's pub surface IS the interface contract.

### Interface metadata
Cargo-polylith uses an explicit metadata declaration in each component's `Cargo.toml`:

```toml
[package.metadata.polylith]
interface = "user"
```

New components get this automatically (`component new` defaults `interface` to the crate name).
Existing components can be updated with `component update <name> [--interface <NAME>]`.
`cargo polylith check` warns for any component missing this declaration.

### Base
A Cargo **library crate** under `bases/<name>/`. Exposes a runtime API (HTTP server, CLI, IPC, gRPC …) as ordinary Rust functions (`run()`, `serve()`, `create_sockets()`). Bases wire components together but do not hardcode which implementations are used.

**Bases must NOT have `src/main.rs`.** If a base were a binary, two bases could never share one process.

```
bases/http_api/
  Cargo.toml
  src/
    lib.rs        ← pub fn serve(...) / run(...) / create_sockets(...)
    handler.rs    ← private implementation
```

### Project
A Cargo workspace root under `projects/<name>/`. Owns `src/main.rs` and calls the bases' runtime-API functions. Projects CAN depend on components directly (valid polylith). Projects MUST depend on at least one base.

```
projects/production/
  Cargo.toml      ← project workspace root + [package] + [[bin]]
  src/main.rs     ← entry point: calls base fns, wires components
```

A project has **no domain logic** — all logic lives in bricks. The `main.rs` is a thin wiring point.

### Development Workspace
The repo root `Cargo.toml`. Lists ALL components and bases as members. Used for `cargo check`, IDE support, and day-to-day development. Not a deployment artifact.

## Directory Layout

```
repo-root/
  Cargo.toml              ← development workspace: members = all components + bases
  .cargo/config.toml      ← [build] target-dir = "target"  (shared across all workspaces)
  components/             ← library crates (NOT a workspace root)
    user/
      Cargo.toml
      src/lib.rs          ← pub re-exports only
      src/user.rs         ← private impl
    user_inmemory/        ← alternative implementation (same crate name "user")
      Cargo.toml
      src/lib.rs
      src/user.rs
  bases/                  ← runtime-API library crates (lib only, no main.rs)
    http_api/
      Cargo.toml
      src/lib.rs          ← pub fn serve(...)
  projects/
    production/
      Cargo.toml          ← project workspace root + [package] + [[bin]]
      src/main.rs         ← entry point: calls base fns
    test-env/
      Cargo.toml          ← different implementation choices
      src/main.rs
```

## `cargo polylith check` Violations

**Hard errors** (non-zero exit, must fix):

| Violation | Meaning | Fix |
|-----------|---------|-----|
| `dep-key-mismatch` | Path dep key doesn't match `package.name` and no `package` alias | Add `package = "<real-crate-name>"` to the dep entry |
| `profile_impl_path_not_found` | Profile entry references a component path that doesn't exist | Correct path in `.polylith/profiles.toml` or create the component |
| `profile_impl_not_a_component` | Profile entry points to a path without `[package.metadata.polylith]` | Add metadata or point profile at a proper component |

**Warnings** (exit 0, flag for attention):

| Violation | Meaning | Fix |
|-----------|---------|-----|
| `hardwired_dep` | Component/base uses direct `path = "..."` instead of `workspace = true` — bypasses swap | Move dep to `[workspace.dependencies]`, reference as `{ workspace = true }` |
| `WildcardReExport` | `pub use foo::*` in lib.rs | Use named re-exports |
| `OrphanComponent` | Component not used by any project | Wire it or remove it |
| `ProjectFeatureDrift` | Project dep has fewer features than root workspace dep | Add missing features |
| `ProjectVersionDrift` | Project dep version differs from workspace | Align versions |
| `MissingInterface` | Component missing `[package.metadata.polylith] interface` | Add metadata |
| `AmbiguousInterface`, `DuplicateName`, `ProjectMissingBase`, `NotInRootWorkspace`, `BaseHasMainRs` | Structural issues | Fix per violation name |

Notes:
- Projects depending directly on components is valid and not flagged.
- Bases depending on other bases is valid and not flagged.

## Swappable Implementations

Components share an interface name (via `[package.metadata.polylith] interface`) but can have different package names. Projects select which implementation is active by declaring a path dependency aliased to the interface name:

```toml
# projects/prod/Cargo.toml — use the real implementation
[dependencies]
user = { path = "../../components/user" }
# package omitted: the crate is already named "user"

# projects/bdd/Cargo.toml — use the stub
[dependencies]
user = { path = "../../components/user_stub", package = "user-stub" }
# package required: the stub crate is named "user-stub", not "user"
```

All code calls `use user::UserService;` identically in both projects. The compiler enforces that both components expose the same public API — mismatched functions are compile errors. No traits needed.

### How Projects Select Implementations

Bases declare component dependencies as **workspace-inherited deps** (`workspace = true`). The project workspace defines which path each name resolves to:

```toml
# bases/http_api/Cargo.toml
[dependencies]
user = { workspace = true }   # resolves to whatever the project workspace says
```

```toml
# projects/production/Cargo.toml
[workspace]
members = ["../../bases/http_api"]

[workspace.dependencies]
user = { path = "../../components/user" }           # real implementation
```

```toml
# projects/test-env/Cargo.toml
[workspace]
members = ["../../bases/http_api"]

[workspace.dependencies]
user = { path = "../../components/user-stub", package = "user-stub" }  # stub
```

The `cargo-polylith` tool generates and manages these project workspace files. Build a specific project with:
```bash
cargo build --manifest-path projects/production/Cargo.toml
```

## Profiles

A **profile** is a named set of interface-to-implementation mappings stored in `.polylith/profiles.toml`. Profiles let you switch the full set of component implementations for a project with one command, rather than editing `Cargo.toml` files manually.

```toml
# .polylith/profiles.toml
[profiles.prod]
user = "components/user"
storage = "components/storage-postgres"

[profiles.bdd]
user = "components/user-stub"
storage = "components/storage-memory"
```

Profile commands:

```bash
cargo polylith profile list [--json]                      # list defined profiles
cargo polylith profile build <name> [--no-build]          # activate profile (rewrites Cargo.tomls); --no-build patches without building
cargo polylith profile add <interface> \
  --impl <path> --profile <name>                          # add/update one mapping
cargo polylith check --profile <name>                     # validate as if profile were active
```

`profile build` resolves each mapping and rewrites `[dependencies]` in the relevant `Cargo.toml` files. `check --profile` adds profile-specific violations (`profile_impl_path_not_found`, `profile_impl_not_a_component`) on top of standard checks.

## Project Workspace Structure

A project workspace lists bases as `[workspace].members`. Components are not workspace members (they live outside the project directory) — they come in as workspace-level path dependencies resolved transitively.

```toml
# projects/production/Cargo.toml
[workspace]
members = [
  ".",
  "../../bases/http_api",
  "../../bases/cli",
]

[package]
name = "production"
version = "0.1.0"

[[bin]]
name = "production"
path = "src/main.rs"

[dependencies]
http_api = { path = "../../bases/http_api" }
cli      = { path = "../../bases/cli" }

[workspace.dependencies]
user     = { path = "../../components/user" }
library  = { path = "../../components/library_service" }
```

All bases in the same project share the same `[workspace.dependencies]` pool — one implementation choice per interface name per project.

## Shared Target Directory

Without configuration, each project workspace would have its own `target/` directory and recompile everything. Solve this with a single `.cargo/config.toml` at the repo root:

```toml
[build]
target-dir = "target"
```

Cargo hashes artifacts by (crate + features + profile + target triple), so identical builds across projects share compiled artifacts.

## The Development Workspace

The repo root workspace lists all components and bases:

```toml
# Cargo.toml (repo root)
[workspace]
members = [
  "components/*",
  "bases/*",
]
```

This gives full IDE support and lets you run `cargo check` across the entire codebase. Component dependencies here can use direct path deps (no `workspace = true` needed since the dev workspace isn't a project).

## What the cargo-polylith Tool Does

Managing this structure by hand is tedious. `cargo-polylith` handles:

- **Scaffolding**: `cargo polylith component new <name> [--interface <NAME>]` — always creates interface metadata (defaults to crate name)
- **Interface update**: `cargo polylith component update <name> [--interface <NAME>]` — set/replace interface on an existing component
- **Base scaffolding**: `cargo polylith base new <name>` creates `bases/<name>/` with `lib.rs` (pub fn run() skeleton) and Cargo.toml
- **Project management**: `cargo polylith project new <name>` generates the project workspace manifest
- **Overview**: `cargo polylith deps` shows which components are used by which bases and projects
- **Interface checking**: `cargo polylith check` verifies structural correctness and reports violations
- **Interactive editor**: `cargo polylith edit` — TUI to toggle project/component connections, set interface names ('i' key), write all staged changes to disk ('w')
