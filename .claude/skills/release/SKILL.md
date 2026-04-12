---
name: release
description: Run the full release checklist from RELEASING.md. Spawns agents in sequence.
user-invocable: true
---

# Release Checklist

Read `RELEASING.md` and execute the Release Checklist section step by step.

## Steps (execute in order, do not skip)

1. **Hygiene gate** — run `mcp__rust-codebase__hygiene_report` (tests + clippy).
   DO NOT PROCEED if this fails.

2. **Architect review** — spawn the architect agent:
   "Review all changes since the last release tag. Check for design issues,
   missing tests, and code quality. Report findings."

3. **Product-owner review** — spawn the product-owner agent:
   "Review the planned release. Read VISION.md and ROADMAP.md. Confirm
   the release delivers intended value. Report any concerns."

4. **Documenter** — spawn the documenter agent:
   "Update docs for this release. Read RELEASING.md for the full checklist.
   Update README, site feature pages, user guide. Review the homepage."

5. **Update ROADMAP.md** — mark completed items, clean up milestones.

6. **ADR housekeeping** — spawn the architect agent:
   "Review all ADRs for relevance and status. Accept implemented ADRs
   still marked Proposed. Mark superseded ADRs. Create new ADRs for
   significant architectural decisions made since the last release."

7. **Version bump** — spawn code-minion to bump version in all Cargo.toml files.

8. **Release-manager** — spawn the release-manager agent to start and finish
   the release branch. Provide the tag message summarizing what shipped.

9. **Human pushes** — remind the user to push: `git push origin master develop --tags`

Each step must complete successfully before proceeding to the next.
If any step fails, stop and report the issue.
