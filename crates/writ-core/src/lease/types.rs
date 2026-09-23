//! Lease identity, inspection, and reconcile types.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Lease admission modes reserved by #124.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LeaseMode {
    Unassigned,
    WriterLocked,
    ReviewOnly,
    NeedsHuman,
    Blocked,
    MergeReady,
    /// Stored value is not one of the known modes. Not treated as released.
    Unknown,
}

impl LeaseMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "UNASSIGNED",
            Self::WriterLocked => "WRITER_LOCKED",
            Self::ReviewOnly => "REVIEW_ONLY",
            Self::NeedsHuman => "NEEDS_HUMAN",
            Self::Blocked => "BLOCKED",
            Self::MergeReady => "MERGE_READY",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "WRITER_LOCKED" => Self::WriterLocked,
            "REVIEW_ONLY" => Self::ReviewOnly,
            "NEEDS_HUMAN" => Self::NeedsHuman,
            "BLOCKED" => Self::Blocked,
            "MERGE_READY" => Self::MergeReady,
            "UNASSIGNED" => Self::Unassigned,
            _ => Self::Unknown,
        }
    }
}

/// Durable allocation phase for crash recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AllocationState {
    Prepared,
    Mutating,
    Active,
    NeedsAttention,
    Released,
    Tombstoned,
    Aborted,
}

impl AllocationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "PREPARED",
            Self::Mutating => "MUTATING",
            Self::Active => "ACTIVE",
            Self::NeedsAttention => "NEEDS_ATTENTION",
            Self::Released => "RELEASED",
            Self::Tombstoned => "TOMBSTONED",
            Self::Aborted => "ABORTED",
        }
    }

    pub(super) fn parse(value: &str) -> Self {
        match value {
            "PREPARED" => Self::Prepared,
            "MUTATING" => Self::Mutating,
            "ACTIVE" => Self::Active,
            "NEEDS_ATTENTION" => Self::NeedsAttention,
            "RELEASED" => Self::Released,
            "TOMBSTONED" => Self::Tombstoned,
            _ => Self::Aborted,
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Released | Self::Tombstoned)
    }

    pub(crate) fn is_in_progress(self) -> bool {
        matches!(self, Self::Prepared | Self::Mutating | Self::NeedsAttention)
    }
}

/// Classification of lease-vs-git evidence. Distinct from TTL expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Matching,
    Retryable,
    NeedsAttention,
    OrphanedLease,
    MissingLease,
    Released,
    Tombstoned,
    Absent,
}

impl EvidenceClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matching => "matching",
            Self::Retryable => "retryable",
            Self::NeedsAttention => "needs_attention",
            Self::OrphanedLease => "orphaned_lease",
            Self::MissingLease => "missing_lease",
            Self::Released => "released",
            Self::Tombstoned => "tombstoned",
            Self::Absent => "absent",
        }
    }
}

/// One lease row, including crash-consistency identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub repo: String,
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub branch: String,
    pub branch_ref: String,
    pub worktree_path: String,
    pub requested_start_point: String,
    pub start_commit: String,
    pub operation_id: String,
    pub allocation_state: AllocationState,
    pub mode: LeaseMode,
    /// Stored mode text before mapping; unrecognized values survive verbatim.
    pub mode_raw: String,
    pub ttl: Option<i64>,
    pub heartbeat: Option<i64>,
    pub max_files: Option<i64>,
    pub max_churn: Option<i64>,
    pub max_fix_cycles: Option<i64>,
    pub fix_cycles: Option<i64>,
    pub pending_fix_cycles: Option<i64>,
    pub pending_fix_op_id: Option<String>,
    pub released_at: Option<i64>,
    pub tombstoned_at: Option<i64>,
    /// SQLite row id. Identity for the lease record, not an ownership generation.
    pub row_id: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One `agents` registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRecord {
    pub agent_id: String,
    pub agent_type: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub stopped_at: Option<i64>,
}

/// Inputs required to persist an allocation before the ownership mutation.
#[derive(Debug, Clone, Copy)]
pub struct AllocateRequest<'a> {
    pub repo: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
    pub worktree_path: &'a Path,
    pub requested_start_point: &'a str,
    pub start_commit: &'a str,
    pub ttl: Option<i64>,
}

/// Owner/repo/job identity without the branch (unique lease row key).
#[derive(Debug, Clone, Copy)]
pub struct JobKey<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
}

/// Inputs required to grant or refresh a writer lock after a checkout exists.
#[derive(Debug, Clone, Copy)]
pub struct LeaseGrant<'a> {
    pub repo: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
    pub worktree_path: &'a Path,
    pub start_commit: &'a str,
}

/// Durable resume identity key: owner/repo/job plus the branch name.
#[derive(Debug, Clone, Copy)]
pub struct ResumeKey<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
}

/// Agent-registry upsert payload.
#[derive(Debug, Clone, Copy)]
pub struct AgentIdentity<'a> {
    pub agent_id: &'a str,
    pub agent_type: &'a str,
    pub session_id: Option<&'a str>,
}

/// Read-only inspection inputs. Never used to adopt or mutate git state.
#[derive(Debug, Clone, Copy)]
pub struct InspectRequest<'a> {
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub worktree_path: &'a Path,
    pub branch: Option<&'a str>,
}

/// Observed git + lease identity. Produced without mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllocationInspection {
    pub operation_id: Option<String>,
    pub allocation_state: Option<String>,
    pub requested_start_point: Option<String>,
    pub resolved_start_commit: Option<String>,
    pub derived_path: PathBuf,
    pub path_exists: bool,
    pub branch_ref: String,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
    pub repo_identity: String,
    pub lease_present: bool,
    pub ttl_expired: bool,
    pub classification: EvidenceClass,
    pub conflicts: Vec<String>,
}

/// Deterministic reconcile result. Never deletes unproven git state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Promoted {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Retry {
        operation_id: String,
        inspection: AllocationInspection,
    },
    NeedsAttention {
        lease: Lease,
        inspection: AllocationInspection,
    },
    AlreadyActive {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Released {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Tombstoned {
        lease: Lease,
        inspection: AllocationInspection,
    },
}

impl ReconcileOutcome {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Promoted { .. } => "promoted",
            Self::Retry { .. } => "retry",
            Self::NeedsAttention { .. } => "needs_attention",
            Self::AlreadyActive { .. } => "already_active",
            Self::Released { .. } => "released",
            Self::Tombstoned { .. } => "tombstoned",
        }
    }
}

/// Authorization token for the one accumulated budget counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixCycleToken {
    pub operation_id: String,
    pub authorized: i64,
}

/// Crash recovery for `fix_cycles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixCycleReconcile {
    Committed { fix_cycles: i64 },
    Aborted { fix_cycles: i64 },
    NeedsAttention { pending: i64, operation_id: String },
    Idle { fix_cycles: i64 },
}
