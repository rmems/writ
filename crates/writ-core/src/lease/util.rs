//! Shared lease-store helpers.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, LeaseAttentionFailure, PolicyCode};

use super::{AllocationInspection, AllocationState, Lease, OPERATION_SEQ, ReconcileOutcome};

pub(super) fn terminal_error(lease: &Lease) -> Error {
    let code = if lease.allocation_state == AllocationState::Tombstoned {
        PolicyCode::LeaseTombstoned
    } else {
        PolicyCode::LeaseReleased
    };
    Error::PolicyViolation {
        code,
        message: format!(
            "refusing to resurrect {} lease {}",
            lease.allocation_state.as_str(),
            lease.operation_id
        ),
    }
}

pub(super) fn terminal_outcome(lease: Lease, inspection: AllocationInspection) -> ReconcileOutcome {
    if lease.allocation_state == AllocationState::Tombstoned {
        ReconcileOutcome::Tombstoned { lease, inspection }
    } else {
        ReconcileOutcome::Released { lease, inspection }
    }
}

/// Convert a needs-attention reconcile result into the fail-closed error type.
#[must_use]
pub fn attention_error(lease: &Lease, inspection: &AllocationInspection) -> Error {
    Error::LeaseAttention(Box::new(LeaseAttentionFailure {
        operation_id: lease.operation_id.clone(),
        allocation_state: lease.allocation_state.as_str().to_owned(),
        classification: inspection.classification.as_str().to_owned(),
        conflicts: inspection.conflicts.clone(),
        path: inspection.derived_path.clone(),
        path_exists: inspection.path_exists,
        branch_commit: inspection.branch_commit.clone(),
        head_commit: inspection.head_commit.clone(),
        worktree_registered: inspection.worktree_registered,
    }))
}

pub(super) fn lease_err(context: &'static str, err: rusqlite::Error) -> Error {
    Error::LeaseStore {
        context,
        message: err.to_string(),
    }
}

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(super) fn new_operation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{nanos}-{}-{}",
        std::process::id(),
        OPERATION_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) fn named_git_branch(branch: &str) -> bool {
    !branch.is_empty() && branch != "(detached)"
}

/// Store a real branch as `refs/heads/…`; keep detached/empty identity as `HEAD`.
pub(super) fn stored_branch_ref(branch: &str) -> String {
    if named_git_branch(branch) {
        format!("refs/heads/{branch}")
    } else {
        "HEAD".to_owned()
    }
}
