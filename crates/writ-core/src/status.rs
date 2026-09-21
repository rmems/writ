//! Status and job-query JSON types for agents and humans.
//!
//! The authority is the SQLite lease store (`leases` + `agents`). Unknown
//! collaboration fields stay `null` rather than being inferred. GitHub CI/PR
//! merge state is not mixed into this snapshot.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::checkout::inspect_checkout;
use crate::contract::Response;
use crate::lease::{AgentRecord, Lease, LeaseMode, LeaseStore};
use crate::paths::lease_store_path;
use crate::timeout_policy::{RecoveryStage, TimeoutClass};

/// Lifecycle state of a watched job process.
///
/// Lease-backed status reports [`Unknown`]: process lifecycle is not stored
/// in the coordination database.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
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
    /// Job hit a supervisor timeout / hang residual (see `timeout_class`).
    TimedOut,
    /// Process lifecycle is not recorded in the current store.
    #[default]
    Unknown,
}

/// Local collaboration state derived only from a recorded lease mode.
///
/// Released / paused / conflicted / ready-for-integration are local
/// coordination values. They are never treated as GitHub merged/completed.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationState {
    /// Held writer lock (`WRITER_LOCKED`).
    Running,
    /// Recorded as needing a human (`NEEDS_HUMAN`).
    Waiting,
    /// Review-only / paused writer (`REVIEW_ONLY`).
    Paused,
    /// Recorded blocked / conflicted (`BLOCKED`).
    Conflicted,
    /// Locally marked ready for integration (`MERGE_READY`).
    ReadyForIntegration,
    /// Identity kept after release (`UNASSIGNED`). Not GitHub-completed.
    Unassigned,
    /// Mode is missing or not mapped.
    #[default]
    Unknown,
}

/// CI check-run classification for a job's head commit.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiClass {
    /// All required CI checks passed.
    Pass,
    /// At least one required check failed.
    Fail,
    /// Checks are queued or in progress.
    Pending,
    /// CI status could not be determined.
    #[default]
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
            Self::TimedOut => "timed_out",
            Self::Unknown => "unknown",
        })
    }
}

impl fmt::Display for CollaborationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Conflicted => "conflicted",
            Self::ReadyForIntegration => "ready_for_integration",
            Self::Unassigned => "unassigned",
            Self::Unknown => "unknown",
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

/// Status of a single registered checkout / lease identity.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
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
    /// Absolute path to the registered checkout.
    pub worktree_path: String,
    /// Branch recorded on the lease (coordination identity).
    pub branch: String,
    /// Process lifecycle. Lease-backed rows are `unknown`.
    #[serde(default)]
    pub process_state: ProcessState,
    /// Last error message when one is recorded.
    #[serde(default)]
    pub last_error: Option<String>,
    /// CI classification. Lease-backed rows are `unknown`.
    #[serde(default)]
    pub ci_class: CiClass,
    /// Stuck class when `process_state` is `timed_out` (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_class: Option<TimeoutClass>,
    /// Structured leftovers (`timeout:hard`, CI codes, …). Empty omitted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residual_blockers: Vec<String>,
    /// Completed terminal runs of this item (harness-owned). Supervisor does not increment this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redispatch_count: Option<u32>,
    /// Successful fix attempts. Timeout residuals must not increment this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_count: Option<u32>,
    /// Last supervisor recovery action for a hang residual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_stage: Option<RecoveryStage>,
    /// Milliseconds from spawn to last captured child byte, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_ms: Option<u64>,
    /// Host redispatch cap recorded on the residual. Supervisor never consumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_redispatch_per_item: Option<u32>,
    /// Local collaboration state mapped from lease mode only.
    #[serde(default)]
    pub collaboration_state: CollaborationState,
    /// Raw lease mode string (`WRITER_LOCKED`, `UNASSIGNED`, …).
    #[serde(default)]
    pub lease_mode: Option<String>,
    /// HEAD commit when known.
    #[serde(default)]
    pub head: Option<String>,
    /// `checkout` when live inspect succeeded, otherwise `lease`.
    #[serde(default)]
    pub head_source: Option<String>,
    /// SQLite lease row id.
    #[serde(default)]
    pub lease_row_id: Option<i64>,
    /// Lease `updated_at` unix seconds.
    #[serde(default)]
    pub updated_at: Option<i64>,
    /// Ownership generation. Not stored today; always `null`.
    #[serde(default)]
    pub ownership_generation: Option<i64>,
    /// Agent id joined to this job. Not stored on the lease; always `null`.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Session id joined to this job. Not stored on the lease; always `null`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Declared path scopes. Not stored today; always `null`.
    #[serde(default)]
    pub declared_paths: Option<Vec<String>>,
    /// Latest blocker. Not stored today; always `null`.
    #[serde(default)]
    pub blocker: Option<String>,
    /// Latest handoff. Not stored today; always `null`.
    #[serde(default)]
    pub handoff: Option<String>,
    /// `true` when an active lease cannot inspect its checkout.
    #[serde(default)]
    pub recovery_needed: Option<bool>,
}

