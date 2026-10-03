# Checkpoint: scoped work to a push

Portable worker contract for any coding agent. Stops at an honest checkpoint push. Opening or updating a pull request is [Checkpoint → pull request](safe-verified-commit-to-pr.md).

Safety rules live in [`AGENTS.md`](../../AGENTS.md) and [`SKILL.md`](../../SKILL.md). This file does not relax them.

Historical filenames and contracts: isolation [#6](https://github.com/rmems/writ/issues/6), skill/procedure [#84](https://github.com/rmems/writ/issues/84). Those issues are history, not a required twin for new work.

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| Task | yes | Maintainer Linear task, or a contributor GitHub issue or pull request. One record is enough. |
| `owner` / `repo` | no | Resolve from `git remote` when omitted. Do not hard-code an owner. |
| `dry_run` | no | Intake, isolate, and plan only. No commit, push, or status comment. |

## Hard stops

Abort and report if any of these fail:

- The task has no actionable acceptance criteria, or a referenced GitHub issue is closed or is a pull request with nothing left to do.
- Owner is outside the configured allowlist, unless the operator named this repository explicitly.
- Unsafe identity or path mismatch, a genuine ownership collision, or a non-recoverable cleanliness or remote check. Exact remote-base equality applies only to a newly created, unpublished assigned branch. A published branch must have the expected upstream and local/remote relationship. Repair a clean bootstrap source or unpublished verified-base alignment before editing. A dirty or stale primary checkout is not a reason to abort.
- `writ` is missing and no enforcing wrapper is available for a mutating run. An enforcing wrapper routes the mutation through `writ-core`'s allowlist and branch verification. A wrapper that calls `git` directly is not one.
- A deny-listed command would be required (GitHub pull-request merge, local merge into `main`/`master`, dirty-WIP merge, bare `--force` / `-f`).
- The checkpoint push exits non-zero or the remote rejects it. Do not report that local HEAD as a pushed SHA.

## Stages

### 1. Intake

1. Read the Linear task or the existing GitHub issue with the host's authorized tools.
2. Read `AGENTS.md`, `README.md`, and [`REVIEW.md`](../../REVIEW.md).
3. Keep the acceptance criteria. Do not invent extra scope, a GitHub issue twin, a child issue, or a Beads claim.

### 2. Isolate

Read-only review stays in the current checkout. Writable work:

1. Start from an up-to-date base. Never edit `main` or `master`.
2. The harness creates the checkout (native worktree support, or plain `git worktree add <path> -b <branch> <remote>/<base>` after fetching). `writ worktree create` is deprecated. Never derive the start point from the source checkout's ambient `HEAD`.
3. Register it: `writ --json worktree register <path> --job <job-id>`. Registration records the observed branch, HEAD, and dirty state. It does not create, move, fetch, reset, or delete the checkout.
4. If `writ` is missing, stop, unless a wrapper routes the mutation through `writ-core`. Re-implemented policy that then calls `git` directly does not qualify. Routing provides the git/gh argv allowlist, local feature-branch merge with default-branch and dirty-WIP guards, no GitHub pull-request merge path, force-with-lease only, branch verification, supervised child processes, origin-slug matching for `gh -R`, and owner-allowlist enforcement for `gh` repository selectors.
5. For a newly created, unpublished assigned branch, prove it equals the fetched remote-base commit before edits. It may have no upstream only for that proof.
6. For a published assigned branch, verify the expected upstream and local/remote relationship. Stop on an unexpected upstream, unexpected remote commit, behind state, or divergence.
7. Run the rest of the pre-edit checklist in `AGENTS.md`: worktree path, branch name, clean assigned tree, no path escape.
8. One writable checkout per writer. Do not share it.

If `bd` is already installed and you choose to use Beads, you may claim a bead. If `bd` is missing, continue. Do not install Beads in order to start.

### 3. Implement

Change only that checkout and branch. Stay inside the task. No drive-by refactors.

### 4. Focused checks

Run checks that cover the change. Record what you ran and any failure. Do not treat an unrun or failing check as passed. A full workspace suite is not required before a checkpoint push.

If a check you need in order to claim the task is done fails or times out, report that residual. You may still push a checkpoint when the report is honest. Do not claim the task is merge-ready.

### 5. Commit

- Focused commits. Stage only the intended files. No secrets and no unrelated dirt.
- The message names the change and the task id you already have (Linear `RM-*` or a GitHub issue). Do not invent the other tracker.

### 6. Push

Immediately before push, confirm the worktree path, branch, and remote still match the assignment. Abort on a mismatch.

1. On a newly created, unpublished branch, fetch the remote base again and rebase or align only when that rebase is successful and conflict-free. On a published branch, reconcile with the expected upstream the same way. If alignment conflicts, stop and record the residual. Do not report a push that did not happen.
2. Push with `git push -u <verified-remote> <assigned-branch>` the first time, then `git push`. If the remote rejects it, stop. Do not report `git rev-parse HEAD` as a pushed SHA.
3. Never merge a GitHub pull request. Local `git merge` of peer work into this assigned feature branch is allowed. Never merge into `main`/`master`, and refuse a merge that would lose uncommitted WIP.
4. Never `git push --force` or `git push -f`. `--force-with-lease` only on this job branch after a rebase you own, and only after the identity check above.

### 7. Report

After the remote accepts the push, comment on the task you started from. Include the branch, the pushed SHA, what landed, which checks ran, and residual blockers. Use [reply attribution](../../SKILL.md#reply-attribution). Do not open a twin issue to hold that comment. Do not claim a merge.

## Dry run

Stop after isolate and a written plan. No commit, push, or comment.

## Done when

The branch push was accepted, or you stopped with an explicit residual and did not report a rejected push as a SHA. Merge or release readiness is a later declaration, not this checkpoint.
