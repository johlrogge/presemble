# CLAUDE.md

## Project: Presemble

Presemble is a site publisher tool focused on editorial collaboration and semantic content safety.

Core qualities:
- **Editorial**: comment/suggest/track-changes workflow in the browser (structured, not WYSIWYG)
- **Semantic**: content schemas for compile-time safety — no runtime surprises from missing template data
- **Live edit mode**: serve locally with in-browser editing (like `hugo serve` + editorial UI)
- **Publish scheduling**: strong scheduling foundation with time-travel preview (see any point in time)

The name: *pre-* (the upstream collaborative phase) + *semble* (ensemble — the gathering).

## Workflow

For any non-trivial code change, follow the multi-agent workflow:

1. **architect** — reviews design and produces a task list; never writes code
2. **code-minion** — implements based on architect's task list
3. **You (orchestrator)** — delegate; do NOT implement directly

When you reach for Edit or Write on source files: stop. Spawn a code-minion instead.
Small, isolated, obviously-safe changes (config values, typos, comments) may be done directly.
Everything else goes through the workflow.

## Environment

This project runs in an immutable Nix environment managed by devenv.
**Do NOT** run `pip install`, `npm install -g`, `cargo install`, `brew install`,
`apt-get install`, or any other imperative package manager.
If a tool or package is missing, add it to `devenv.nix` and re-enter the shell.
All tools, packages, hooks, and services are declared in `devenv.nix`.

## Conventions

**Build and test:**
- `cargo check` — type-check the workspace
- `cargo test` — run all tests
- `cargo clippy` — lint
- `cargo run -p publisher -- <site-dir>` — run the publisher CLI

**Code style:**
- Idiomatic Rust. Type-driven design: make illegal states unrepresentable.
- No stringly-typed fields where newtypes would serve.
- CUPID properties preferred (see `.claude/skills/architect/SKILL.md`).
- Commit messages follow Conventional Commits. Delegate all commits to the commit agent.

## Architecture

Rust monorepo using cargo-polylith.

**Components** (shared library code):
- `schema` — grammar types and schema parser
- `content` — document parser and validator

**Bases** (runtime entry points):
- `publisher_cli` — CLI wiring for the build command

**Projects** (deployable binaries):
- `publisher` — `presemble build <site-dir>` CLI
- `content_management` — long-running multiplayer editing service (future)

ADRs live in `docs/adr/`. Read relevant ADRs before making significant design decisions.

## Agents

This project uses the shared metadev multi-agent workflow. Run `/init` on first session.

| Agent | Role |
|---|---|
| `architect` | Reviews design, proposes task lists. Never writes code. |
| `code-minion` | Implements based on architect's instructions. |
| `commit` | Writes conventional commit messages. |
| `metadev` | Onboards the project, installs skills, audits CLAUDE.md. |
| `product-owner` | Reviews vision and roadmap, advises on priorities. |
