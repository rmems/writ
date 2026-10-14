//! Shared lease clock, identity, and error helpers.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, PolicyCode, Result};

use super::types::{AllocationInspection, AllocationState, Lease, ReconcileOutcome};

pub(in crate::lease) static OPERATION_SEQ: AtomicU64 = AtomicU64::new(0);

pub(in crate::lease) fn lease_err(context: &'static str, err: rusqlite::Error) -> Error {
    Error::LeaseStore {
        context,
        message: err.to_string(),
    }
}

pub(in crate::lease) fn require_lease(
    row: Option<Lease>,
    context: &'static str,
    message: String,
) -> Result<Lease> {
    match row {
        Some(lease) => Ok(lease),
        None => Err(Error::LeaseStore { context, message }),
    }
}

pub(in crate::lease) fn store_error(context: &'static str, message: impl Into<String>) -> Error {
    Error::LeaseStore {
        context,
        message: message.into(),
    }
}

pub(in crate::lease) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(in crate::lease) fn new_operation_id() -> String {
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

pub(in crate::lease) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(in crate::lease) fn ensure_not_terminal(lease: &Lease) -> Result<()> {
    if lease.allocation_state.is_terminal() {
        Err(terminal_error(lease))
    } else {
        Ok(())
    }
}

pub(in crate::lease) fn terminal_error(lease: &Lease) -> Error {
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

pub(in crate::lease) fn terminal_outcome(
    lease: Lease,
    inspection: AllocationInspection,
) -> ReconcileOutcome {
    if lease.allocation_state == AllocationState::Tombstoned {
        ReconcileOutcome::Tombstoned { lease, inspection }
    } else {
        ReconcileOutcome::Released { lease, inspection }
    }
}

pub(in crate::lease) fn recorded_fix_cycles(lease: &Lease) -> i64 {
    lease.fix_cycles.unwrap_or(0)
}
