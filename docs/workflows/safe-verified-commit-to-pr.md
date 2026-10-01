# Checkpoint → pull request

Runs after [Checkpoint: scoped work to a push](safe-issue-verified-commit.md). Opens or updates a pull request. Does not merge it, and opening it does not declare merge or release readiness.

Safety rules live in [`AGENTS.md`](../../AGENTS.md), [`SKILL.md`](../../SKILL.md), and [`REVIEW.md`](../../REVIEW.md).

Historical contract: Issue → PR [#8](https://github.com/rmems/writ/issues/8) / Linear [RM-123](https://linear.app/rpd-34/issue/RM-123/issue-pr-workflow-never-auto-merge). That history is not a required GitHub issue twin. Interactive monitoring, when needed, belongs to the installed companion `babysit-pr` skill.

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| Pushed branch | yes | The remote accepted the push. Test status is honest, including checks not run. |
| Task | yes | The Linear task or GitHub issue you already have. |
| `owner` / `repo` | no | Resolve from `git remote` when omitted. |
| `dry_run` | no | Describe the pull request. Do not create or update it. |

## Hard stops

- No accepted push yet.
- Shared `main`/`master` checkout. Work only in the assigned checkout.
- Owner outside the configured allowlist unless the operator named this job.
- Any GitHub pull-request merge command, merge API, auto-merge, or merge-queue enablement.
- Bare `git push --force` / `git push -f`.
- A pull request with zero commits.

## Open or update

Use the host's authorized GitHub tools.

1. Confirm the job checkout and branch (`git rev-parse --show-toplevel`, `git branch --show-current`).
2. `git fetch`. Read `HEAD` after the last successful push.
3. Base is the repository default branch, or the stack parent. Process stacks bottom-up.
4. If a pull request already exists for this branch, update it. Do not open a second one for the same task.
5. Otherwise create it. The title reflects the task. Link the Linear id when that is the maintainer task. Link a GitHub issue only when one already exists (`Fixes #<n>` only when that issue is fully done; otherwise `Refs #<n>`). Leave auto-merge off. A checkpoint stays **draft** until readiness is claimed.

Suggested body:

```markdown
## Summary

<what changed>

## Task

Linear: RM-<n>
Refs #<n>

## Test plan

- [ ] Focused checks: <what ran, and the result>
- [ ] Not run: <what you did not run>
- [ ] Required GitHub checks: <pending, green, or failing. Do not mark a pass you did not see>

## Notes for review

- Checkpoint or merge-ready:
- Residuals:
```

Omit the Linear line when the contributor has no Linear id. Omit the GitHub issue line when no issue exists.

## Handoff

Record the repository, task id, pull-request number and URL, branch, `head_sha` after the last successful push, status (`draft`, `open`, or `blocked`), and the honest test note.

Before calling the handoff done, confirm with the host's authorized GitHub tools:

1. The pull request is open or draft.
2. Auto-merge is disabled and the pull request is not in the merge queue.
3. The recorded `head_sha` matches the pull-request head and the remote branch tip.
4. Any GitHub issue link points at an issue that already existed. A missing issue link is not a failure when the task is Linear-only.

If a check fails, report the residual. Do not merge, and do not call a draft merge-ready.

Comment on the existing task with the pull-request URL, the validated pushed SHA, residuals, and agent attribution. Do not create a GitHub issue to hold that comment.

## Readiness

Call the pull request merge-ready only when the validation and review the change needs are done, required GitHub checks are green, conflicts are absent, and review threads that block the change are resolved. That status is advisory. A human merges on GitHub. This workflow does not merge.

Never:

- `gh pr merge` (including `--auto`)
- GraphQL `mergePullRequest`
- REST `PUT /repos/.../merge`
- merge-queue or auto-merge enablement
- claiming the agent merged the pull request

If the pull request is already merged, report that a human merged it and stop.

## Done when

An open or draft pull request exists, auto-merge and the merge queue are off, the head SHA matches the accepted push, and the task comment includes the URL, SHA, and honest test status. Readiness is separate and is not implied by opening the pull request.
