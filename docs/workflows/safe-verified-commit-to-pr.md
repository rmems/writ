# Safe Verified Commit → PR

Portable worker contract for any coding agent. Runs **after** [Safe Issue → Verified Commit](safe-issue-verified-commit.md). Opens or updates a human-reviewable pull request. Never merges.

Safety rules live in [`AGENTS.md`](../../AGENTS.md), [`SKILL.md`](../../SKILL.md), and [`REVIEW.md`](../../REVIEW.md). This file does not relax them.

Contract: Issue → PR [#8](https://github.com/rmems/writ/issues/8) / Linear [RM-123](https://linear.app/rpd-34/issue/RM-123/issue-pr-workflow-never-auto-merge). Isolation prerequisite [#6](https://github.com/rmems/writ/issues/6). Interactive monitoring, when needed after handoff, belongs to the installed companion `babysit-pr` skill rather than this workflow.

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| Verified push | yes | Branch already pushed; local gates already passed or residuals already reported |
| `issue` | yes | GitHub issue this PR implements |
| `owner` / `repo` | no | Resolve from `git remote` if omitted |
| `dry_run` | no | Describe the PR you would open. Do not create or update it |

One issue → one PR unless the issue explicitly groups work.

## Hard stops

- No verified push yet — run the commit workflow first.
- Shared `main`/`master` checkout — work only in the job worktree/branch.
- Owner outside the configured allowlist unless the operator named this job.
- Any merge command, merge API, auto-merge, or merge-queue enablement.
- Bare `git push --force` / `git push -f`.
- Opening a no-op “kick CI” PR.

## Stages

### 8. Open or update the PR

GitHub **MCP first**. Shell `gh` only if MCP is unavailable.

1. Confirm you are still on the job branch inside the job worktree (`git rev-parse --show-toplevel`, `git branch --show-current`).
2. `git fetch`. Re-read `HEAD` after any last rebase/push: `git rev-parse HEAD`.
3. Base = repository default branch, or the stack parent if this is a stacked PR. Process stacks **bottom-up**.
4. If a PR already exists for this branch, update it. Do not open a second PR for the same issue.
5. Otherwise create the PR:

   - title reflects the issue
   - body links the GitHub issue (`Fixes #<n>` only when the issue is fully done; otherwise `Refs #<n>`)
   - if a Linear `RM-*` twin already exists, body also includes that issue’s URL or id; validate it matches the existing twin (do not invent one)
   - no merge flags
   - do not enable auto-merge

Suggested body:

```markdown
## Summary

<what changed>

## Issue
Fixes #<n>   <!-- or Refs #<n> if partial -->
Linear: RM-<n>  <!-- omit this line if no twin exists -->

## Test plan
- [ ] Local gates from README.md
- [ ] CI on PR

## Notes for review
- Known residuals: ...
```

### Partial / blocked

- Code exists but the issue is incomplete: open a **draft** or clearly partial PR with a Remaining section.
- Zero commits: do **not** open an empty PR. Comment residuals on the issue instead.

### 9. Handoff

After the create/update call, record at least:

| Field | Rule |
| --- | --- |
| `repo` | `owner/repo` |
| `issue` | GitHub issue number |
| `linear` | existing `RM-*` id/URL, or explicit none |
| `pr` | PR number |
| `url` | PR URL |
| `branch` | job branch |
| `head_sha` | `git rev-parse HEAD` **after** the last push |
| `status` | open / draft / blocked |
| `notes` | residuals |

Before treating the PR as handed off, validate **all** of the following via GitHub **MCP first** (do not invoke merge commands or APIs):

1. The PR is still **open** or **draft**. Query this state through GitHub MCP.
2. Auto-merge is **disabled** and the PR is **not** in the merge queue. Query both through GitHub MCP.
3. The PR’s GitHub `Fixes` or `Refs` reference **targets the input issue** and uses the required completion keyword (`Fixes` only when the issue is fully done; otherwise `Refs`). Absent, wrong number, or the wrong keyword fails.
4. If a Linear twin exists, the PR body contains that `RM-*` link and it matches the twin.
5. Query the remote source branch tip and the PR head SHA via GitHub MCP. Both must equal the recorded `head_sha` (the `git rev-parse HEAD` value after the last successful push).

Abort the handoff and report a residual (do **not** merge, do not claim handoff complete) if any check fails — including a closed/merged PR, auto-merge or merge-queue enabled, a GitHub issue reference that is missing or points elsewhere, or a remote SHA that differs from `head_sha`.

Comment on the GitHub issue with PR URL, the **validated** pushed commit SHA, residuals, and agent name. Mention the Linear twin only if it already exists.

If interactive monitoring is needed after handoff, invoke the installed companion `babysit-pr` skill. This workflow itself only creates or updates the PR and hands it off.

### 10. Never merge

Success is **PR opened (or updated) and handoff ready**, not “landed on main.”

Never:

- `gh pr merge` (including `--auto`)
- GraphQL `mergePullRequest`
- REST `PUT /repos/.../merge`
- merge-queue / auto-merge enablement
- claiming the agent merged the PR

If the PR is already merged, report that a human merged it and stop.

## Done when

An open (or draft) PR has auto-merge and merge-queue disabled, `Fixes`/`Refs` the **input** GitHub issue with the required keyword, includes a matching Linear `RM-*` link when a twin exists, remote source-branch tip and PR head both equal the recorded `head_sha`, the issue comment includes URL + validated SHA + agent, and no merge path was invoked.
