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
    Open(Lease),
}

enum PromoteGate {
    Missing,
    Terminal(Lease),
    Active(Lease),
    Open(Lease),
}

enum PromoteInTx {
    Retry,
    Terminal(Lease),
    AlreadyActive(Lease),
    Promoted,
}

enum AbortInTx {
    Retry,
    Terminal(Box<Lease>),
}

enum AttentionInTx {
    Retry,
    Terminal(Box<Lease>),
    Marked,
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
        match inspection.classification {
            EvidenceClass::Matching => self.promote_if_still_open(lease, inspection),
            EvidenceClass::Retryable => self.abort_if_still_open(lease, inspection),
            _ => self.attention_if_still_open(lease, inspection),
        }
    }

    fn promote_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        match self.promote_in_tx(expected)? {
            PromoteInTx::Retry => Ok(retry_with(expected, inspection)),
            PromoteInTx::Terminal(lease) => Ok(terminal_outcome(lease, inspection)),
            PromoteInTx::AlreadyActive(lease) => {
                Ok(ReconcileOutcome::AlreadyActive { lease, inspection })
            }
            PromoteInTx::Promoted => self.promoted(expected, inspection),
        }
    }

    fn promote_in_tx(&self, expected: &Lease) -> Result<PromoteInTx> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin promote")?;
        let current = reread_job(&tx, expected)?;
        dispatch_promote(tx, expected, current, now)
    }

    fn promoted(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let row = self.find_by_operation(&expected.operation_id)?;
        let lease = require_lease(
            row,
            "promote lease",
            "lease row missing after promote".to_owned(),
        )?;
        Ok(ReconcileOutcome::Promoted { lease, inspection })
    }

    fn abort_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let decision = self.abort_in_tx(expected)?;
        Ok(abort_outcome(decision, expected, inspection))
    }

    fn abort_in_tx(&self, expected: &Lease) -> Result<AbortInTx> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin abort")?;
        let current = reread_op(&tx, &expected.operation_id, "re-read lease before abort")?;
        dispatch_abort(tx, &expected.operation_id, current, now)
    }

    fn attention_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        match self.attention_in_tx(expected)? {
            AttentionInTx::Retry => Ok(retry_with(expected, inspection)),
            AttentionInTx::Terminal(lease) => Ok(terminal_outcome(*lease, inspection)),
            AttentionInTx::Marked => self.marked_attention(expected, inspection),
        }
    }

    fn attention_in_tx(&self, expected: &Lease) -> Result<AttentionInTx> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin attention")?;
        let current = reread_op(
            &tx,
            &expected.operation_id,
            "re-read lease before attention",
        )?;
        dispatch_attention(tx, &expected.operation_id, current, now)
    }

    fn marked_attention(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let row = self.find_by_operation(&expected.operation_id)?;
        let lease = require_lease(
            row,
            "mark needs attention",
            "lease row missing after attention".to_owned(),
        )?;
        Ok(ReconcileOutcome::NeedsAttention { lease, inspection })
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

fn abort_outcome(
    decision: AbortInTx,
    expected: &Lease,
    inspection: AllocationInspection,
) -> ReconcileOutcome {
    match decision {
        AbortInTx::Retry => retry_with(expected, inspection),
        AbortInTx::Terminal(lease) => terminal_outcome(*lease, inspection),
    }
}

fn gate_existing(current: Option<Lease>) -> OpenGate {
    match current {
        None => OpenGate::Missing,
        Some(lease) if lease.allocation_state.is_terminal() => OpenGate::Terminal(lease),
        Some(lease) => OpenGate::Open(lease),
    }
}

fn gate_promote(current: Option<Lease>) -> PromoteGate {
    match gate_existing(current) {
        OpenGate::Missing => PromoteGate::Missing,
        OpenGate::Terminal(lease) => PromoteGate::Terminal(lease),
        OpenGate::Open(lease) if lease.allocation_state == AllocationState::Active => {
            PromoteGate::Active(lease)
        }
        OpenGate::Open(lease) => PromoteGate::Open(lease),
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

fn dispatch_promote(
    tx: Transaction<'_>,
    expected: &Lease,
    current: Option<Lease>,
    now: i64,
) -> Result<PromoteInTx> {
    match gate_promote(current) {
        PromoteGate::Missing => commit_value(tx, "commit promote", PromoteInTx::Retry),
        PromoteGate::Terminal(lease) => {
            commit_value(tx, "commit promote", PromoteInTx::Terminal(lease))
        }
        PromoteGate::Active(lease) => {
            commit_value(tx, "commit promote", PromoteInTx::AlreadyActive(lease))
        }
        PromoteGate::Open(lease) => write_promote(tx, expected, lease, now),
    }
}

fn write_promote(
    tx: Transaction<'_>,
    expected: &Lease,
    current: Lease,
    now: i64,
) -> Result<PromoteInTx> {
    if !promoted_row(&tx, &expected.operation_id, now)? {
        return commit_value(tx, "commit promote", PromoteInTx::Terminal(current));
    }
    update_op_phase(
        &tx,
        OpPhase {
            operation_id: &expected.operation_id,
            phase: "COMMIT",
            status: "COMMITTED",
            now,
        },
    )?;
    commit_value(tx, "commit promote", PromoteInTx::Promoted)
}

fn promoted_row(tx: &Transaction<'_>, operation_id: &str, now: i64) -> Result<bool> {
    tx.execute(
        PROMOTE_SQL,
        params![
            AllocationState::Active.as_str(),
            LeaseMode::WriterLocked.as_str(),
            now,
            operation_id,
        ],
    )
    .map_err(|e| lease_err("promote lease", e))?;
    Ok(tx.changes() == 1)
}

fn dispatch_abort(
    tx: Transaction<'_>,
    operation_id: &str,
    current: Option<Lease>,
    now: i64,
) -> Result<AbortInTx> {
    match gate_existing(current) {
        OpenGate::Missing => commit_value(tx, "commit abort", AbortInTx::Retry),
        OpenGate::Terminal(lease) => {
            commit_value(tx, "commit abort", AbortInTx::Terminal(Box::new(lease)))
        }
        OpenGate::Open(lease) => delete_reservation(tx, operation_id, lease, now),
    }
}

fn delete_reservation(
    tx: Transaction<'_>,
    operation_id: &str,
    current: Lease,
    now: i64,
) -> Result<AbortInTx> {
    if !deleted_row(&tx, operation_id)? {
        return commit_value(tx, "commit abort", AbortInTx::Terminal(Box::new(current)));
    }
    update_op_phase(
        &tx,
        OpPhase {
            operation_id,
            phase: "PREPARE",
            status: "ABORTED",
            now,
        },
    )?;
    commit_value(tx, "commit abort", AbortInTx::Retry)
}

fn deleted_row(tx: &Transaction<'_>, operation_id: &str) -> Result<bool> {
    tx.execute(ABORT_SQL, params![operation_id])
        .map_err(|e| lease_err("abort reservation", e))?;
    Ok(tx.changes() == 1)
}

fn dispatch_attention(
    tx: Transaction<'_>,
    operation_id: &str,
    current: Option<Lease>,
    now: i64,
) -> Result<AttentionInTx> {
    match gate_existing(current) {
        OpenGate::Missing => commit_value(tx, "commit attention", AttentionInTx::Retry),
        OpenGate::Terminal(lease) => commit_value(
            tx,
            "commit attention",
            AttentionInTx::Terminal(Box::new(lease)),
        ),
        OpenGate::Open(lease) => mark_attention(tx, operation_id, lease, now),
    }
}

fn mark_attention(
    tx: Transaction<'_>,
    operation_id: &str,
    current: Lease,
    now: i64,
) -> Result<AttentionInTx> {
    if !attention_row(&tx, operation_id, now)? {
        return commit_value(
            tx,
            "commit attention",
            AttentionInTx::Terminal(Box::new(current)),
        );
    }
    update_op_phase(
        &tx,
        OpPhase {
            operation_id,
            phase: "MUTATE",
            status: "NEEDS_ATTENTION",
            now,
        },
    )?;
    commit_value(tx, "commit attention", AttentionInTx::Marked)
}

fn attention_row(tx: &Transaction<'_>, operation_id: &str, now: i64) -> Result<bool> {
    tx.execute(
        ATTENTION_SQL,
        params![
            AllocationState::NeedsAttention.as_str(),
            LeaseMode::NeedsHuman.as_str(),
            now,
            operation_id,
        ],
    )
    .map_err(|e| lease_err("mark needs attention", e))?;
    Ok(tx.changes() == 1)
}
