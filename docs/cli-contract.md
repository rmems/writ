# CLI compatibility and capability contract

This reference records the command surface observed at commit
`9537f6703b52cd770aa643f1e0938bfa02ca32f4` (2026-09-22). It is a compatibility
snapshot, not a promise that future releases will retain deprecated commands.
The inventory was checked against `writ --help`, each subcommand's help, the
Rust dispatch in `crates/writ/src/main.rs`, and the native CLI/contract tests.

`--json` is a global option and may appear before or after a subcommand. Unless
the table says otherwise, human mode prints a summary while JSON mode prints one
compact envelope on stdout:

```json
{"ok":true,"schema_version":1,"command":"cli.bootstrap","data":{},"error":null}
```

The generic envelope is version 1, but it is not the schema for every boundary.
Status success **and failure** use version 2. Deprecated exact-base worktree
creation also uses version 2 when explicitly selected. Callers must inspect both
`command` and `schema_version`; they must not treat one global version as the
version of every payload.

## Implemented command matrix

| CLI command | State | Human / JSON output | JSON `command`; success / failure schema | Exit behavior | Persistence and effects |
| --- | --- | --- | --- | --- | --- |
| `writ` (no subcommand) | Current | Human: no output. JSON: bootstrap envelope. | `cli.bootstrap`; v1 / n/a | 0 | Read-only; no store is opened. |
| `status`, `jobs` | Current | Human collaboration summary or JSON snapshot. | `cli.status`, `cli.jobs`; **v2 / v2** | 0 on a readable or missing store; 1 on load/query failure. | Read-only view of the same-host SQLite lease store. A missing store is an empty view and is not created. |
| `git-safe …` | Current | Human execution summary plus child streams; JSON v1 result including child stdout, stderr, and exit code. | `git.safe`; v1 on an executed command; validation failures have no envelope. | Propagates child 1–255; maps other nonzero values to 1. Policy rejection is 2. | Runs the validated `git` subprocess. Effects are those of the admitted git command; writ adds no state store. |
| `gh-safe …` | Current | Same shape as `git-safe`. | `gh.safe`; v1 on an executed command; validation failures have no envelope. | Same child-code mapping; policy rejection is 2. | Runs the validated `gh` subprocess. GitHub owns remote PR/review/check/merge policy; `gh pr merge` is rejected. No local writ persistence. |
| `supervisor run …` | Current | Human mode emits the supervised result JSON (without an envelope); JSON mode emits an envelope. | `supervisor.run`; v1. Runtime/policy failures use v1 when the supervisor writes a response. | Human mode propagates a child code, uses 124 for timeout/kill, and 1 for spawn/unknown failure. JSON mode returns 0 for a completed supervised run even when its payload records child failure/timeout; boundary errors remain nonzero and policy errors are 2. | Owns child/process-group supervision and in-process concurrency only. It does not persist coordination state, retry, merge, push, or delete a checkout. |
| `worktree register PATH`, `unregister PATH` | Current | Human `ok=… command=…` summary or JSON. | `worktree.register`, `worktree.unregister`; v1 / v1. | 0 success; 1 operational error; **2** when `register` hits an active lease held by another checkout path (`LEASE_CONFLICT`). | Mutate lease rows in the same-host SQLite store. They record/release an existing harness-owned checkout and never create/delete its files or branch. |
| `worktree inspect PATH` | Current | Human summary or JSON. | `worktree.inspect`; v1 / v1. | 0 success; 1 operational error; **2** when the path is not a git checkout (`GIT_DIR_UNAVAILABLE`). | Read-only checkout metadata; does not open the lease store or register the checkout. |
| `worktree list` | Current | Human summary or JSON. | `worktree.list`; v1 / v1. | 0 success; 1 operational error. | Reads lease rows, but when `WRIT_LEASE_PATH` is missing, `CheckoutRegistry::new()` opens the store **writable** and creates the parent directory, SQLite file, and schema before listing. |
| `worktree create … --schema-version 2 --start-point …` | **Deprecated** | Human summary or JSON exact-base result. The default v1 request deliberately returns an upgrade error. | `worktree.create`; selected v2 for success and v2 failures after selection; default request fails in a v1 envelope naming required v2. | 0 success; 1 contract/operational error; 2 policy rejection. | Legacy managed lifecycle: mutates git worktrees/branches and registers a lease. Harness-owned checkout creation plus `register` is the current path. |
| `worktree remove PATH`, `prune --repo …` | **Deprecated** | Human summary or JSON. | `worktree.remove`, `worktree.prune`; v1 / v1. | 0 success; 1 operational error; 2 policy rejection where applicable. | Legacy git worktree mutation **plus** coordination-store side effects: both commands construct `WorktreeManager`, which opens the lease store writable (creating it when absent). `remove` calls `release_by_path` after a successful removal and also when the path is already absent. Harnesses own physical cleanup; prefer `unregister` when only the lease row should change. |
| `attribution format …` | Current | Rendered text or JSON result. | `attribution.format`; v1 / v1. | 0 success; 1 invalid/operational input. | Pure formatting; no persistence or network access. |
| `install` | Current | Human changed/unchanged line or JSON result. | `cli.install`; v1 on success; installation errors have no envelope. | 0 success; 1 operational error. | Idempotently creates or rewrites the selected Claude Code settings file (default `.claude/settings.json` relative to the current working directory when `--settings` is omitted). Does not mutate git metadata or lease rows, but can dirty the checkout tree. |
| `hook` | Current | Hook protocol output; `--json` does **not** add a CLI envelope. Input is hook JSON on stdin. | No envelope identifier/version. | 0 allow/no-op; 2 block. | `PreToolUse` validates commands at the process boundary. Worktree lifecycle events may register/release lease rows but never create/remove checkout files. `SubagentStart`/`SubagentStop` upsert/retire rows in the `agents` table via a writable lease-store connection (creating the store when absent). |
| `watchlist list` | Current view | Human table or JSON. | `cli.watchlist.list`; v1 / v1. | 0 success; 1 operational error. | Read-only lease-store view; a missing store remains missing. |
| `watchlist check`, `check-all` | Current view | Human table or JSON with an ephemeral GitHub overlay. | `cli.watchlist.check`, `cli.watchlist.check_all`; v1 / v1. | 0 success; 1 operational error. Policy-denied GitHub probes do **not** exit 2: disallowed owners are filtered before probing, and probe policy failures become `github:owner_not_allowed` (or similar) residuals inside a successful view. | Reads the lease store read-only when present (missing file → empty view) and queries GitHub. The overlay is not persisted. |
| `watchlist add`, `remove` | Compatibility no-op | Human migration hint or JSON `{ "persisted": false, … }`. | `cli.watchlist.add`, `cli.watchlist.remove`; v1 / v1. | 0 | Never mutate state; use `worktree register` / `unregister`. |

