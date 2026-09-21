# Watchlist view schema

`writ watchlist` is a **consumer** of shared coordination state. RM-825 owns `leases.db` (leases, and later `coord_claims` / `coord_messages`). This command never writes those tables and never creates `watchlist.json`.

Path: `{user_data}/writ/leases.db`, overridable with `WRIT_LEASE_PATH`. The resolver still falls back to a pre-rename `worktrees-hives` root when `writ/` is absent. This process does not write into `pr-babysit/`. `list`/`check` open the lease store through the existing `LeaseStore` helper (which may create an empty `leases`/`agents` schema if the file is missing). Coord overlay opens the same file **read-only** and never creates `coord_*` tables.

## Commands

| Command | GitHub probes | Persistence |
| --- | --- | --- |
| `writ watchlist list` | no | none |
| `writ watchlist check` / `check-all` | live `gh pr list --head` | none |
| `writ watchlist add` / `remove` | no | none — prints a hint to use `worktree register` / `unregister` |

Filters: `--owner`, `--repo`, `--job`, `--include-released`. Multi-owner rows coexist; identity is `(owner, repo_name, job_id)` from the lease store. GitHub `--repo` selectors still require `WRIT_ALLOWED_OWNERS`.

## Envelope

```json
{
  "ok": true,
  "schema_version": 1,
  "command": "cli.watchlist.list",
  "data": {
    "entries": [],
    "coord_available": false,
    "github_probed": false
  },
  "error": null
}
```

`coord_available` is true only when `coord_claims` exists in the same SQLite file. Missing tables are not an error.

## `collab_status` (local)

Distinct from GitHub check status. Not a merge gate.

| Value | Meaning |
| --- | --- |
| `running` | Held writer lock, not paused or blocked |
| `waiting` | Unacked help/handoff/dependency, or `BLOCKED` / `REVIEW_ONLY` |
| `paused` | Coord claim `paused_at` set; WIP is preserved |
| `conflicted` | `NEEDS_HUMAN` or GitHub `mergeable=CONFLICTING` |
| `ready_for_integration` | Lease mode `MERGE_READY` (local integration, not GitHub-merged) |

## `recovery_status`

| Value | Meaning |
| --- | --- |
| `live` | Unreleased lease and checkout path exists |
| `released` | Identity row kept after unregister |
| `stale_heartbeat` | `ttl` elapsed since last heartbeat |
| `missing_checkout` | Lease path is gone; harness owns cleanup |

## GitHub overlay

Present only after `check` / `check-all`. `check_status` values: `healthy`, `pending`, `failed`, `residual`, `conflict`, `merged`, `closed`, `unknown`. Residuals use prefixed codes (`class_a:…`, `class_b:…`, `class_c:…`, `review:…`, `conflict:mergeable`). Locally integrated/tested (`ready_for_integration`) is independent of GitHub `MERGED`.
