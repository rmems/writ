# Safe Issue → Verified Commit

Portable worker contract for any coding agent (Codex, Claude, Grok, Hermes, Devin, or later). Stops at a verified push and an issue comment. Does not open a PR — that is [Safe Verified Commit → PR](safe-verified-commit-to-pr.md).

Safety rules live in [`AGENTS.md`](../../AGENTS.md) and [`SKILL.md`](../../SKILL.md). This file does not relax them.

Contracts: isolation [#6](https://github.com/rmems/writ/issues/6), skill/procedure [#84](https://github.com/rmems/writ/issues/84).

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| `task` | yes | Linear `RM-*` (maintainers) and/or GitHub issue URL/number. Do not create a GitHub issue twin for Linear-only work. |
| `owner` / `repo` | no | If omitted, resolve from `git remote` in the current repository |
| `dry_run` | no | Intake + isolate + plan only. No commit, push, or issue comment |

Do not hard-code an owner. Multi-repo discovery and scheduling still use `WRIT_ALLOWED_OWNERS` / explicit API args.

## Hard stops

Abort and report if any of these fail:

- Task is closed, is a pull request, or has no actionable acceptance criteria
- Owner is outside the configured allowlist (unless the operator named this repo/job explicitly).
- Unsafe identity or path mismatch, a genuine ownership collision, or a non-recoverable cleanliness/remote check. Exact remote-base equality applies only to a newly created, unpublished assigned branch. A published branch must have the expected upstream and local/remote relationship instead. Repair a clean bootstrap source or unpublished verified-base alignment before editing; do not abort isolated work because a primary checkout is dirty or stale.
- `writ` is missing and no enforcing wrapper is available (mutating runs). An "enforcing wrapper" means a wrapper that routes the mutation through `writ-core`'s allowlist and branch verification; a wrapper that merely calls `git` directly is not one, and does not satisfy this check.
- A **merge-ready** claim when a required quality gate failed or timed out (a checkpoint with honest residuals is not this stop)
- A deny-listed command would be required (GitHub PR merge, local merge into `main`/`master`, dirty-WIP merge, bare `--force` / `-f`)
- `git push` exits non-zero or the remote rejects the push

## Stages

### 1. Intake (read-only)

1. Read the Linear task and/or GitHub issue (GitHub **MCP first** for GitHub). Shell `gh` only if MCP is unavailable.
2. Read `AGENTS.md` or `CLAUDE.md`, `README.md`, and [`REVIEW.md`](../../REVIEW.md).
3. Extract acceptance criteria. Preserve them. Do not invent extra scope.
4. Do not create a GitHub issue twin for Linear work, or a Linear task for a GitHub-only contribution.

### 2. Isolate

1. Beads is optional. If `bd` is on `PATH`, you may claim a bead; missing `bd` is not a stop. Native/harness task lists are allowed.
2. Start from an up-to-date base. Never edit `main` or `master`.
3. Create or reuse a dedicated branch and isolated checkout:
   - The harness creates the checkout wherever it wants (native worktree support, or plain `git worktree add <path> -b <branch> <remote>/<base>` after fetching). `writ worktree create` is deprecated; do not use it for new work. Never derive the start point from the source checkout's ambient `HEAD`.
   - Required: `writ --json worktree register <path> --job <job-id>` (`WRIT_BIN` or `PATH`). Registration is coordination-only: it records the observed branch/HEAD/dirty state and grants a lease row. It does not create, move, fetch, reset, or delete the checkout, and a standalone clone registers fine.
   - If `writ` is missing, stop, unless a wrapper routes the mutation through `writ-core` -- the same definition as the hard stop above. A wrapper that re-implements the checks itself and then calls `git` directly does **not** qualify: re-implemented policy is prompt-level text, not the code-enforced boundary.
   - What routing through `writ-core` actually gets you, so the promise is not larger than the code: the git/gh argv allowlist, local feature-branch merge with default-branch and dirty-WIP guards, no GitHub PR merge path, force-with-lease only, branch verification, supervised child processes, origin-slug matching for `gh -R`, and owner-allowlist enforcement for `gh` repository selectors (`-R` / `--repo` in any pflag spelling, plus `GH_REPO`; `WRIT_ALLOWED_OWNERS`, `WH_ALLOWED_OWNERS`, or `--allowed-owners` / explicit API args; empty or unset denies). Checkout placement and lifecycle are no longer writ-enforced; they are the harness's responsibility.

4. Suggested issue branch: `hive/issue-<n>-<short-slug>` (document any local override).
5. For a newly created, unpublished assigned branch, fetch the intended remote base and prove that the branch equals that exact remote-base commit before edits. It may have no upstream only for this creation proof; never use an ambient or stale `HEAD` as the base.
6. For a published assigned branch, fetch and verify the configured expected upstream and the expected local/remote relationship; do not compare the branch for equality with the base. Stop on an unexpected upstream, unexpected remote commit, behind state, or divergence.
7. Run the remaining pre-edit checklist in `AGENTS.md` / `SKILL.md`: worktree path, `git branch --show-current`, clean assigned tree, expected remote, no path escape.
8. One writable worktree per job. Do not share it with another agent.

### 3. Implement

- Change only that worktree and branch.
- Stay inside Rust / skill ownership (`AGENTS.md`).
- No drive-by refactors. No files outside the worktree.

### 4. Focused validation during implementation (fail-closed)

Run focused, task-relevant checks as changes are made. An issue may add
focused checks. Checkpoint commits may proceed with honest residuals; do not
call an unrun or failing check passed. Do not use an early full native suite
as a substitute for merge-readiness validation in Stage 6.

The complete native suite for this repository is always:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run every `cargo` check under an explicit process timeout supplied by the host (orchestrator supervisor, CI job timeout, or equivalent). Do not use a Linux-only timeout command as the contract.

If a focused required gate fails or the process is killed for time, **do not
claim merge readiness**. You may still make a checkpoint commit or draft PR
when the residual is stated honestly. A hang or timeout is not a license to
report a failed gate as passed.

After the first tested implementation, obtain one independent review matched to
the change's risk before final publication. Additional review is required only
for a named high-risk boundary or a reproduced finding that warrants follow-up.
Address in-scope findings and rerun affected focused checks before continuing.

### 5. Commit

- One or few focused commits. Scoped `git add` (no secrets, no unrelated dirt).
- Message names the agent and links Linear `RM-*` and/or the GitHub issue when one exists.
- If Beads is in use, keep its status aligned; otherwise skip it.

### 6. Push

Immediately before final alignment or `git push`, fail closed if any of these differ from the assigned job: worktree path (`git rev-parse --show-toplevel`), job branch (`git branch --show-current`), or remote. A published branch must also have its expected upstream and local/remote relationship. A newly created, unpublished assigned branch may lack an upstream only when the Stage 2 exact remote-base proof succeeded. Abort and report on any mismatch. Do not rebase or push on a mismatch.

Then:

1. On a newly created, unpublished branch, fetch and verify the expected remote base again, then perform final alignment or a **successful, conflict-free** rebase on that assigned unpublished branch. Never let an ambient or stale `HEAD` substitute for the verified base. On a published branch, fetch its expected upstream and reconcile the local branch with that upstream; `git pull --rebase` must be successful and conflict-free when a pull is needed. Do not require the published branch to equal the base. If alignment or reconciliation fails or leaves conflicts, **stop**: record the issue as a residual, do **not** run the full native gates, and do **not** `git push`.
2. For a **checkpoint** push or draft PR, rerun the focused checks that cover the change and report that status truthfully. For a **merge-ready** claim, run the complete native suite named in Stage 4 exactly once (with the same process timeouts) on the exact `HEAD` that would be pushed. Any later tree change invalidates a readiness run. If a readiness gate fails or times out, **do not claim merge-ready**. A checkpoint may still push when residuals are explicit.
3. For a first publication, push the already verified remote and assigned branch explicitly: `git push -u <verified-remote> <assigned-branch>`. After upstream is set, use `git push`. If the command exits non-zero or the remote rejects the update, **stop before Stage 7**: record the push failure as a residual. Do **not** report `git rev-parse HEAD` as the pushed SHA.

- Never merge a GitHub pull request. Local `git merge` of peer work into this assigned feature branch is allowed; never merge into `main`/`master`, and refuse a merge that would lose uncommitted WIP.
- Never `git push --force` or `git push -f`.
- `--force-with-lease` only on **this** job branch after a rebase you own, after the identity check above succeeds.

### 7. Report

Run this stage **only** after Stage 6 step 3 succeeded (push accepted by the remote).

Comment on the Linear task (maintainers) and, when a GitHub issue already exists, on that issue (MCP first) with:

- branch
- pushed SHA (`git rev-parse HEAD` **after** that successful push)
- what landed
- residual blockers and whether this is a checkpoint or a merge-ready claim
- agent name, using the [reply attribution](../../SKILL.md#reply-attribution) templates (`writ attribution format`)

Do not claim a merge. Do not open a PR here. A local HEAD SHA after a failed or rejected push is not a completion report.

## Dry run

Stop after isolate + a written implementation plan. No commits, push, or issue comment.

## Done when

Branch is pushed (remote accepted), status is truthful (checkpoint vs merge-ready), and the task or GitHub issue has a SHA-bearing comment only after that successful push.
