# Status JSON Schema

> [!WARNING]
> **Both commands return an empty `jobs` array unless something outside this workspace supplies the state file.** `crates/writ-core/src/state.rs` has a read path and no writer, so nothing here creates `watched.json`, and with the file absent -- the normal case -- the output is empty. Verified:
>
> ```console
> $ writ --json status
> {"ok":true,"schema_version":1,"command":"cli.status","data":{"jobs":[]},"error":null}
> ```
>
> The populated shapes below are still reachable, so consumers must handle them: `load_jobs_from` returns an empty vec only when the file is missing, and parses and returns any file that *is* present -- placed there externally, or pointed at via `WRIT_STATE_PATH`. What is missing is the writer, not the read path.
>
> The store is superseded rather than unfinished: the SQLite lease store in [#124](https://github.com/rmems/writ/issues/124) replaces it, with crash consistency in [#136](https://github.com/rmems/writ/issues/136).

This document defines the JSON schema emitted by `writ status --json` and `writ jobs --json`.

## Envelope

All status responses use the shared v1 envelope defined in the `Response<T>` type (`crates/writ-core/src/contract.rs`):

| Field | Type | Description |
| --- | --- | --- |
| `ok` | `bool` | `true` when the query succeeded. |
| `schema_version` | `u8` | Always `1` for the current contract. |
| `command` | `string` | Machine-readable command identifier (`cli.status` or `cli.jobs`). |
| `data` | `object` | Command payload — contains a `jobs` array (see below). |
| `error` | `object \| null` | Structured error payload; always `null` on success. |

The `data` object for status commands has one field:

| Field | Type | Description |
| --- | --- | --- |
| `jobs` | `JobStatus[]` | Array of job status objects (may be empty). |

## Failure contract (state load errors)

When the watched state file exists but cannot be read or parsed:

1. **Stdout** still receives a full v1 envelope with `ok: false`, `data.jobs: []`, and
   `error: { "code": "STATE_LOAD_FAILED", "message": "..." }`.
2. **Process exit code is non-zero** (the CLI exits with failure after writing the envelope).
3. Human mode (no `--json`) prints the error to **stderr** and exits non-zero; it does **not**
   print a healthy empty summary such as `No watched jobs.`.

Consumers that use `subprocess.run(..., check=True)` will raise `CalledProcessError` on load
failure. The failure envelope is still available on `exc.stdout` — always parse stdout even when
the exit code is non-zero.

Missing state file is **not** an error: it yields `ok: true` with an empty `jobs` array and exit 0.

## `JobStatus` object

Each entry in the `jobs` array has these fields:

| Field | Type | Nullable | Description |
| --- | --- | --- | --- |
| `job_id` | `string` | no | Unique job identifier (e.g. `writ-347`). |
| `owner` | `string` | no | Repository owner (e.g. `acme`). |
| `repo` | `string` | no | Repository name (e.g. `example-org`). |
| `issue_number` | `u64` | yes | Linked GitHub issue number. Omitted when absent. |
| `pr_number` | `u64` | yes | Linked pull-request number. Omitted when absent. |
| `worktree_path` | `string` | no | Absolute path to the job's isolated worktree. |
| `branch` | `string` | no | Current branch checked out in the worktree. |
| `process_state` | `ProcessState` | no | Lifecycle state of the job process. |
| `last_error` | `string` | yes | Last error message if the job failed. Omitted when absent. |
| `ci_class` | `CiClass` | no | CI classification for the job's head commit. |

## `ProcessState` enum

Serialized as a lowercase snake_case string.

| Variant | Meaning |
| --- | --- |
| `pending` | Job created but not yet started. |
| `running` | Job is actively running. |
| `completed` | Job finished successfully. |
| `failed` | Job terminated with an error. |
| `cancelled` | Job was cancelled by the operator. |

## `CiClass` enum

Serialized as a lowercase snake_case string.

| Variant | Meaning |
| --- | --- |
| `pass` | All required CI checks passed. |
| `fail` | At least one required check failed. |
| `pending` | Checks are queued or in progress. |
| `unknown` | CI status could not be determined. |

## Example: empty report

```json
{
  "ok": true,
  "schema_version": 1,
  "command": "cli.status",
  "data": {
    "jobs": []
  },
  "error": null
}
```

## Example: report with jobs

```json
{
  "ok": true,
  "schema_version": 1,
  "command": "cli.status",
  "data": {
    "jobs": [
      {
        "job_id": "writ-100",
        "owner": "acme",
        "repo": "example-org",
        "issue_number": 29,
        "pr_number": 42,
        "worktree_path": "/home/user/.local/share/writ/worktrees/acme/example-org/writ-100",
        "branch": "feature/status-json-cli",
        "process_state": "running",
        "ci_class": "pending"
      },
      {
        "job_id": "writ-101",
        "owner": "acme",
        "repo": "example-org",
        "issue_number": 30,
        "worktree_path": "/home/user/.local/share/writ/worktrees/acme/example-org/writ-101",
        "branch": "feature/python-bridge",
        "process_state": "failed",
        "last_error": "git command failed (`git push`): permission denied",
        "ci_class": "unknown"
      }
    ]
  },
  "error": null
}
```

## Example: state load failure

```json
{
  "ok": false,
  "schema_version": 1,
  "command": "cli.status",
  "data": {
    "jobs": []
  },
  "error": {
    "code": "STATE_LOAD_FAILED",
    "message": "failed to parse /path/to/watched.json: expected value at line 1 column 1"
  }
}
```

## Versioning

The `schema_version` field is backward-compatible. Additive fields will be introduced within schema version `1`. Removals or semantic renames require a version bump. Consumers should ignore unknown fields.

## Commands

| Command | Description |
| --- | --- |
| `writ status --json` | Emit a status report for all watched jobs. |
| `writ jobs --json` | Alias for `writ status --json`. |

Without `--json`, both commands print a brief human-readable summary to standard output.

## Python consumption example

Prefer `check=False` so a non-zero exit (state load failure) still leaves the failure envelope on stdout for inspection:

```python
import json
import subprocess
import sys

result = subprocess.run(
    ["writ", "status", "--json"],
    capture_output=True,
    text=True,
    check=False,  # load failures exit non-zero but still write a v1 envelope
)
if not result.stdout.strip():
    print(result.stderr or "writ status produced no output", file=sys.stderr)
    sys.exit(result.returncode or 1)

report = json.loads(result.stdout)
assert report["schema_version"] == 1

if not report["ok"] or result.returncode != 0:
    err = report.get("error") or {}
    print(f"status failed: {err.get('code')}: {err.get('message')}", file=sys.stderr)
    sys.exit(result.returncode or 1)

for job in report["data"]["jobs"]:
    print(f"{job['job_id']}: {job['process_state']}")
```

Equivalent pattern when catching `CalledProcessError` (if you keep `check=True`):

```python
import json
import subprocess

try:
    result = subprocess.run(
        ["writ", "status", "--json"],
        capture_output=True,
        text=True,
        check=True,
    )
    report = json.loads(result.stdout)
except subprocess.CalledProcessError as exc:
    # Failure envelope is on stdout even when exit code is non-zero.
    report = json.loads(exc.stdout)
    assert report["ok"] is False
    assert report["error"]["code"] == "STATE_LOAD_FAILED"
    raise
```
