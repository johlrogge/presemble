---
name: conventional-commits
description: Conventional Commits specification. Defines commit message format, types, and rules.
---

# Conventional Commits

All commits follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

## Format

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

## Types

| Type | Purpose |
|------|---------|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `refactor` | Code restructuring without behavior change |
| `docs` | Documentation only |
| `test` | Adding or updating tests |
| `ci` | CI/CD pipeline changes |
| `chore` | Maintenance (deps, versions, tooling) |
| `perf` | Performance improvement |
| `build` | Build system changes |

## Scopes

Scopes are project-specific. Check the project's conventional-commits skill or CLAUDE.md
for defined scopes. Omit scope for changes spanning many areas.

## Breaking Changes

Mark with `!` after the type/scope:

    feat(api)!: change response format

Or use a `BREAKING CHANGE:` footer.

## Rules

1. **Imperative mood**: "add feature" not "added feature"
2. **Lowercase** description start
3. **No period** at end of description
4. **Body** explains *why*, not *what*
5. **72 characters** max for the first line
6. **Single scope** per commit — split multi-scope work into multiple commits
