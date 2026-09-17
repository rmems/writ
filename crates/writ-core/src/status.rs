//! Status and job-query JSON types for Python and agent consumption.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::contract::Response;
use crate::timeout_policy::TimeoutResidual;

/// Lifecycle state of a watched job process.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Job has been created but not yet started.
    Pending,
    /// Job is actively running.
    Running,
    /// Job finished successfully.
    Completed,
    /// Job terminated with an error.
    Failed,
    /// Job was cancelled by the operator.
    Cancelled,
}

/// CI check-run classification for a job's head commit.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiClass {
    /// All required CI checks passed.
    Pass,
    /// At least one required check failed.
    Fail,
    /// Checks are queued or in progress.
    Pending,
    /// CI status could not be determined.
    Unknown,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        })
    }
}

impl fmt::Display for CiClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        })
    }
}

/// Status of a single watched job.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobStatus {
    /// Unique job identifier (e.g. `writ-347`).
    pub job_id: String,
    /// Repository owner (e.g. `acme`).
    pub owner: String,
    /// Repository name (e.g. `example-org`).
    pub repo: String,
    /// Linked issue number, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,
    /// Linked pull-request number, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    /// Absolute path to the job's isolated worktree.
    pub worktree_path: String,
    /// Current branch checked out in the worktree.
    pub branch: String,
    /// Lifecycle state of the job process.
    pub process_state: ProcessState,
    /// Last error message, if the job is in `Failed` state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Timeout / hang residual when the job stopped because a worker was stuck.
    ///
    /// Specified for watchlist / aggregate reports (GitHub #12 / #16). Hosts
    /// persist this additive v1 field; `writ` has no state writer today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_residual: Option<TimeoutResidual>,
    /// CI classification for the job's head commit.
    pub ci_class: CiClass,
}

/// Payload for `writ status` and `writ jobs` v1 envelope responses.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobsData {
    /// List of job statuses (may be empty when no jobs are watched).
    pub jobs: Vec<JobStatus>,
}

/// Named envelope type for `writ status --json` / `writ jobs --json` responses.
pub type StatusReport = Response<JobsData>;

/// Build a successful v1 envelope response for the given command and job list.
///
/// The returned value serializes as `{ ok, schema_version, command, data: { jobs }, error }`,
/// matching the shared envelope contract defined in [`crate::contract::Response`].
#[must_use]
pub fn status_response(command: &'static str, jobs: Vec<JobStatus>) -> StatusReport {
    Response::success(command, JobsData { jobs })
}