For worktree, attribution, and watchlist commands, JSON dispatch errors use the
listed envelope and a structured error. Status has its dedicated v2 error path.
By contrast, `git-safe`/`gh-safe` validation failures and `install` failures print
diagnostics to stderr without an envelope. Clap syntax/help failures also occur
before command dispatch. Policy violations use exit 2; other writ operational
errors use exit 1. An executed safe subprocess may have its own nonzero status,
as described above.

## State and authority boundaries

The local coordination authority is one SQLite lease store (`leases` and
`agents`) shared by processes on the **same host**. Matching filesystem paths on
separate hosts do not make a shared store. `status`, `jobs`, `worktree list`, and
the watchlist are views of that store, not independent registries.

The harness owns checkout/worktree creation and deletion. Current writ commands
register and inspect those checkouts; the managed `create`, `remove`, and `prune`
commands remain only for compatibility. GitHub remains authoritative for remote
source, pull requests, reviews, checks, repository rules, and protected-branch
merges.

There is **no `writ coord` command in this snapshot**. Planned claim, overlap,
message, handoff, and lease-reconciliation capabilities belong to RM-825 and must
extend the existing lease store rather than introduce a second database. Until
that work lands on the inspected branch, documentation and clients must describe
those capabilities as planned, not probe or invoke them as implemented commands.

## Captured status outcomes

These compact outputs were captured from the built binary at the inspected
commit. A nonexistent `WRIT_LEASE_PATH` produced a healthy empty snapshot and
exit 0:

```json
{"ok":true,"schema_version":2,"command":"cli.status","data":{"source":"lease_store","jobs":[],"agents":[]},"error":null}
```

Pointing `WRIT_LEASE_PATH` at a directory produced exit 1, a v2 failure envelope
on stdout, and a diagnostic on stderr (the temporary path is abbreviated here):

```json
{"ok":false,"schema_version":2,"command":"cli.status","data":{"source":"lease_store","jobs":[],"agents":[]},"error":{"code":"STATE_LOAD_FAILED","message":"lease store path is not a regular file: <temporary-directory>"}}
```

The corresponding native contract tests are `crates/writ/tests/status_cli.rs`,
`crates/writ/tests/worktree_cli.rs`, and the unit tests beside the CLI dispatcher
and `writ-core` contract/status implementations.
