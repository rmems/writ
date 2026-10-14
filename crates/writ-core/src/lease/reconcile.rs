//! Deterministic reconcile of an interrupted allocation.

use std::path::Path;

use rusqlite::{Transaction, params};

use crate::error::Result;

use super::LeaseStore;
use super::inspect::inspect_now;
use super::row::query_lease_tx;
use super::tx::{OpPhase, begin_immediate, commit_value, update_op_phase};
use super::types::{
    AllocationInspection, AllocationState, EvidenceClass, InspectRequest, JobKey, Lease, LeaseMode,
    ReconcileOutcome,
};
use super::util::{lease_err, now_secs, require_lease, terminal_outcome};

const PROMOTE_SQL: &str = "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ";

const ABORT_SQL: &str = "
            DELETE FROM leases
            WHERE operation_id = ?1
              AND allocation_state IN ('PREPARED', 'MUTATING', 'ABORTED')
              AND released_at IS NULL AND tombstoned_at IS NULL
            ";

const ATTENTION_SQL: &str = "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ";

enum OpenGate {
    Missing,
    Terminal(Lease),
    Active(Lease),
    Open(Lease),
}

enum Settle {
    Done(ReconcileOutcome),
    Pending(Lease, AllocationInspection),
}

impl LeaseStore {
    /// Reconcile an interrupted allocation without destructive cleanup.
    pub fn reconcile(&self, key: JobKey<'_>, repo_root: &Path) -> Result<Option<ReconcileOutcome>> {
        let Some(lease) = self.find_job(key)? else {
            return Ok(None);
        };
        let inspection = inspection_for(&lease, key, repo_root);
        Ok(Some(self.reconcile_state(lease, inspection)?))
    }

    fn reconcile_state(
        &self,
        lease: Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        match settle(lease, inspection) {
            Settle::Done(outcome) => Ok(outcome),
            Settle::Pending(lease, inspection) => self.reconcile_pending(&lease, inspection),
        }
    }

    fn reconcile_pending(
        &self,
        lease: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let write = match inspection.classification {
            EvidenceClass::Matching => OpenWrite::Promote,
            EvidenceClass::Retryable => OpenWrite::Abort,
            _ => OpenWrite::Attention,
        };
        self.finish_open(lease, inspection, write)
    }

    fn finish_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
        write: OpenWrite,
    ) -> Result<ReconcileOutcome> {
        match self.settle_open(expected, write)? {
            OpenEffect::Retry => Ok(retry_with(expected, inspection)),
            OpenEffect::Terminal(lease) => Ok(terminal_outcome(lease, inspection)),
            OpenEffect::AlreadyActive(lease) => {
                Ok(ReconcileOutcome::AlreadyActive { lease, inspection })
            }
            OpenEffect::Reload(kind) => self.reload_outcome(expected, inspection, kind),
        }
    }

    fn settle_open(&self, expected: &Lease, write: OpenWrite) -> Result<OpenEffect> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, write.begin())?;
        let current = reread_for(&tx, expected, write)?;
        settle_current(tx, expected, current, now, write)
    }

    fn reload_outcome(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
        kind: ReloadKind,
    ) -> Result<ReconcileOutcome> {
        let row = self.find_by_operation(&expected.operation_id)?;
        let lease = require_lease(row, kind.context(), kind.missing())?;
        Ok(kind.outcome(lease, inspection))
    }
}

fn inspection_for(lease: &Lease, key: JobKey<'_>, repo_root: &Path) -> AllocationInspection {
    let request = InspectRequest {
        repo_root,
        owner: key.owner,
        repo_name: key.repo_name,
        job_id: key.job_id,
        worktree_path: Path::new(&lease.worktree_path),
        branch: Some(&lease.branch),
    };
    inspect_now(&Some(lease.clone()), request)
}

fn settle(lease: Lease, inspection: AllocationInspection) -> Settle {
    match lease.allocation_state {
        AllocationState::Tombstoned => {
            Settle::Done(ReconcileOutcome::Tombstoned { lease, inspection })
        }
        AllocationState::Released => Settle::Done(ReconcileOutcome::Released { lease, inspection }),
        AllocationState::Active => {
            Settle::Done(ReconcileOutcome::AlreadyActive { lease, inspection })
        }
        AllocationState::Aborted => Settle::Done(ReconcileOutcome::Retry {
            operation_id: lease.operation_id,
            inspection,
        }),
        _ => Settle::Pending(lease, inspection),
    }
}

fn retry_with(expected: &Lease, inspection: AllocationInspection) -> ReconcileOutcome {
    ReconcileOutcome::Retry {
        operation_id: expected.operation_id.clone(),
        inspection,
    }
}

fn gate_existing(current: Option<Lease>) -> OpenGate {
    match current {
        None => OpenGate::Missing,
        Some(lease) if lease.allocation_state.is_terminal() => OpenGate::Terminal(lease),
        Some(lease) => OpenGate::Open(lease),
    }
}

fn reread_job(tx: &Transaction<'_>, lease: &Lease) -> Result<Option<Lease>> {
    query_lease_tx(
        tx,
        "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
        params![lease.owner, lease.repo_name, lease.job_id],
        "re-read lease before promote",
    )
}