/// Build a failure v1 envelope when watched state cannot be loaded.
///
/// Serializes as `{ ok: false, schema_version, command, data: { jobs: [] }, error: { code, message } }`.
#[must_use]
pub fn status_error(command: &'static str, message: String) -> StatusReport {
    Response {
        ok: false,
        schema_version: crate::contract::SCHEMA_VERSION,
        command,
        data: JobsData { jobs: Vec::new() },
        error: Some(crate::contract::ErrorData {
            code: "STATE_LOAD_FAILED".to_owned(),
            message,
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::contract::SCHEMA_VERSION;
    use crate::test_support::sample_job;

    #[test]
    fn job_status_serializes_with_optional_fields_present() {
        let job = JobStatus {
            last_error: Some("task failed: permission denied".to_owned()),
            ..sample_job()
        };
        let v = serde_json::to_value(&job).unwrap();

        // A single subset comparison checks every populated field at once
        // (job_id, process_state, ci_class, issue_number, pr_number,
        // last_error) without a large assertion block.
        assert_eq!(
            v,
            json!({
                "job_id": "writ-100",
                "owner": "acme",
                "repo": "example-org",
                "issue_number": 29,
                "pr_number": 42,
                "worktree_path": "/tmp/worktrees/acme/example-org/writ-100",
                "branch": "feature/status-json-cli",
                "process_state": "running",
                "last_error": "task failed: permission denied",
                "ci_class": "pending",
            })
        );
    }

    #[test]
    fn job_status_omits_none_fields() {
        let job = JobStatus {
            issue_number: None,
            pr_number: None,
            last_error: None,
            ..sample_job()
        };
        let v = serde_json::to_value(&job).unwrap();

        // Exact-match comparison: the expected object omits issue_number,
        // pr_number, last_error, and timeout_residual, so any of those keys
        // leaking into the output fails this single assertion.
        assert_eq!(
            v,
            json!({
                "job_id": "writ-100",
                "owner": "acme",
                "repo": "example-org",
                "worktree_path": "/tmp/worktrees/acme/example-org/writ-100",
                "branch": "feature/status-json-cli",
                "process_state": "running",
                "ci_class": "pending",
            })
        );
    }

    #[test]
    fn timeout_residual_serializes_on_failed_job() {
        use crate::timeout_policy::{RecoveryStage, StuckReason, TimeoutResidual};
        let job = JobStatus {
            process_state: ProcessState::Failed,
            last_error: Some("timed out: wall_clock".to_owned()),
            timeout_residual: Some(TimeoutResidual {
                reason: StuckReason::WallClock,
                recovery_stage: RecoveryStage::Kill,
                redispatch_count: 1,
                max_redispatch_per_item: 1,
                elapsed_ms: 1_800_000,
                last_output_ms: None,
            }),
            ..sample_job()
        };
        let v = serde_json::to_value(&job).unwrap();

        // Exact-match of the residual object verifies reason, recovery_stage,
        // and max_redispatch_per_item, and (being an exact match) also proves
        // no `sha` key is emitted.
        assert_eq!(
            v["timeout_residual"],
            json!({
                "reason": "wall_clock",
                "recovery_stage": "kill",
                "redispatch_count": 1,
                "max_redispatch_per_item": 1,
                "elapsed_ms": 1_800_000,
            })
        );
    }

    #[test]
    fn status_response_uses_v1_envelope() {
        let response = status_response("cli.status", vec![sample_job()]);
        let v = serde_json::to_value(&response).unwrap();

        // One exact-match comparison verifies the whole envelope: the
        // schema_version / command / ok / explicit-null error keys, that jobs
        // live under `data` (not at the top level), and that exactly one job
        // is present.
        assert_eq!(
            v,
            json!({
                "ok": true,
                "schema_version": SCHEMA_VERSION,
                "command": "cli.status",
                "data": { "jobs": [serde_json::to_value(sample_job()).unwrap()] },
                "error": null,
            })
        );
    }

    #[test]
    fn empty_response_has_zero_jobs() {
        let response = status_response("cli.jobs", Vec::new());
        assert!(response.data.jobs.is_empty());
        assert!(response.ok);
    }

    #[test]
    fn status_error_sets_ok_false_and_error_payload() {
        let response = status_error("cli.status", "parse failed".to_owned());
        let v = serde_json::to_value(&response).unwrap();

        // Exact-match verifies ok=false, command, the error code/message
        // payload, and an empty jobs list in a single assertion.
        assert_eq!(
            v,
            json!({
                "ok": false,
                "schema_version": SCHEMA_VERSION,
                "command": "cli.status",
                "data": { "jobs": [] },
                "error": { "code": "STATE_LOAD_FAILED", "message": "parse failed" },
            })
        );
    }

    #[test]
    fn roundtrip_through_json() {
        let response = status_response("cli.status", vec![sample_job()]);
        // Round-trip through a JSON string and back into a typed value.
        let json = serde_json::to_string(&response).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Single subset comparison covers the command plus the single job's
        // identifying fields (job_id, branch) surviving the round-trip.
        assert_eq!(
            v,
            json!({
                "ok": true,
                "schema_version": SCHEMA_VERSION,
                "command": "cli.status",
                "data": { "jobs": [serde_json::to_value(sample_job()).unwrap()] },
                "error": null,
            })
        );
    }

    #[test]
    fn process_state_variants_serialize_correctly() {
        let cases = [
            (ProcessState::Pending, "\"pending\""),
            (ProcessState::Running, "\"running\""),
            (ProcessState::Completed, "\"completed\""),
            (ProcessState::Failed, "\"failed\""),
            (ProcessState::Cancelled, "\"cancelled\""),
        ];
        for (state, expected) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), expected);
        }
    }

    #[test]
    fn ci_class_variants_serialize_correctly() {
        let cases = [
            (CiClass::Pass, "\"pass\""),
            (CiClass::Fail, "\"fail\""),
            (CiClass::Pending, "\"pending\""),
            (CiClass::Unknown, "\"unknown\""),
        ];
        for (class, expected) in cases {
            assert_eq!(serde_json::to_string(&class).unwrap(), expected);
        }
    }
}
