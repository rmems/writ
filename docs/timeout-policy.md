# Timeout policy and hang recovery

Named defaults, stuck detection, and recovery for `writ supervisor`. This is
the surviving **Rust supervisor** contract from Linear [RM-15](https://linear.app/rpd-34/issue/RM-15/worktrees-hives-gh15-subagent-timeouts-and-hang-recovery) (original GH#15).

GitHub [#15](https://github.com/rmems/writ/issues/15) closed the *worker-graph / orchestrator retry* scope as not planned. `writ` does not run worker nodes and does not merge. Task re-dispatch belongs to the host harness (Claude Code agent teams, `/batch`, Cursor `/multitask`, Codex). Process containment stays here.

Related: process supervisor [#27](https://github.com/rmems/writ/issues/27), kernel tests [#81](https://github.com/rmems/writ/issues/81), lease heartbeat [#124](https://github.com/rmems/writ/issues/124).

## Config keys and defaults

| Key | CLI | Env | Default | Meaning |
| --- | --- | --- | --- | --- |
| `worker_seconds` | `--timeout` | `WRIT_SUPERVISOR_TIMEOUT_SECS` | `0` (disabled) | Hard wall-clock for the run, **including** max-parallel permit wait |
| `step_seconds` | `--step` | — | `0` (disabled) | Optional per-step cap; child sees `min(remaining worker, step)` |
| `idle_seconds` | `--idle` (`--stall` alias) | `WRIT_SUPERVISOR_IDLE_SECS` (legacy: `WRIT_SUPERVISOR_STALL_SECS` documented in handoff) | `0` (disabled) | Hang detector: process alive but no captured stdout/stderr |
| `orchestrator_seconds` | `--orchestrator` | `0` (falls back to worker) | Pool-level wait budget |
| `grace_seconds` | `--grace` | `WRIT_SUPERVISOR_GRACE_SECS` | `5` | Soft-cancel then kill |
| `progress_secs` | `--progress-secs` | `WRIT_SUPERVISOR_PROGRESS_SECS` | `15` | Stderr ticks while waiting; `0` disables |
| `max_redispatch_per_item` | `--max-redispatch` | `WRIT_SUPERVISOR_MAX_REDISPATCH` | `1` | Harness retry budget. **Supervisor never retries**; hosts use [`RedispatchBudget`](../crates/writ-core/src/timeout_policy.rs) |

Library: [`TimeoutPolicy`](../crates/writ-core/src/timeout_policy.rs). The original `Supervisor::run(..., timeout, ...)` API is wall-clock only with `grace = 0` (immediate kill) so existing callers keep their timing.

Recommended production: set `--timeout` to a useful wall cap, `--idle` for silent hangs, keep `--grace 5`.

## Stuck detection

A run is **stuck** when any of:

1. **Hard timeout** — elapsed ≥ worker / step / orchestrator limit (`timeout_class: hard`)
2. **Idle timeout** — child alive, no captured output for `idle_seconds` (`timeout_class: idle`)
3. **Permit wait** — max-parallel queue exceeded the wall-clock budget before spawn (`timeout_class: permit_wait`, `recovery_stage: none`)
4. **Lost child** — `wait` failed; PID/handle gone without a terminal status (`timeout_class: lost_child`)
4. **Redispatch exhausted** — harness has already used `max_redispatch_per_item` extra runs (`timeout_class: redispatch_exhausted`). The supervisor never emits this class; `can_redispatch(completed_attempts, max)` is the decision helper.

Heartbeat used by this binary: last stdout/stderr byte timestamp. Hosts may also watch a status file or step events; those map onto the same four classes (multi-platform hook for [#17](https://github.com/rmems/writ/issues/17)).

## Recovery playbook

1. **Detect** hard, idle, or lost-child.
2. **Soft cancel** — Unix: `SIGTERM` to the process group. Windows: no SIGTERM group; wait `grace` then kill the direct child.
3. **Kill** — if still alive after `grace`, Unix `SIGKILL` to the group; Windows `child.kill()` (direct child only; no job object yet).
4. **State update** — JSON outcome: `timed_out`, `killed`, `error_code`, `timeout_class` (`permit_wait` when no child spawned), `recovery_stage` (`none` / `graceful_cancel` / `kill`), optional `last_output_ms`, `elapsed_ms`, `redispatch_count: 0`, `max_redispatch_per_item`. Watchlist fields: `process_state: timed_out`, `residual_blockers: ["timeout:hard"|…]`. Do **not** write a SHA unless a push was accepted by the remote. After reap, the supervisor **disarms** the process-group guard so a reused PID is not SIGKILL'd on Drop. Pipe drain is always bounded (30s when no worker deadline).
5. **Re-dispatch or residual** — harness may run the item again only while `can_redispatch` is true (default: one extra run). Otherwise mark residual and free the slot. `writ` does not re-dispatch.

If a host subagent API cannot kill: stop waiting, mark `timeout:lost_child` (or host-equivalent residual), warn the operator. Never merge, never bare `--force` / `-f`, never invent a SHA.

## Progress reporting

While waiting, ticks go to **stderr** (diagnostics), never JSON stdout:

```text
supervisor: active=1/2 elapsed=12s idle=3s step=running
```

`active` / `max_parallel` are process-local (one `writ` invocation). Steps: `permit_wait`, `running`, `recovering`, `draining`.

## Safety

Recovery only terminates the supervised child. It does not invoke `git` or `gh`. Mutating commands are still allowlisted **before spawn**; `gh pr merge` and bare `git push --force` remain blocked during a timed run.

## Fix-cap interaction

A timeout residual is **not** a successful fix attempt. `TimeoutClass::counts_toward_fix_cap` is always `false`. Do not increment `fix_count` / `fix_cycles` because a worker timed out, was idle-killed, or was lost. Redispatch budget (`max_redispatch_per_item`) is separate from the fix cap.

## Watchlist / report fields

Additive v1 fields on `JobStatus` (see [status-schema.md](status-schema.md)):

| Field | When present |
| --- | --- |
| `process_state` | `timed_out` for hang residuals |
| `timeout_class` | `hard` / `idle` / `lost_child` / `redispatch_exhausted` |
| `residual_blockers` | includes `timeout:hard` (or sibling token) |
| `redispatch_count` | completed terminal runs (harness-owned) |
| `fix_count` | successful fix attempts; unchanged on timeout |

Nothing in this repository writes `watched.json` today (`state.rs` is read-only). The fields are specified so a later lease store ([#124](https://github.com/rmems/writ/issues/124)) and reports ([#16](https://github.com/rmems/writ/issues/16)) share tokens.

## Platform mapping (#17)

| Host | Wait | Cancel | Hang detect | Kill missing? |
| --- | --- | --- | --- | --- |
| `writ supervisor` | tokio `wait` + timers | SIGTERM then SIGKILL (Unix); grace then `kill` (Windows, direct child) | last pipe byte; wall clock | Always has kill for the direct child |
| Claude Code / Cursor Task | host wait | host cancel/abort if exposed | host heartbeat or idle | Mark residual + warn; do not wait forever |
| GitHub Actions | job/`timeout-minutes` | runner cancel | GHA, not this binary | Out of scope (non-goal vs remote CI) |

## CLI

```bash
writ supervisor run --timeout 600 --idle 120 --grace 5 --progress-secs 15 -- cargo test --workspace
```
