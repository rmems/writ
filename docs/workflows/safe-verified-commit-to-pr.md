# Safe Verified Commit → PR

Portable worker contract for any coding agent. Runs **after** [Safe Issue → Verified Commit](safe-issue-verified-commit.md). Opens or updates a human-reviewable pull request. Does not merge the pull request.

Safety rules live in [`AGENTS.md`](../../AGENTS.md), [`SKILL.md`](../../SKILL.md), and [`REVIEW.md`](../../REVIEW.md). This file does not relax them.

Contract: Issue → PR [#8](https://github.com/rmems/writ/issues/8) / Linear [RM-123](https://linear.app/rpd-34/issue/RM-123/issue-pr-workflow-never-auto-merge). Isolation prerequisite [#6](https://github.com/rmems/writ/issues/6). Interactive monitoring, when needed after handoff, belongs to the installed companion `babysit-pr` skill rather than this workflow.

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| Verified push | yes | Branch already pushed; report focused-check or readiness status truthfully |
| `task` | yes | Linear `RM-*` (maintainers) and/or GitHub issue. Do not create a GitHub issue twin for Linear-only work. |
| `owner` / `repo` | no | Resolve from `git remote` if omitted |
| `dry_run` | no | Describe the PR you would open. Do not create or update it |

One task → one PR unless the task explicitly groups work.

## Hard stops

- No verified push yet — run the commit workflow first.
- Shared `main`/`master` checkout — work only in the job worktree/branch.
- Owner outside the configured allowlist unless the operator named this job.
- Any GitHub PR merge command, merge API, auto-merge, or merge-queue enablement. Local feature-branch integration is not this stop.
- Bare `git push --force` / `git push -f`.
- Opening a no-op “kick CI” PR.
- Claiming merge-ready or release-ready when integrated validation was skipped or failed.

## Stages

### 8. Open or update the PR

GitHub **MCP first**. Shell `gh` only if MCP is unavailable.

1. Confirm you are still on the job branch inside the job worktree (`git rev-parse --show-toplevel`, `git branch --show-current`).
2. `git fetch`. Re-read `HEAD` after any last rebase/push: `git rev-parse HEAD`.
3. Base = repository default branch, or the stack parent if this is a stacked PR. Process stacks **bottom-up**.
4. If a PR already exists for this branch, update it. Do not open a second PR for the same task.
5. Otherwise create the PR:

   - title reflects the task
   - body links Linear `RM-*` for maintainer work, and `Fixes #<n>` / `Refs #<n>` only when a GitHub issue already exists (`Fixes` only when that issue is fully done)
   - do not invent a GitHub issue or Linear task so the body has a number
   - no merge flags
   - do not enable auto-merge
   - use **draft** when this is a checkpoint rather than a merge-ready claim

Suggested body:

```markdown
## Summary

<what changed>

## Task
Linear: RM-<n>   <!-- omit if this is GitHub-only work -->
Refs #<n>        <!-- omit if no GitHub issue exists; use Fixes only when that issue is done -->

## Status
checkpoint / merge-ready candidate

## Test plan
- [ ] Focused checks covering this change (required for checkpoints; report residuals)
- [ ] Integrated native gates from README.md (required before a merge-ready claim)
- [ ] CI on PR

## Notes for review
- Known residuals: ...
```

### Partial / blocked

- Code exists but the task is incomplete: open a **draft** or clearly partial PR with a Remaining section. That is a checkpoint, not merge-ready.
- Zero commits: do **not** open an empty PR. Comment residuals on the Linear task or existing GitHub issue instead.

### 9. Handoff

After the create/update call, record at least:

| Field | Rule |
| --- | --- |
| `repo` | `owner/repo` |
| `task` | Linear `RM-*` and/or GitHub issue number; explicit none for the unused tracker |
| `pr` | PR number |
| `url` | PR URL |
| `branch` | job branch |
| `head_sha` | `git rev-parse HEAD` **after** the last push |
| `status` | draft checkpoint / open / blocked / merge-ready candidate |
| `notes` | residuals and which checks actually ran |

Before treating the PR as handed off, validate **all** of the following via GitHub **MCP first** (do not invoke merge commands or APIs):

1. The PR is still **open** or **draft**. Query this state through GitHub MCP.
2. Auto-merge is **disabled** and the PR is **not** in the merge queue. Query both through GitHub MCP.
3. The PR body links the **input** Linear task and/or existing GitHub issue. Do not fail handoff because a GitHub issue twin is absent. If a GitHub issue was an input, `Fixes`/`Refs` must target that issue (`Fixes` only when fully done).
4. Query the remote source branch tip and the PR head SHA via GitHub MCP. Both must equal the recorded `head_sha` (the `git rev-parse HEAD` value after the last successful push).
5. Status is truthful: a draft/checkpoint must not be labeled merge-ready.

Abort the handoff and report a residual (do **not** merge, do not claim handoff complete) if any check fails — including a closed/merged PR, auto-merge or merge-queue enabled, a missing Linear/GitHub link that was an actual input, or a remote SHA that differs from `head_sha`.

Comment on the Linear task (maintainers) and, when a GitHub issue already exists, on that issue (MCP first) with PR URL, the **validated** pushed commit SHA, checkpoint vs merge-ready status, residuals, and agent name using the [reply attribution](../../SKILL.md#reply-attribution) templates. Do not open a GitHub issue just to host the comment.

If interactive monitoring is needed after handoff, invoke the installed companion `babysit-pr` skill. This workflow itself only creates or updates the PR and hands it off.

### 10. Do not merge the pull request

Success is **PR opened (or updated) and handoff ready**, not “landed on main.” A checkpoint draft is complete when the push and PR exist with honest test status. A merge-ready claim additionally requires risk-matched review and the integrated native gates.

Never:

- `gh pr merge` (including `--auto`)
- GraphQL `mergePullRequest`
- REST `PUT /repos/.../merge`
- merge-queue / auto-merge enablement
- claiming the agent merged the PR

If the PR is already merged, report that a human merged it and stop.

## Done when

An open (or draft) PR has auto-merge and merge-queue disabled, links the input Linear task and/or existing GitHub issue without inventing a twin, remote source-branch tip and PR head both equal the recorded `head_sha`, the tracker comment includes URL + validated SHA + agent + checkpoint vs readiness, and no GitHub PR merge path was invoked.
