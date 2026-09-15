# Supervisor timeout policy and hang recovery

This is the process-layer contract for **subagent timeouts and hang recovery**
(Linear [RM-129](https://linear.app/rpd-34/issue/RM-129/subagent-timeouts-and-hang-recovery),
historically GitHub #15). It applies to `writ supervisor run` and
`writ_core::supervisor::Supervisor`.

Hive **worker-node redispatch** is not implemented here. Under the product
pivot in GitHub #1, task lifecycle belongs to the host harness. `writ`
contains the child, reports a residual, and records the host redispatch cap.
It never merges, never bare-force-pushes, and never invents a commit SHA.

## Named defaults and config keys

| Name | CLI | Env | Default | Meaning |
| --- | --- | --- | --- | --- |
| `unlimited` | `--timeout` | `WRIT_SUPERVISOR_TIMEOUT_SECS` | `0` | Wall-clock seconds including permit wait. `0` = no deadline. |
| `worker_default` | (host) | same key | `1800` | Recommended host value for dispatched workers. |
| `gate_default` | (host) | same key | `600` | Recommended host value for `cargo test` / clippy / fmt. |
| `grace` | `--grace` | `WRIT_SUPERVISOR_GRACE_SECS` | `5` | Unix SIGTERM wait before SIGKILL. `0` skips graceful cancel. |
| `stall` | `--stall` | `WRIT_SUPERVISOR_STALL_SECS` | `0` | Seconds without child stdout/stderr. `0` = wall-clock only. |
| `progress` | `--progress` | `WRIT_SUPERVISOR_PROGRESS_SECS` | `10` | Heartbeat interval while waiting. `0` disables. |
| `max_redispatch_per_item` | `--max-redispatch` | `WRIT_SUPERVISOR_MAX_REDISPATCH` | `1` | Host retry cap after a residual. `0` = never redispatch. |

Constants live in `writ_core::timeout_policy`
(`TIMEOUT_SECS_UNLIMITED`, `TIMEOUT_SECS_WORKER`, `TIMEOUT_SECS_GATE`,
`GRACE_SECS_DEFAULT`, `STALL_SECS_DEFAULT`, `PROGRESS_SECS_DEFAULT`,
`MAX_REDISPATCH_PER_ITEM_DEFAULT`).

Library `TimeoutPolicy::default()` keeps historical kill-immediately
behaviour (`grace = 0`, stall/progress off) so in-process callers do not
grow a five-second recovery tail. The CLI uses the named table above.

## Stuck detection

A child is stuck when **any** of these fire:

1. **Wall-clock** — `--timeout` / `WRIT_SUPERVISOR_TIMEOUT_SECS` elapsed since
   `run` started, including time spent waiting for a process-local
   `--max-parallel` permit (`StuckReason::wall_clock` or `permit_wait`).
2. **Stall** — optional, off by default. No captured stdout/stderr bytes for
   `--stall` seconds while the child is still running (`StuckReason::stall`).
   Silent-but-healthy compiles should leave stall disabled or set it above
   the longest expected quiet period.

Wall-clock remains the primary bound. Stall is the beyond-naive detector.

## Recovery playbook

1. **Graceful cancel** (Unix only): `SIGTERM` to the process group.
2. **Kill**: if the child is still alive after `--grace`, `SIGKILL` the
   process group (Unix) or `child.kill()` (Windows). `--grace 0` skips
   step 1.
3. **Re-dispatch or residual** (host): `writ` does **not** respawn the
   command. The residual records `redispatch_count: 0` for this invocation
   and `max_redispatch_per_item` from config. The host calls
   `RedispatchBudget::try_acquire` before a retry. When the budget is
   exhausted, mark the item residual, free the slot, and stop.
4. **Safe cleanup**: process-group kill on Unix Drop; pipes are drained
   with a bounded join. Worktree paths are not deleted by recovery.

Safety cross-checks, enforced in `writ-core` on every attempt:

- Recovery never runs `gh pr merge` or any other merge path.
- Recovery never runs bare `git push --force` / `-f`.
- Timeout residuals have no `sha`, `commit`, or `head` fields, so they
  cannot be posted as fake verified-commit replies.

## Progress reporting

While the supervisor waits (permit queue or running child) it emits
diagnostics on **stderr** (never stdout):

```text
writ supervisor: elapsed=5s remaining=25s last_output=none wait=child active=1
```

Library callers receive the same events through `RunOptions::on_progress`.
JSON `--json` envelopes stay on stdout; heartbeats stay on stderr.

## Watchlist / report fields

Additive v1 `JobStatus.timeout_residual` (see [`status-schema.md`](status-schema.md)):

| Field | Type | Meaning |
| --- | --- | --- |
| `reason` | `wall_clock` \| `stall` \| `permit_wait` | Stuck criterion |
| `recovery_stage` | `none` \| `graceful_cancel` \| `kill` | Last supervisor action |
| `redispatch_count` | `u32` | Host retries already consumed |
| `max_redispatch_per_item` | `u32` | Configured cap |
| `elapsed_ms` | `u64` | Wall time until residual |
| `last_output_ms` | `u64?` | Last child byte, if any |

`process_state` stays `failed` (timeout/stall) or `cancelled` (operator).
`last_error` should name the reason, for example `timed out: wall_clock`.
Nothing in this repository writes `watched.json`; hosts persist the field.

The same object is `SupervisedOutput.residual` on `writ supervisor run`.

## Platform mapping (hook for GitHub #17)

| Step | Unix | Windows | Hosts without child-kill |
| --- | --- | --- | --- |
| Wait | `select` on child + deadline + stall poll + progress | same | poll the platform's run handle |
| Graceful cancel | `kill(-pid, SIGTERM)` after `process_group(0)` | **unsupported** — skip to kill | request the platform cancel API |
| Kill | `kill(-pid, SIGKILL)` + `child.kill()` | direct child only (`kill_on_drop`); no job object | mark residual and free the slot |
| Progress | stderr / `on_progress` | same | host progress channel |
| Fallback if kill is missing | n/a | grandchildren may leak (documented limitation) | do not fake success or a SHA; record `recovery_stage: none` |

Each host maps this playbook onto native cancellation (Claude Code
subagent abort, Cursor task cancel, Codex interrupt). If the platform
cannot kill, the host still emits a residual and must not claim the work
finished.

`--max-parallel` remains **process-local**. Cross-process throttling is
tracked with the lease store in GitHub #124.
