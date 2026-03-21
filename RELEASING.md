# Feature & Release Workflow

Presemble uses git flow with a multi-agent workflow for both feature development and releases.

## Branch Model

- `master` — released code only, always tagged
- `develop` — integration branch, features merge here
- `feature/*` — individual features, branch from develop
- `release/*` — release prep, branch from develop
- `hotfix/*` — urgent fixes, branch from master

## Versioning (semver)

- `feat` commits → minor bump (0.4.0 → 0.5.0)
- `fix` / `chore` commits → patch bump (0.5.0 → 0.5.1)
- Breaking change (`!`) → major bump (0.5.0 → 1.0.0)

## Feature Workflow

New features go through a design-implement-review loop before release.

1. **product-owner** — prioritize and scope the feature on the roadmap
   > "Should we add X? Where does it fit in the roadmap?"

2. **devops** — start the feature branch
   > "Start feature <name>"

3. **architect** — design the feature; produce a task breakdown for minions
   > "Design the implementation for <feature>"
   The architect produces clear, scoped tasks — one per minion.

4. **code-minions** — implement in parallel, one task each
   > Spawn multiple minions simultaneously with different task descriptions.
   Each minion: writes a failing test → implements → runs checks → reports back.

5. **architect** — review all minion output
   > "Review the changes from <minion tasks>"
   - If approved: says COMMIT with a suggested message
   - If changes needed: dispatches minions again with specific fix instructions
   - Loop until approved

6. **commit** — commit the approved changes
   > Invoked by the orchestrator when architect approves

7. **devops** — finish the feature branch
   > "Finish feature <name>"

## Release Checklist

Run these agents in order before cutting a release:

1. **architect** — review all changes since last release
   > "Review changes since last release"

2. **product-owner** — confirm the release delivers intended value
   > "Review the planned 0.x.0 release"

3. **documenter** — update README files to reflect the release
   > "Update docs for release 0.x.0"

4. **devops** — start and finish the release branch
   > "Start release 0.x.0" → confirm → "Finish release 0.x.0"

5. **Human** — push to remote
   ```
   git push origin master develop --tags
   ```

## Hotfix Checklist

1. **devops** — start hotfix
2. **commit** — commit the fix
3. **devops** — finish hotfix (confirm before calling)
4. **Human** — push

## Notes

- Agents never push — that always stays with the human
- Always confirm with devops before finishing a release or hotfix
- Multiple code-minions can run in parallel on different tasks within the same feature
- The commit agent reads `.claude/skills/conventional-commits/SKILL.md` for format
- The architect never writes code — it designs and reviews only
