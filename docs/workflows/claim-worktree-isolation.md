# Claim → branch → worktree isolation

Portable contract for turning a GitHub issue or pull request into one isolated
git worktree. This is the primitive used by [Safe Issue → Verified Commit](safe-issue-verified-commit.md)
and by PR babysit workers. It never merges and never pushes.

Safety rules live in [`AGENTS.md`](../../AGENTS.md) and [`SKILL.md`](../../SKILL.md).

GitHub: [#6](https://github.com/rmems/writ/issues/6) · Linear: [RM-121](https://linear.app/rpd-34/issue/RM-121/claim-issue-branch-worktree-isolation) (`RM-6`)

## Inputs

| Input | Required | Notes |
| --- | --- | --- |
| GitHub issue or PR | yes | URL (`https://github.com/owner/repo/issues/N` or `/pull/N`) **or** owner + repo + number |
| `--repo` | yes | Local clone used as the shared object store (`git worktree add` from this root) |
| `--start-point` | yes | Exact commit or ref. Never derive it from the primary checkout's ambient `HEAD` |
| `--slug` | no | Issue path only. Sanitized into `hive/issue-<n>-<slug>` |
| `--head-branch` | PR only | Existing PR head branch name; never renamed |
| `--head-repo` | no | Fork slug `owner/repo` recorded on the result; git still uses `--repo` |

`writ claim` does not call `gh` or GitHub. The caller fetches refs and supplies
`--start-point` (and the PR head branch) after reading the issue or PR.

## Naming

| Kind | Job id (path segment) | Branch |
| --- | --- | --- |
| Issue | `gh-<n>` | `hive/issue-<n>` or `hive/issue-<n>-<slug>` |
| Pull request | `pr-<n>` | caller-supplied head branch, unchanged |

Worktree path: `{WRIT_WORKTREE_BASE}/{owner}/{repo}/{job_id}`.

Default base is `{user-data}/writ/worktrees` (legacy `{user-data}/worktrees-hives/worktrees`
if that root still exists). Override with `WRIT_WORKTREE_BASE` (or `WH_WORKTREE_BASE`).

The retired Python claim used `hive/gh-<n>`. Rust claim uses `hive/issue-<n>` so the
branch matches the portable worker contract. Job ids stay `gh-<n>` / `pr-<n>` so
paths remain unique per issue or PR.

## Commands

Issue from a URL:

```bash
writ --json claim issue \
  --repo <clone> \
  --start-point origin/main \
  --slug short-slug \
  --url https://github.com/acme/example/issues/42
```

Issue from owner / repo / number:

```bash
writ --json claim issue \
  --repo <clone> \
  --start-point origin/main \
  acme example 42
```

Pull request (existing local head branch is attached, not renamed):

```bash
writ --json claim pr \
  --repo <clone> \
  --start-point <full-pr-head-sha> \
  --head-branch feature/existing \
  acme example 9
```

If the PR head branch does not exist locally, claim creates it at `--start-point`
using that same name. If it exists, claim attaches only when the tip equals
`--start-point` and the branch is not checked out elsewhere.

Lower-level `writ worktree create` remains available. Claim is the issue/PR
policy layer: naming, one worktree per job, and PR-head attach.

## One worktree per job

A second claim for the same job id (`gh-<n>` or `pr-<n>`) fails with
`WORKTREE_ALREADY_CLAIMED` (exit 2). Claim does **not** reuse an existing tree.
Remove it first, or pick a different job.

## Isolation rule

Workers edit only the claimed worktree and branch. Before any edit:

1. `pwd` / `git rev-parse --show-toplevel` is the claimed path.
2. `git rev-parse --abbrev-ref HEAD` (or `git branch --show-current`) matches
   the claimed branch.
3. The assigned tree has no foreign uncommitted changes.
4. No files outside that worktree are modified.

A dirty or stale **primary** clone is not a blocker. Claim creates or attaches
from `--start-point` without stashing `main`.

## Cleanup

| Path | What to do |
| --- | --- |
| Success (PR opened or job finished) | `writ worktree remove <path>`. Keep the local branch; the PR still needs it. |
| Failure / crash | Leave residual state. Automatic deletion is forbidden because another agent may have adopted the branch. Report path, branch, and residual flags. |
| Orphan directory already gone | `writ worktree prune --repo <clone>` |
| Force-remove a dirty tree | `writ worktree remove --force <path>` |
| Local branch after PR open | **Do not delete** by default. Delete only when the operator asks and the branch has an upstream PR or is otherwise abandoned. |

There is no age-based TTL in v1. Operators prune stale trees under
`WRIT_WORKTREE_BASE` when disk grows. Sparse checkout is a non-goal.

## Failure modes

| Symptom | Code / exit | Recovery |
| --- | --- | --- |
| Missing `git` | `IO_ERROR` / 1 | Install git. There is no local fallback. |
| Missing `gh` | n/a | Claim does not invoke `gh`. Fetch PR metadata yourself (GitHub MCP, then `gh` if MCP is down). |
| Auth / network | n/a | Happens on the caller's `git fetch`, not inside claim. |
| Worktree already exists | `WORKTREE_ALREADY_CLAIMED` / 2 | Remove the job worktree or stop; do not claim twice. |
| Owner not allowlisted | `OWNER_NOT_ALLOWED` / 2 | Set `WRIT_ALLOWED_OWNERS` to include this owner, or unset it for an explicit single-repo claim. |
| Issue branch already exists | `WORKTREE_RESUME_UNPROVEN` / 2 | Do not reuse an unproven hive branch. Choose a new name or inspect the existing ref. |
| PR branch tip ≠ start point | `WORKTREE_ATTACH_MISMATCH` / 2 | Fetch the PR head and pass that exact commit. Claim will not move the branch. |
| PR branch checked out elsewhere | `WORKTREE_BRANCH_IN_USE` / 2 | Exclusive per PR id; finish or move the other worktree first. |
| Disk full / occupied path | `IO_ERROR` or `WORKTREE_CREATE_FAILED` / 1 | Residual branch is left in place. Free disk, then inspect `git branch` before retrying. |
| Invalid URL or number | `INVALID_CLAIM` / 1 | Use a `github.com` `/issues/N` or `/pull/N` URL, or owner + repo + positive number. |

Claim never merges, never enables auto-merge, and never pushes. Publishing the
branch is a later worker step.

## Job record fields

The JSON envelope (`claim.issue` / `claim.pr`) exposes the fields orchestrators
and the watched-job schema need:

`owner`, `repo`, `job_id`, `path`, `branch`, `issue_number`, `pr_number`,
`owns_branch`, `start_commit`, `head_commit`, `head_repo`, `repo_root`.

Claim does **not** write `watched.json`. Persistence belongs to the planned
lease store ([#124](https://github.com/rmems/writ/issues/124)), not a new
JSON writer.

## Residual risks

Submodule and LFS checkouts may need extra steps after `worktree add`. Fork PRs
whose head branch name collides with a local branch fail rather than renaming.