/// One agent-registry row from the same lease store.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub agent_id: String,
    pub agent_type: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub stopped_at: Option<i64>,
}

/// Payload for `writ status` and `writ jobs` v1 envelope responses.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct JobsData {
    /// Store this snapshot was built from.
    pub source: String,
    /// Lease identities (active and released).
    pub jobs: Vec<JobStatus>,
    /// Agent registry rows (live and stopped).
    pub agents: Vec<AgentStatus>,
}

impl JobsData {
    /// Empty successful snapshot of the lease store.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            source: SOURCE_LEASE_STORE.to_owned(),
            jobs: Vec::new(),
            agents: Vec::new(),
        }
    }
}

/// Named envelope type for `writ status --json` / `writ jobs --json` responses.
pub type StatusReport = Response<JobsData>;

const SOURCE_LEASE_STORE: &str = "lease_store";

/// Map a recorded lease mode onto local collaboration state. No GitHub merge
/// outcome is inferred.
#[must_use]
pub fn collaboration_state_from_mode(mode: LeaseMode) -> CollaborationState {
    match mode {
        LeaseMode::WriterLocked => CollaborationState::Running,
        LeaseMode::NeedsHuman => CollaborationState::Waiting,
        LeaseMode::ReviewOnly => CollaborationState::Paused,
        LeaseMode::Blocked => CollaborationState::Conflicted,
        LeaseMode::MergeReady => CollaborationState::ReadyForIntegration,
        LeaseMode::Unassigned => CollaborationState::Unassigned,
    }
}

/// Load collaboration status from the default lease-store path.
pub fn load() -> Result<JobsData, String> {
    load_from_path(&lease_store_path())
}

/// Load collaboration status from an explicit SQLite path.
pub fn load_from_path(path: &Path) -> Result<JobsData, String> {
    let store = LeaseStore::open(path).map_err(|e| e.to_string())?;
    load_from_store(&store)
}

/// Load collaboration status from an already-open store.
pub fn load_from_store(store: &LeaseStore) -> Result<JobsData, String> {
    let leases = store.list_all().map_err(|e| e.to_string())?;
    let agents = store.list_agents().map_err(|e| e.to_string())?;
    Ok(JobsData {
        source: SOURCE_LEASE_STORE.to_owned(),
        jobs: leases.iter().map(job_from_lease).collect(),
        agents: agents.into_iter().map(agent_status).collect(),
    })
}

fn job_from_lease(lease: &Lease) -> JobStatus {
    let (head, head_source, recovery_needed) = resolve_head(lease);
    JobStatus {
        job_id: lease.job_id.clone(),
        owner: lease.owner.clone(),
        repo: lease.repo_name.clone(),
        issue_number: None,
        pr_number: None,
        worktree_path: lease.worktree_path.clone(),
        branch: lease.branch.clone(),
        process_state: ProcessState::Unknown,
        last_error: None,
        ci_class: CiClass::Unknown,
        timeout_class: None,
        residual_blockers: Vec::new(),
        redispatch_count: None,
        fix_count: None,
        recovery_stage: None,
        last_output_ms: None,
        max_redispatch_per_item: None,
        collaboration_state: collaboration_state_from_mode(lease.mode),
        lease_mode: Some(lease.mode.as_str().to_owned()),
        head,
        head_source,
        lease_row_id: Some(lease.row_id),
        updated_at: Some(lease.updated_at),
        ownership_generation: None,
        agent_id: None,
        session_id: None,
        declared_paths: None,
        blocker: None,
        handoff: None,
        recovery_needed,
    }
}

