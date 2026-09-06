# Safe Issue → Verified Commit

Portable worker contract for any coding agent (Codex, Claude, Grok, Hermes, Devin, or later). Stops at a verified push and an issue comment. Does not open a PR — that is [Safe Verified Commit → PR](safe-verified-commit-to-pr.md).

Safety rules live in [`AGENTS.md`](../../AGENTS.md) and [`SKILL.md`](../../SKILL.md). This file does not relax them.

Contracts: isolation [#6](https://github.com/rmems/writ/issues/6), skill/procedure [#84](https://github.com/rmems/writ/issues/84).

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| `issue` | yes | GitHub issue URL or number |
| `owner` / `repo` | no | If omitted, resolve from `git remote` in the current repository |
| `dry_run` | no | Intake + isolate + plan only. No commit, push, or issue comment |

Do not hard-code an owner. Multi-repo discovery and scheduling still use `WRIT_ALLOWED_OWNERS` / explicit API args.

## Hard stops

Abort and report if any of these fail:

- Issue is closed, is a pull request, or has no actionable acceptance criteria
- Owner is outside the configured allowlist (unless the operator named this repo/job explicitly). **No code enforces this today** -- `WRIT_ALLOWED_OWNERS` has no reader under `crates/` since the Python layer was removed, so this stop depends on the operator, not the boundary (#146).
- Unsafe identity or path mismatch, a genuine ownership collision, or a non-recoverable cleanliness/remote check. Exact remote-base equality applies only to a newly created, unpublished assigned branch. A published branch must have the expected upstream and local/remote relationship instead. Repair a clean bootstrap source or unpublished verified-base alignment before editing; do not abort isolated work because a primary checkout is dirty or stale.
- `writ` is missing and no enforcing wrapper is available (mutating runs). An "enforcing wrapper" means a wrapper that routes the mutation through `writ-core`'s allowlist and branch verification; a wrapper that merely calls `git` directly is not one, and does not satisfy this check.
- Any required quality gate fails or times out
- A deny-listed command would be required (GitHub merge, local merge of another PR or stacked/peer branch, bare `--force` / `-f`)
- `git push` exits non-zero or the remote rejects the push

## Stages

### 1. Intake (read-only)

1. Read the GitHub issue with **GitHub MCP first**. Shell `gh` only if MCP is unavailable.
2. Read `AGENTS.md` or `CLAUDE.md`, `README.md`, and [`REVIEW.md`](../../REVIEW.md).
3. Extract acceptance criteria. Preserve them. Do not invent extra scope.
4. If a Linear twin is already linked, note its id. Do not create a second twin.

### 2. Isolate

1. If this repo uses Beads, run `bd prime`, inspect `bd ready`, and claim the relevant bead.
2. Start from an up-to-date base. Never edit `main` or `master`.
3. Create or reuse a dedicated branch and isolated worktree:
   - Required: `writ --json worktree create --schema-version 2 --repo <repo> --start-point <exact-commit-or-ref> <owner> <repo-name> <job-id> <branch>` (`WRIT_BIN` or `PATH`). Never omit the caller-selected boundary version/start point or derive it from the source checkout's ambient `HEAD`.
   - If `writ` is missing, stop, unless a wrapper routes the mutation through `writ-core` -- the same definition as the hard stop above. A wrapper that re-implements the checks itself and then calls `git` directly does **not** qualify: re-implemented policy is prompt-level text, not the code-enforced boundary.
   - What routing through `writ-core` actually gets you, so the promise is not larger than the code: the git/gh argv allowlist, no merge path, force-with-lease only, sandboxed path derivation with symlink rejection, exact-base resolution, branch and `HEAD` postcondition verification, supervised child processes, and origin-slug matching for `gh -R`. **Owner-allowlist enforcement is not among them.** `WRIT_ALLOWED_OWNERS` has no reader anywhere under `crates/`; it was enforced in the Python layer this PR deletes, so it is currently policy text with no code behind it.
   - Raw `git worktree add` is forbidden on mutating runs.
4. Suggested issue branch: `hive/issue-<n>-<short-slug>` (document any local override).
5. For a newly created, unpublished assigned branch, fetch the intended remote base and prove that the branch equals that exact remote-base commit before edits. It may have no upstream only for this creation proof; never use an ambient or stale `HEAD` as the base.
6. For a published assigned branch, fetch and verify the configured expected upstream and the expected local/remote relationship; do not compare the branch for equality with the base. Stop on an unexpected upstream, unexpected remote commit, behind state, or divergence.
7. Run the remaining pre-edit checklist in `AGENTS.md` / `SKILL.md`: worktree path, `git branch --show-current`, clean assigned tree, expected remote, no path escape.
8. One writable worktree per job. Do not share it with another agent.

### 3. Implement

- Change only that worktree and branch.
- Stay inside Rust / Python / skill ownership (`AGENTS.md`).
- No drive-by refactors. No files outside the worktree.

### 4. Focused validation during implementation (fail-closed)

Run focused, task-relevant checks as changes are made. An issue may add
focused checks. Do not use those extra checks, or an early full native
suite, as a substitute for the final exact-head run in Stage 6.

The complete native suite for this repository is always:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If `python/` changed, also run the Python test extra documented in `README.md`.

Run every `cargo` and Python check under an explicit process timeout supplied by the host (orchestrator supervisor, CI job timeout, or equivalent). Do not use a Linux-only timeout command as the contract.

If a focused required gate fails or the process is killed for time, **do not advance to the final publication sequence**. Report the failure or timeout as a residual on the issue. A hang or timeout is not a license to skip the gate and push anyway.

After the first tested implementation, obtain one independent review matched to
the change's risk before final publication. Additional review is required only
for a named high-risk boundary or a reproduced finding that warrants follow-up.
Address in-scope findings and rerun affected focused checks before continuing.

### 5. Commit

- One or few focused commits. Scoped `git add` (no secrets, no unrelated dirt).
- Message names the agent and links the GitHub issue (and Linear id if already known).
- Beads status matches reality.

### 6. Push

Immediately before final alignment or `git push`, fail closed if any of these differ from the assigned job: worktree path (`git rev-parse --show-toplevel`), job branch (`git branch --show-current`), or remote. A published branch must also have its expected upstream and local/remote relationship. A newly created, unpublished assigned branch may lack an upstream only when the Stage 2 exact remote-base proof succeeded. Abort and report on any mismatch. Do not rebase or push on a mismatch.

Then:

1. On a newly created, unpublished branch, fetch and verify the expected remote base again, then perform final alignment or a **successful, conflict-free** rebase on that assigned unpublished branch. Never let an ambient or stale `HEAD` substitute for the verified base. On a published branch, fetch its expected upstream and reconcile the local branch with that upstream; `git pull --rebase` must be successful and conflict-free when a pull is needed. Do not require the published branch to equal the base. If alignment or reconciliation fails or leaves conflicts, **stop**: record the issue as a residual, do **not** run the full native gates, and do **not** `git push`.
2. Always run the complete native suite named in Stage 4 exactly once (with the same process timeouts) on the exact `HEAD` that would be pushed. An issue may add focused checks but must not replace or reduce this suite. Any later tree change invalidates the run and requires repeating final alignment and this full suite. If any gate fails or times out, **do not push**. Report residuals.
3. For a first publication, push the already verified remote and assigned branch explicitly: `git push -u <verified-remote> <assigned-branch>`. After upstream is set, use `git push`. If the command exits non-zero or the remote rejects the update, **stop before Stage 7**: record the push failure as a residual. Do **not** report `git rev-parse HEAD` as the pushed SHA.

- Never merge, including a local `git merge` of another PR or stacked/peer branch.
- Never `git push --force` or `git push -f`.
- `--force-with-lease` only on **this** job branch after a rebase you own, after the identity check above succeeds.

### 7. Report

Run this stage **only** after Stage 6 step 3 succeeded (push accepted by the remote).

Comment on the GitHub issue (MCP first) with:

- branch
- pushed SHA (`git rev-parse HEAD` **after** that successful push)
- what landed
- residual blockers
- agent name

Do not claim a merge. Do not open a PR here. A local HEAD SHA after a failed or rejected push is not a completion report.

## Dry run

Stop after isolate + a written implementation plan. No commits, push, or issue comment.

## Done when

Branch is pushed (remote accepted), required gates passed (or residuals are explicit and nothing was committed or reported as pushed over a failed gate, timeout, rebase failure, or rejected push), and the issue has a SHA-bearing comment only after that successful push.
