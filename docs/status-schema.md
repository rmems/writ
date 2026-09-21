# Status JSON Schema

`writ status --json` and `writ jobs --json` report **local collaboration state**
from the SQLite lease store (`leases` + `agents`). Human output is formatted from
the same snapshot. This is not a GitHub PR/check/merge gate and does not revive
the deleted Python orchestrator or `watched.json`.

Missing lease-store files are **not** an error and are **not** created: the
command returns `ok: true` with empty `jobs` and `agents` arrays.

## Envelope

All status responses use the shared v1 envelope (`crates/writ-core/src/contract.rs`):

| Field | Type | Description |
| --- | --- | --- |
| `ok` | `bool` | `true` when the query succeeded. |
| `schema_version` | `u8` | Always `1` for the current contract. |
| `command` | `string` | `cli.status` or `cli.jobs`. |
| `data` | `object` | Snapshot payload (see below). |
| `error` | `object \| null` | Structured error payload; `null` on success. |

## `data` object

| Field | Type | Description |
| --- | --- | --- |
| `source` | `string` | Always `lease_store`. |
| `jobs` | `JobStatus[]` | Lease identities, including released (`UNASSIGNED`) rows. |
| `agents` | `AgentStatus[]` | Agent-registry rows from the same database. |

Additive fields within schema version `1` are compatible. Removals or semantic
renames require a version bump. Consumers should ignore unknown fields and must
treat JSON `null` as unknown rather than inferring a value.

## Failure contract

When the lease store cannot be opened or queried:

1. **Stdout** still receives a v1 envelope with `ok: false`, empty `jobs` /
   `agents`, `source: "lease_store"`, and
   `error: { "code": "STATE_LOAD_FAILED", "message": "..." }`.
2. **Process exit code is non-zero**.
3. Human mode prints the error to **stderr** and does **not** print
   `No collaboration participants.`

## `JobStatus` object

| Field | Type | Nullable | Description |
| --- | --- | --- | --- |
| `job_id` | `string` | no | Coordination job id. |
| `owner` | `string` | no | Repository owner recorded on the lease. |
| `repo` | `string` | no | Repository name recorded on the lease. |
| `issue_number` | `u64` | yes | Omitted when unknown (not stored on the lease). |
| `pr_number` | `u64` | yes | Omitted when unknown (not stored on the lease). |
| `worktree_path` | `string` | no | Registered checkout path. |
| `branch` | `string` | no | Branch recorded on the lease. |
| `process_state` | `ProcessState` | no | Lease-backed rows are `unknown`. |
| `last_error` | `string \| null` | yes | Always present; `null` when none is recorded. |
| `ci_class` | `CiClass` | no | Lease-backed rows are `unknown`. GitHub CI is not queried. |
| `timeout_class` | `TimeoutClass` | yes | Stuck class when `process_state` is `timed_out`. Omitted when absent. Values: `hard`, `idle`, `lost_child`, `permit_wait`, `redispatch_exhausted`. |
| `recovery_stage` | `RecoveryStage` | yes | Last supervisor action: `none`, `graceful_cancel`, `kill`. Never merge/push. |
| `last_output_ms` | `u64` | yes | Last captured child byte, if any. |
| `max_redispatch_per_item` | `u32` | yes | Host retry cap recorded on the residual. `0` means never redispatch. |
| `residual_blockers` | `string[]` | yes | Structured leftovers (e.g. `timeout:hard`). Omitted when empty. |
| `redispatch_count` | `u32` | yes | Completed terminal runs (harness-owned). Supervisor runs always report `0` internally. |
| `fix_count` | `u32` | yes | Successful fix attempts. Timeout residuals must not increment this. |
| `collaboration_state` | `CollaborationState` | no | Mapped only from recorded `lease_mode`. |
| `lease_mode` | `string \| null` | yes | Raw mode (`WRITER_LOCKED`, `UNASSIGNED`, \u2026). |
| `head` | `string \| null` | yes | Live checkout `HEAD` when inspect succeeds, else lease `start_commit`. |
| `head_source` | `string \| null` | yes | `checkout` or `lease`. |
| `lease_row_id` | `i64 \| null` | yes | SQLite row id. |
| `updated_at` | `i64 \| null` | yes | Lease `updated_at` unix seconds. |
| `ownership_generation` | `i64 \| null` | yes | Not stored today; always `null`. |
| `agent_id` | `string \| null` | yes | Not joined on the lease; always `null`. |
| `session_id` | `string \| null` | yes | Not joined on the lease; always `null`. |
| `declared_paths` | `string[] \| null` | yes | Not stored today; always `null`. |
| `blocker` | `string \| null` | yes | Not stored today; always `null`. |
| `handoff` | `string \| null` | yes | Not stored today; always `null`. |
| `recovery_needed` | `bool \| null` | yes | `true` when an **active** lease cannot inspect its checkout; `false` when inspect succeeds. |