fn resolve_head(lease: &Lease) -> (Option<String>, Option<String>, Option<bool>) {
    match inspect_checkout(Path::new(&lease.worktree_path)) {
        Ok(info) => (info.head_commit, Some("checkout".to_owned()), Some(false)),
        Err(_) => (
            nonempty_head(&lease.start_commit),
            Some("lease".to_owned()),
            Some(lease.released_at.is_none()),
        ),
    }
}

fn nonempty_head(start_commit: &str) -> Option<String> {
    let trimmed = start_commit.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn agent_status(record: AgentRecord) -> AgentStatus {
    AgentStatus {
        agent_id: record.agent_id,
        agent_type: record.agent_type,
        session_id: record.session_id,
        started_at: record.started_at,
        stopped_at: record.stopped_at,
    }
}

/// Human summary derived from the same snapshot as JSON.
#[must_use]
pub fn format_human(data: &JobsData) -> String {
    if data.jobs.is_empty() && data.agents.is_empty() {
        return "No collaboration participants.\n".to_owned();
    }
    let mut out = format!("source={}\n", data.source);
    for job in &data.jobs {
        out.push_str(&format_job_line(job));
        out.push('\n');
    }
    if !data.agents.is_empty() {
        out.push_str("agents:\n");
        for agent in &data.agents {
            out.push_str(&format_agent_line(agent));
            out.push('\n');
        }
    }
    out
}

fn format_job_line(job: &JobStatus) -> String {
    let head = job.head.as_deref().unwrap_or("unknown");
    let mode = job.lease_mode.as_deref().unwrap_or("unknown");
    format!(
        "{} [{}] {}/{} path={} branch={} head={} lease={}",
        job.job_id,
        job.collaboration_state,
        job.owner,
        job.repo,
        job.worktree_path,
        job.branch,
        head,
        mode
    )
}

fn format_agent_line(agent: &AgentStatus) -> String {
    let session = agent.session_id.as_deref().unwrap_or("unknown");
    let liveness = if agent.stopped_at.is_some() {
        "stopped"
    } else {
        "live"
    };
    format!(
        "  {} type={} session={} {}",
        agent.agent_id, agent.agent_type, session, liveness
    )
}

/// Build a successful v1 envelope response for the given command and snapshot.
#[must_use]
pub fn status_response(command: &'static str, data: JobsData) -> StatusReport {
    Response::success(command, data)
}

/// Build a failure v1 envelope when coordination state cannot be loaded.
#[must_use]
pub fn status_error(command: &'static str, message: String) -> StatusReport {
    Response {
        ok: false,
        schema_version: crate::contract::SCHEMA_VERSION,
        command,
        data: JobsData::empty(),
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
    use crate::lease::{AgentIdentity, LeaseGrant};
    use crate::timeout_policy::TimeoutClass;
    use tempfile::tempdir;

    fn sample_job() -> JobStatus {
        JobStatus {
            job_id: "writ-100".to_owned(),
            owner: "acme".to_owned(),
            repo: "example-org".to_owned(),
            worktree_path: "/tmp/worktrees/acme/example-org/writ-100".to_owned(),
            branch: "feature/status-json-cli".to_owned(),
            issue_number: Some(29),
            pr_number: Some(42),
            process_state: ProcessState::Unknown,
            last_error: None,
            ci_class: CiClass::Unknown,
            collaboration_state: CollaborationState::Running,
            lease_mode: Some("WRITER_LOCKED".to_owned()),
            head: Some("abc123".to_owned()),
            head_source: Some("lease".to_owned()),
            lease_row_id: Some(1),
            updated_at: Some(1),
            ..JobStatus::default()
        }
    }

    fn grant<'a>(repo: &'a Path, wt: &'a Path, job_id: &'a str, branch: &'a str) -> LeaseGrant<'a> {
        LeaseGrant {
            repo,
            owner: "acme",
            repo_name: "sample",
            job_id,
            branch,
            worktree_path: wt,
            start_commit: "abc123",
        }
    }

    #[test]
    fn job_status_serializes_unknown_fields_as_null() {
        let job = sample_job();
        let json = serde_json::to_string(&job).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v.get("job_id").expect("missing job_id"), "writ-100");
        assert_eq!(
            v.get("collaboration_state")
                .expect("missing collaboration_state"),
            "running"
        );
        assert_eq!(
            v.get("process_state").expect("missing process_state"),
            "unknown"
        );
        assert_eq!(v.get("ci_class").expect("missing ci_class"), "unknown");
        assert!(v.get("last_error").expect("missing last_error").is_null());
        assert!(
            v.get("ownership_generation")
                .expect("missing ownership_generation")
                .is_null()
        );
        assert!(
            v.get("declared_paths")
                .expect("missing declared_paths")
                .is_null()
        );
        assert!(v.get("blocker").expect("missing blocker").is_null());
        assert!(v.get("handoff").expect("missing handoff").is_null());
        assert!(v.get("agent_id").expect("missing agent_id").is_null());
        assert!(v.get("session_id").expect("missing session_id").is_null());
    }

    #[test]
    fn job_status_omits_legacy_none_issue_fields() {
        let job = JobStatus {
            issue_number: None,
            pr_number: None,
            ..sample_job()
        };
        let json = serde_json::to_string(&job).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(v.get("issue_number").is_none());
        assert!(v.get("pr_number").is_none());
    }

    #[test]
    fn status_response_uses_v1_envelope() {
        let mut data = JobsData::empty();
        data.jobs.push(sample_job());
        let response = status_response("cli.status", data);
        let json = serde_json::to_string(&response).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

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
        assert!(v.get("jobs").is_none(), "jobs must not be at top level");
        let data = v.get("data").expect("missing data");
        assert_eq!(data.get("source").expect("missing source"), "lease_store");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert_eq!(jobs.len(), 1);
        let agents = data
            .get("agents")
            .expect("missing data.agents")
            .as_array()
            .expect("data.agents must be an array");
        assert!(agents.is_empty());
    }

    #[test]
    fn empty_response_has_zero_jobs() {
        let response = status_response("cli.jobs", JobsData::empty());
        assert!(response.data.jobs.is_empty());
        assert!(response.ok);
        assert_eq!(response.data.source, "lease_store");
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
    fn timeout_residual_fields_serialize_when_present() {
        let job = JobStatus {
            process_state: ProcessState::TimedOut,
            timeout_class: Some(TimeoutClass::Idle),
            residual_blockers: vec![TimeoutClass::Idle.residual_blocker().to_owned()],
            redispatch_count: Some(1),
            fix_count: Some(0),
            ..sample_job()
        };
        let json = serde_json::to_string(&job).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v.get("process_state").unwrap(), "timed_out");
        assert_eq!(v.get("timeout_class").unwrap(), "idle");
        assert_eq!(
            v.get("residual_blockers").unwrap(),
            &serde_json::json!(["timeout:idle"])
        );
        assert_eq!(v.get("fix_count").unwrap(), 0);
        assert!(v.get("sha").is_none());
        assert!(v.get("commit").is_none());
        assert!(
            !TimeoutClass::Idle.counts_toward_fix_cap(),
            "timeout residual must not be treated as a fix"
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
            (ProcessState::TimedOut, "\"timed_out\""),
            (ProcessState::Unknown, "\"unknown\""),
        ];
        for (state, expected) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), expected);
        }
    }

    #[test]
    fn collaboration_state_maps_lease_modes_without_github_completion() {
        let cases = [
            (
                LeaseMode::WriterLocked,
                CollaborationState::Running,
                "running",
            ),
            (
                LeaseMode::NeedsHuman,
                CollaborationState::Waiting,
                "waiting",
            ),
            (LeaseMode::ReviewOnly, CollaborationState::Paused, "paused"),
            (
                LeaseMode::Blocked,
                CollaborationState::Conflicted,
                "conflicted",
            ),
            (
                LeaseMode::MergeReady,
                CollaborationState::ReadyForIntegration,
                "ready_for_integration",
            ),
            (
                LeaseMode::Unassigned,
                CollaborationState::Unassigned,
                "unassigned",
            ),
        ];
        for (mode, state, json) in cases {
            assert_eq!(collaboration_state_from_mode(mode), state);
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{json}\"")
            );
            assert_ne!(state, CollaborationState::Unknown);
        }
        assert_ne!(
            collaboration_state_from_mode(LeaseMode::Unassigned).to_string(),
            "completed"
        );
        assert_ne!(
            collaboration_state_from_mode(LeaseMode::MergeReady).to_string(),
            "completed"
        );
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

    #[test]
    fn two_leases_render_as_distinct_participants() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt_a = tmp.path().join("checkouts/a");
        let wt_b = tmp.path().join("checkouts/b");
        store.grant(grant(&repo, &wt_a, "job-a", "hive/a")).unwrap();
        store.grant(grant(&repo, &wt_b, "job-b", "hive/b")).unwrap();
        store
            .upsert_agent(AgentIdentity {
                agent_id: "agent-a",
                agent_type: "Worker",
                session_id: Some("session-a"),
            })
            .unwrap();
        store
            .upsert_agent(AgentIdentity {
                agent_id: "agent-b",
                agent_type: "Worker",
                session_id: Some("session-b"),
            })
            .unwrap();

        let data = load_from_store(&store).unwrap();
        assert_eq!(data.jobs.len(), 2);
        assert_eq!(data.jobs[0].job_id, "job-a");
        assert_eq!(data.jobs[1].job_id, "job-b");
        assert_ne!(data.jobs[0].worktree_path, data.jobs[1].worktree_path);
        assert_eq!(
            data.jobs[0].collaboration_state,
            CollaborationState::Running
        );
        assert_eq!(data.jobs[0].process_state, ProcessState::Unknown);
        assert_eq!(data.jobs[0].ci_class, CiClass::Unknown);
        assert_eq!(data.jobs[0].head.as_deref(), Some("abc123"));
        assert_eq!(data.jobs[0].head_source.as_deref(), Some("lease"));
        assert_eq!(data.jobs[0].recovery_needed, Some(true));
        assert_eq!(data.agents.len(), 2);
        assert_eq!(data.agents[0].agent_id, "agent-a");
        assert_eq!(data.agents[1].agent_id, "agent-b");

        let human = format_human(&data);
        assert!(human.contains("job-a"));
        assert!(human.contains("job-b"));
        assert!(human.contains("agent-a"));
        assert!(human.contains("agent-b"));
        assert!(!human.contains("completed"));
    }

    #[test]
    fn released_lease_is_unassigned_not_completed() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkouts/a");
        store.grant(grant(&repo, &wt, "job-a", "hive/a")).unwrap();
        store.release_by_path(&wt).unwrap();

        let data = load_from_store(&store).unwrap();
        assert_eq!(data.jobs.len(), 1);
        assert_eq!(
            data.jobs[0].collaboration_state,
            CollaborationState::Unassigned
        );
        assert_eq!(data.jobs[0].lease_mode.as_deref(), Some("UNASSIGNED"));
        assert_eq!(data.jobs[0].process_state, ProcessState::Unknown);
        assert_ne!(
            data.jobs[0].collaboration_state.to_string(),
            ProcessState::Completed.to_string()
        );
        let human = format_human(&data);
        assert!(human.contains("unassigned"));
        assert!(!human.contains("completed"));
    }

    #[test]
    fn human_empty_snapshot_is_not_a_watchlist_message() {
        let text = format_human(&JobsData::empty());
        assert_eq!(text, "No collaboration participants.\n");
        assert!(!text.contains("watched"));
    }
}
