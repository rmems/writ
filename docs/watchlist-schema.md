# Watchlist JSON schema

`writ watchlist` persists a **multi-owner PR watchlist** so check cycles can
`add` / `remove` / `list` / `check-all` without rediscovering work each session.

This file is **not** [`watched.json`](status-schema.md). `writ status` / `writ jobs`
read a JSON *array* of job-status objects from `watched.json` (read path only).
The watchlist is a versioned *object* in `watchlist.json`. Mixing the two schemas
would break status output.

## Location

| | Path |
| --- | --- |
| Default file | `{user_data}/writ/watchlist.json` |
| User data (Unix) | `$XDG_DATA_HOME` or `~/.local/share` |
| User data (macOS) | `~/Library/Application Support` |
| User data (Windows) | `%APPDATA%` |
| Legacy root | `{user_data}/worktrees-hives/watchlist.json` if that directory exists and `writ/` does not |
| Override | `WRIT_WATCHLIST_PATH`, then `WH_WATCHLIST_PATH` |

The parent directory is created on first write (self-bootstrapping). File mode on
Unix is `0600` because titles are stored locally.

`WRIT_STATE_PATH` / `WH_STATE_PATH` point at job-status `watched.json` and are
**never** used as the watchlist path.

The store never writes into `pr-babysit/`. `writ watchlist import-pr-babysit`
reads that tree if you ask it to.

## Commands

| Command | Envelope `command` | Effect |
| --- | --- | --- |
| `writ watchlist add [--repo owner/name] <number>…` | `watchlist.add` | `gh pr view`; skip MERGED/CLOSED; dedupe `(repo, number)` |
| `writ watchlist remove [--repo owner/name] <number>` | `watchlist.remove` | Remove one entry; stack-mates stay |
| `writ watchlist list [--owner] [--repo]` | `watchlist.list` | All owners by default |
| `writ watchlist check [--repo owner/name] <number>` | `watchlist.check` | One GitHub refresh |
| `writ watchlist check-all [--owner] [--repo]` | `watchlist.check_all` | Full walk in stack order |
| `writ watchlist import-pr-babysit [--path FILE]` | `watchlist.import_pr_babysit` | Read-only copy from pr-babysit JSON |

`--state FILE` on each subcommand overrides the path (tests and operators).

`--json` uses the shared v1 envelope. Diagnostics go to stderr.

## Schema

```json
{
  "version": 1,
  "prs": [
    {
      "repo": "acme/widgets",
      "number": 41,
      "branch": "fix-ci",
      "base": "feat/stack-base",
      "title": "Fix CI",
      "stack_id": "ae-latency",
      "stack_type": null,
      "stack_position": 1,
      "status": "residual",
      "added_at": "2026-07-09T17:00:00Z",
      "last_checked": "2026-07-09T18:00:00Z",
      "check_count": 2,
      "fix_count": 1,
      "residual_blockers": ["class_b:codacy_action_required", "review:REVIEW_REQUIRED"],
      "kind": "pr_babysit",
      "url": "https://github.com/acme/widgets/pull/41"
    }
  ],
  "groups": {
    "ae-latency": {
      "repo": "acme/widgets",
      "numbers": [36, 41]
    }
  }
}
```

Identity is `(repo, number)`. `add` refreshes branch/base/title and preserves
`fix_count` unless `--reset`.

`groups` is `stack_id → { repo, numbers }` with numbers in bottom-up order.

Unknown additive fields on the document or on an entry are preserved on write.

## Status enum

| Value | Meaning |
| --- | --- |
| `healthy` | Checks passed, no residuals |
| `pending` | Checks still running, or GitHub returned no status checks yet |
| `failed` | At least one failing check (`class_a:…`) |
| `residual` | Leftovers such as review (`review:…`) or class B/C |
| `conflict` | GitHub `mergeable=CONFLICTING` |
| `timeout` | `gh pr view` timed out (`timeout:gh`) |

MERGED/CLOSED PRs are skipped on `add` and pruned on `check` / `check-all`.

`check-all` updates `last_checked`, `status`, `residual_blockers`, and
`check_count`. It does **not** increment `fix_count` (babysit / #9 owns that
budget). It does not spawn workers; that remains orchestrator work.

Residual codes prefer `#10` prefixes: `class_a:`, `class_b:`, `class_c:`,
`review:`, `conflict:`, `timeout:`.

## Multi-owner and allowlist

`list` shows every owner in the file. `--owner` / `--repo` are optional filters.

`check-all` without `--repo`/`--owner` is a multi-owner walk and requires
`WRIT_ALLOWED_OWNERS` (comma-separated). An empty allowlist denies that walk
(`OWNER_ALLOWLIST_REQUIRED`, exit 2). There is no built-in owner list.

`add` of an explicit `owner/name` is a single-repository operation: it is
allowed when the allowlist is empty, and rejected with `OWNER_NOT_ALLOWED` when
the allowlist is set and does not include that owner.

## Integrity

- Writes are a temp file in the same directory plus `rename`. On Windows the
  destination is removed first so an existing file can be replaced.
- Missing file → empty watchlist (not an error).
- Corrupt JSON → quarantine to `watchlist.json.corrupt.<stamp>`, warn, exit
  non-zero. The next command sees a missing file (empty list). The original
  bytes are not overwritten.
- Do not run two `check-all` writers against the same file.

## Kind

`kind` is `pr_babysit` (default) or `issue_to_pr`.