fn reread_op(
    tx: &Transaction<'_>,
    operation_id: &str,
    context: &'static str,
) -> Result<Option<Lease>> {
    query_lease_tx(
        tx,
        "WHERE operation_id = ?1",
        params![operation_id],
        context,
    )
}

#[derive(Clone, Copy)]
enum OpenWrite {
    Promote,
    Abort,
    Attention,
}

enum OpenEffect {
    Retry,
    Terminal(Lease),
    AlreadyActive(Lease),
    Reload(ReloadKind),
}

#[derive(Clone, Copy)]
enum ReloadKind {
    Promoted,
    Attention,
}

impl OpenWrite {
    fn begin(self) -> &'static str {
        match self {
            Self::Promote => "begin promote",
            Self::Abort => "begin abort",
            Self::Attention => "begin attention",
        }
    }

    fn commit_label(self) -> &'static str {
        match self {
            Self::Promote => "commit promote",
            Self::Abort => "commit abort",
            Self::Attention => "commit attention",
        }
    }

    fn reread_context(self) -> &'static str {
        match self {
            Self::Promote => "re-read lease before promote",
            Self::Abort => "re-read lease before abort",
            Self::Attention => "re-read lease before attention",
        }
    }
}

impl ReloadKind {
    fn context(self) -> &'static str {
        match self {
            Self::Promoted => "promote lease",
            Self::Attention => "mark needs attention",
        }
    }

    fn missing(self) -> String {
        match self {
            Self::Promoted => "lease row missing after promote".to_owned(),
            Self::Attention => "lease row missing after attention".to_owned(),
        }
    }

    fn outcome(self, lease: Lease, inspection: AllocationInspection) -> ReconcileOutcome {
        match self {
            Self::Promoted => ReconcileOutcome::Promoted { lease, inspection },
            Self::Attention => ReconcileOutcome::NeedsAttention { lease, inspection },
        }
    }
}

fn reread_for(tx: &Transaction<'_>, expected: &Lease, write: OpenWrite) -> Result<Option<Lease>> {
    if matches!(write, OpenWrite::Promote) {
        return reread_job(tx, expected);
    }
    reread_op(tx, &expected.operation_id, write.reread_context())
}

fn settle_current(
    tx: Transaction<'_>,
    expected: &Lease,
    current: Option<Lease>,
    now: i64,
    write: OpenWrite,
) -> Result<OpenEffect> {
    let label = write.commit_label();
    match classify_open(write, current) {
        OpenGate::Missing => commit_value(tx, label, OpenEffect::Retry),
        OpenGate::Terminal(lease) => commit_value(tx, label, OpenEffect::Terminal(lease)),
        OpenGate::Active(lease) => commit_value(tx, label, OpenEffect::AlreadyActive(lease)),
        OpenGate::Open(lease) => apply_open_write(tx, expected, lease, now, write),
    }
}

fn classify_open(write: OpenWrite, current: Option<Lease>) -> OpenGate {
    match gate_existing(current) {
        OpenGate::Open(lease)
            if matches!(write, OpenWrite::Promote)
                && lease.allocation_state == AllocationState::Active =>
        {
            OpenGate::Active(lease)
        }
        other => other,
    }
}

fn apply_open_write(
    tx: Transaction<'_>,
    expected: &Lease,
    current: Lease,
    now: i64,
    write: OpenWrite,
) -> Result<OpenEffect> {
    let label = write.commit_label();
    if !wrote_open_row(&tx, &expected.operation_id, now, write)? {
        return commit_value(tx, label, OpenEffect::Terminal(current));
    }
    let (phase, status, effect) = write.success();
    update_op_phase(
        &tx,
        OpPhase {
            operation_id: &expected.operation_id,
            phase,
            status,
            now,
        },
    )?;
    commit_value(tx, label, effect)
}

impl OpenWrite {
    fn success(self) -> (&'static str, &'static str, OpenEffect) {
        match self {
            Self::Promote => (
                "COMMIT",
                "COMMITTED",
                OpenEffect::Reload(ReloadKind::Promoted),
            ),
            Self::Abort => ("PREPARE", "ABORTED", OpenEffect::Retry),
            Self::Attention => (
                "MUTATE",
                "NEEDS_ATTENTION",
                OpenEffect::Reload(ReloadKind::Attention),
            ),
        }
    }
}

fn wrote_open_row(
    tx: &Transaction<'_>,
    operation_id: &str,
    now: i64,
    write: OpenWrite,
) -> Result<bool> {
    match write {
        OpenWrite::Promote => execute_changed(
            tx,
            PROMOTE_SQL,
            params![
                AllocationState::Active.as_str(),
                LeaseMode::WriterLocked.as_str(),
                now,
                operation_id,
            ],
            "promote lease",
        ),
        OpenWrite::Abort => {
            execute_changed(tx, ABORT_SQL, params![operation_id], "abort reservation")
        }
        OpenWrite::Attention => execute_changed(
            tx,
            ATTENTION_SQL,
            params![
                AllocationState::NeedsAttention.as_str(),
                LeaseMode::NeedsHuman.as_str(),
                now,
                operation_id,
            ],
            "mark needs attention",
        ),
    }
}

fn execute_changed(
    tx: &Transaction<'_>,
    sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<bool> {
    tx.execute(sql, sql_params)
        .map_err(|e| lease_err(context, e))?;
    Ok(tx.changes() == 1)
}
