//! Status and job-query JSON types for Python and agent consumption.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::contract::Response;

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
    use super::*;
    use crate::contract::SCHEMA_VERSION;

    fn sample_job() -> JobStatus {
        JobStatus {
            job_id: "writ-100".to_owned(),
            owner: "acme".to_owned(),
            repo: "example-org".to_owned(),
            issue_number: Some(29),
            pr_number: Some(42),
            worktree_path: "/tmp/worktrees/acme/example-org/writ-100".to_owned(),
            branch: "feature/status-json-cli".to_owned(),
            process_state: ProcessState::Running,
            last_error: None,
            ci_class: CiClass::Pending,
        }
    }

    #[test]
    fn job_status_serializes_with_optional_fields_present() {
        let job = JobStatus {
            last_error: Some("task failed: permission denied".to_owned()),
            ..sample_job()
        };
        let json = serde_json::to_string(&job).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v.get("job_id").expect("missing job_id"), "writ-100");
        assert_eq!(
            v.get("process_state").expect("missing process_state"),
            "running"
        );
        assert_eq!(v.get("ci_class").expect("missing ci_class"), "pending");
        assert_eq!(v.get("issue_number").expect("missing issue_number"), 29);
        assert_eq!(v.get("pr_number").expect("missing pr_number"), 42);
        assert_eq!(
            v.get("last_error").expect("missing last_error"),
            "task failed: permission denied"
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
        let json = serde_json::to_string(&job).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(v.get("issue_number").is_none());
        assert!(v.get("pr_number").is_none());
        assert!(v.get("last_error").is_none());
    }

    #[test]
    fn status_response_uses_v1_envelope() {
        let response = status_response("cli.status", vec![sample_job()]);
        let json = serde_json::to_string(&response).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify envelope keys exist (not Null from missing keys).
        assert_eq!(
            v.get("schema_version").expect("missing schema_version"),
            SCHEMA_VERSION
        );
        assert_eq!(v.get("command").expect("missing command"), "cli.status");
        assert!(v.get("ok").expect("missing ok").as_bool().unwrap());
        assert!(
            v.get("error").expect("missing error").is_null(),
            "error must be explicitly null, not absent"
        );
        // Verify jobs live under data, not at top level.
        assert!(v.get("jobs").is_none(), "jobs must not be at top level");
        let data = v.get("data").expect("missing data");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert_eq!(jobs.len(), 1);
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
        let json = serde_json::to_string(&response).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v.get("ok").expect("missing ok"), false);
        assert_eq!(v.get("command").expect("missing command"), "cli.status");
        let err = v.get("error").expect("missing error");
        assert_eq!(err.get("code").expect("missing code"), "STATE_LOAD_FAILED");
        assert_eq!(err.get("message").expect("missing message"), "parse failed");
        let jobs = v
            .get("data")
            .expect("missing data")
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be array");
        assert!(jobs.is_empty());
    }

    #[test]
    fn roundtrip_through_json() {
        let response = status_response("cli.status", vec![sample_job()]);
        let json = serde_json::to_string(&response).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v.get("command").expect("missing command"), "cli.status");
        let data = v.get("data").expect("missing data");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].get("job_id").expect("missing job_id"), "writ-100");
        assert_eq!(
            jobs[0].get("branch").expect("missing branch"),
            "feature/status-json-cli"
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