## `CollaborationState`

Serialized as lowercase snake_case. These are **local** coordination values.
Released or ready-for-integration is **not** GitHub merged/completed.

| Variant | Lease mode | Meaning |
| --- | --- | --- |
| `running` | `WRITER_LOCKED` | Active writer lock. |
| `waiting` | `NEEDS_HUMAN` | Recorded as needing a human. |
| `paused` | `REVIEW_ONLY` | Review-only / paused writer. |
| `conflicted` | `BLOCKED` | Recorded blocked. |
| `ready_for_integration` | `MERGE_READY` | Locally marked ready to integrate. |
| `unassigned` | `UNASSIGNED` | Identity kept after release. |
| `unknown` | unrecognized / missing | No recorded mapping. |

Phase 1 currently **writes** only `WRITER_LOCKED` (register/grant) and
`UNASSIGNED` (unregister/release). Other modes are reported if present.

## `ProcessState` / `CiClass`

Kept for envelope compatibility. Lease-backed status does not invent process or
CI outcomes; both serialize as `unknown`.

`ProcessState` still accepts `pending`, `running`, `completed`, `failed`,
`cancelled`, and `timed_out` when deserializing legacy JSON (`timed_out` marks a
supervisor hang residual; see `timeout_class`). Lease-backed rows emit
`unknown`; `completed` is never emitted for a released or merge-ready lease.

## `AgentStatus` object

| Field | Type | Description |
| --- | --- | --- |
| `agent_id` | `string` | Registry id. |
| `agent_type` | `string` | Recorded type. |
| `session_id` | `string \| null` | Session when recorded. |
| `started_at` | `i64` | Unix seconds. |
| `stopped_at` | `i64 \| null` | Set when the agent was retired. |

Agents are listed separately because the store does not join them to lease rows.

## Example: empty report

```json
{
  "ok": true,
  "schema_version": 1,
  "command": "cli.status",
  "data": {
    "source": "lease_store",
    "jobs": [],
    "agents": []
  },
  "error": null
}
```

## Example: two local participants

```json
{
  "ok": true,
  "schema_version": 1,
  "command": "cli.status",
  "data": {
    "source": "lease_store",
    "jobs": [
      {
        "job_id": "job-a",
        "owner": "acme",
        "repo": "sample",
        "worktree_path": "/tmp/job-a",
        "branch": "hive/a",
        "process_state": "unknown",
        "last_error": null,
        "ci_class": "unknown",
        "collaboration_state": "running",
        "lease_mode": "WRITER_LOCKED",
        "head": "abc123",
        "head_source": "checkout",
        "lease_row_id": 1,
        "updated_at": 1710000000,
        "ownership_generation": null,
        "agent_id": null,
        "session_id": null,
        "declared_paths": null,
        "blocker": null,
        "handoff": null,
        "recovery_needed": false
      },
      {
        "job_id": "job-b",
        "owner": "acme",
        "repo": "sample",
        "worktree_path": "/tmp/job-b",
        "branch": "hive/b",
        "process_state": "unknown",
        "last_error": null,
        "ci_class": "unknown",
        "collaboration_state": "unassigned",
        "lease_mode": "UNASSIGNED",
        "head": "abc123",
        "head_source": "lease",
        "lease_row_id": 2,
        "updated_at": 1710000001,
        "ownership_generation": null,
        "agent_id": null,
        "session_id": null,
        "declared_paths": null,
        "blocker": null,
        "handoff": null,
        "recovery_needed": false
      }
    ],
    "agents": [
      {
        "agent_id": "agent-a",
        "agent_type": "Worker",
        "session_id": "session-a",
        "started_at": 1710000000,
        "stopped_at": null
      }
    ]
  },
  "error": null
}
```

## Versioning

The `schema_version` field is backward-compatible. Additive fields are introduced
within version `1`. Removals or semantic renames require a version bump.

## Commands

| Command | Description |
| --- | --- |
| `writ status --json` | Emit the collaboration snapshot. |
| `writ jobs --json` | Alias for `writ status --json`. |

Without `--json`, both commands print a human summary of the same snapshot.
