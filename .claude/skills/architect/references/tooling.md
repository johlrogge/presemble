# Rust Development Tooling: bacon and just

## bacon - Background Rust Code Checker

bacon runs cargo commands in the background and shows results immediately.

### Installation
```bash
cargo install bacon
```

### Basic Usage
```bash
bacon              # Run default job (usually check)
bacon test         # Run tests
bacon clippy       # Run clippy
bacon doc          # Build docs
```

### Configuration: bacon.toml

Place in project root:
```toml
# bacon.toml
[jobs.check]
command = ["cargo", "check", "--color=always"]
need_stdout = false

[jobs.clippy]
command = ["cargo", "clippy", "--color=always"]
need_stdout = false

[jobs.test]
command = ["cargo", "test", "--color=always"]
need_stdout = true
watch = ["tests"]

[jobs.run]
command = ["cargo", "run"]
need_stdout = true

[jobs.doc]
command = ["cargo", "doc", "--no-deps", "--open"]
need_stdout = true
on_success = "open"

# Custom job
[jobs.coverage]
command = ["cargo", "tarpaulin", "--out", "Html"]
need_stdout = true
```

### Advanced Patterns

#### Workspace-Specific Checks
```toml
[jobs.check-workspace]
command = ["cargo", "check", "--workspace", "--all-features"]

[jobs.check-package]
command = ["cargo", "check", "-p", "my-package"]
```

#### Different Profiles
```toml
[jobs.check-release]
command = ["cargo", "check", "--release"]

[jobs.clippy-strict]
command = ["cargo", "clippy", "--", "-W", "clippy::pedantic"]
```

#### Watch Specific Paths
```toml
[jobs.test-integration]
command = ["cargo", "test", "--test", "integration"]
watch = ["tests", "src"]
```

### Keyboard Shortcuts
- `c` - cargo check
- `t` - cargo test
- `r` - cargo run
- `d` - cargo doc
- `q` - quit
- `h` - help

### Tips
1. Keep bacon running in a terminal split
2. Use keybindings to switch between jobs
3. Combine with just for complex workflows
4. Configure per-workspace for monorepos

## just - Command Runner

just is a command runner like make, but simpler and cross-platform.

### Installation
```bash
cargo install just
```

### Basic justfile

Place `justfile` (or `Justfile`) in project root:
```just
# List all recipes
default:
    @just --list

# Run tests
test:
    cargo test

# Run tests with coverage
coverage:
    cargo tarpaulin --out Html

# Format code
fmt:
    cargo fmt

# Run clippy
lint:
    cargo clippy -- -D warnings

# Build release
build:
    cargo build --release
```

### Dependencies

Recipes can depend on other recipes:
```just
# Run linter before tests
test: lint
    cargo test

# Format and lint before building
build: fmt lint
    cargo build --release

# Chain multiple dependencies
ci: fmt lint test
    @echo "CI checks passed!"
```

### Recipe Parameters

```just
# Run specific test
test-one TEST:
    cargo test {{TEST}}

# Run package tests
test-package PACKAGE:
    cargo test -p {{PACKAGE}}

# Build with specific features
build-features FEATURES:
    cargo build --features {{FEATURES}}

# Optional parameters with defaults
run profile="dev":
    cargo run --profile {{profile}}
```

### Groups and Attributes

#### Mark recipes as private
```just
# Public recipe (shown in --list)
build:
    cargo build

# Private recipe (hidden from --list)
[private]
_internal-helper:
    echo "Internal use only"

# Use private recipe
deploy: _internal-helper
    cargo build --release
```

#### Group related recipes
```just
# Documentation group
[group: 'docs']
doc:
    cargo doc --no-deps

[group: 'docs']
doc-open:
    cargo doc --no-deps --open

# Testing group
[group: 'test']
test:
    cargo test

[group: 'test']
test-integration:
    cargo test --test integration

# Development group
[group: 'dev']
dev:
    bacon

[group: 'dev']
fmt:
    cargo fmt
```

Now `just --list --groups` shows organized recipes.

### Integration Patterns

#### Complete Development Workflow
```just
# Default: show all commands
default:
    @just --list

# === Development ===
[group: 'dev']
dev:
    bacon check

[group: 'dev']
run *ARGS:
    cargo run -- {{ARGS}}

[group: 'dev']
fmt:
    cargo fmt --all

# === Testing ===
[group: 'test']
test:
    cargo test

[group: 'test']
test-coverage:
    cargo tarpaulin --out Html --output-dir coverage

[group: 'test']
test-watch:
    bacon test

# === Quality ===
[group: 'qa']
lint:
    cargo clippy --all-targets --all-features -- -D warnings

[group: 'qa']
check-all: fmt lint test
    @echo "All checks passed!"

# === Building ===
[group: 'build']
build:
    cargo build

[group: 'build']
build-release:
    cargo build --release --locked

# === Documentation ===
[group: 'docs']
doc:
    cargo doc --no-deps

[group: 'docs']
doc-open:
    cargo doc --no-deps --open

# === CI/CD ===
[group: 'ci']
ci: check-all
    @echo "CI checks passed!"
```

### Best Practices

1. **Use groups for organization**
2. **Mark internal recipes as private**
3. **Use dependencies for workflows**
4. **Document recipes with comments**
5. **Provide defaults for parameters**
6. **Use shell scripts for complexity**
7. **Combine with bacon for live feedback**
