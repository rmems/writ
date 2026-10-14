//! Crash-consistent `fix_cycles` increments.

use rusqlite::{Transaction, params};

use crate::error::Result;

use super::LeaseStore;
use super::row::query_lease_tx;
use super::tx::{OpPhase, OpRecord, begin_immediate, commit_tx, insert_op, update_op_phase};
use super::types::{AllocationState, FixCycleReconcile, FixCycleToken, JobKey, Lease};
use super::util::{
    ensure_not_terminal, lease_err, new_operation_id, now_secs, recorded_fix_cycles, require_lease,
    store_error,
};

const PREPARE_FIX_SQL: &str = "
            UPDATE leases
            SET pending_fix_cycles = ?1, pending_fix_op_id = ?2, updated_at = ?3
            WHERE owner = ?4 AND repo_name = ?5 AND job_id = ?6
              AND allocation_state = 'ACTIVE' AND released_at IS NULL AND tombstoned_at IS NULL
            ";

const COMMIT_FIX_SQL: &str = "
            UPDATE leases
            SET fix_cycles = ?1, pending_fix_cycles = NULL, pending_fix_op_id = NULL,
                updated_at = ?2
            WHERE pending_fix_op_id = ?3 AND released_at IS NULL AND tombstoned_at IS NULL
            ";

const ABORT_FIX_SQL: &str = "
            UPDATE leases
            SET pending_fix_cycles = NULL, pending_fix_op_id = NULL, updated_at = ?1
            WHERE pending_fix_op_id = ?2 AND released_at IS NULL AND tombstoned_at IS NULL
            ";

struct FixPrepare<'a> {
    key: JobKey<'a>,
    lease: &'a Lease,
    operation_id: &'a str,
    authorized: i64,
    now: i64,
}

impl LeaseStore {
    /// Persist intent to increment `fix_cycles` before the authorized mutation.
    pub fn prepare_fix_cycle(&self, key: JobKey<'_>) -> Result<FixCycleToken> {
        let now = now_secs();
        let operation_id = new_operation_id();
        let authorized = self.write_fix_prepare(key, &operation_id, now)?;
        Ok(FixCycleToken {
            operation_id,
            authorized,
        })
    }

    /// Record that the mutation authorized by a pending increment is in flight.
    pub fn mark_fix_cycle_mutating(&self, operation_id: &str) -> Result<()> {
        self.set_fix_cycle_phase(operation_id, "MUTATE")
    }

    /// Commit a pending increment after the authorized mutation.
    pub fn commit_fix_cycle(&self, operation_id: &str) -> Result<i64> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin fix-cycle commit")?;
        let committed = commit_fix_in_tx(&tx, operation_id, now)?;
        commit_tx(tx, "commit fix-cycle commit")?;
        Ok(committed)
    }

    /// Recover a pending `fix_cycles` increment without double-counting or losing it.
    pub fn reconcile_fix_cycle(
        &self,
        key: JobKey<'_>,
        mutation_proven: Option<bool>,
    ) -> Result<FixCycleReconcile> {
        let row = self.find_job(key)?;
        let lease = require_lease(row, "reconcile fix-cycle", "lease not found".to_owned())?;
        let Some(operation_id) = lease.pending_fix_op_id.clone() else {
            return Ok(FixCycleReconcile::Idle {
                fix_cycles: recorded_fix_cycles(&lease),
            });
        };
        self.reconcile_pending_fix(lease, operation_id, mutation_proven)
    }

    fn write_fix_prepare(&self, key: JobKey<'_>, operation_id: &str, now: i64) -> Result<i64> {
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin fix-cycle prepare")?;
        let authorized = prepare_fix_in_tx(&tx, key, operation_id, now)?;
        commit_tx(tx, "commit fix-cycle prepare")?;
        Ok(authorized)
    }

    fn set_fix_cycle_phase(&self, operation_id: &str, phase: &'static str) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin fix-cycle phase")?;
        let lease = load_pending(&tx, operation_id, "advance fix-cycle")?;
        ensure_not_terminal(&lease)?;
        update_op_phase(
            &tx,
            OpPhase {
                operation_id,
                phase,
                status: "IN_PROGRESS",
                now,
            },
        )?;
        commit_tx(tx, "commit fix-cycle phase")
    }

    fn reconcile_pending_fix(
        &self,
        lease: Lease,
        operation_id: String,
        mutation_proven: Option<bool>,
    ) -> Result<FixCycleReconcile> {
        let phase = self.op_phase(&operation_id)?;
        match mutation_proven {
            Some(true) => self.commit_proven(&operation_id),
            Some(false) => self.abort_unproven(&operation_id, recorded_fix_cycles(&lease)),
            None => self.reconcile_unknown(&operation_id, &lease, phase.as_deref()),
        }
    }

    fn commit_proven(&self, operation_id: &str) -> Result<FixCycleReconcile> {
        let fix_cycles = self.commit_fix_cycle(operation_id)?;
        Ok(FixCycleReconcile::Committed { fix_cycles })
    }

    fn abort_unproven(&self, operation_id: &str, fix_cycles: i64) -> Result<FixCycleReconcile> {
        self.abort_fix_cycle(operation_id)?;
        Ok(FixCycleReconcile::Aborted { fix_cycles })
    }

    fn reconcile_unknown(
        &self,
        operation_id: &str,
        lease: &Lease,
        phase: Option<&str>,
    ) -> Result<FixCycleReconcile> {
        if phase == Some("PREPARE") {
            self.abort_unproven(operation_id, recorded_fix_cycles(lease))
        } else {
            Ok(FixCycleReconcile::NeedsAttention {
                pending: lease.pending_fix_cycles.unwrap_or(0),
                operation_id: operation_id.to_owned(),
            })
        }
    }

    fn abort_fix_cycle(&self, operation_id: &str) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin fix-cycle abort")?;
        clear_pending_fix(&tx, operation_id, now)?;
        update_op_phase(
            &tx,
            OpPhase {
                operation_id,
                phase: "PREPARE",
                status: "ABORTED",
                now,
            },
        )?;
        commit_tx(tx, "commit fix-cycle abort")
    }
}

fn prepare_fix_in_tx(
    tx: &Transaction<'_>,
    key: JobKey<'_>,
    operation_id: &str,
    now: i64,
) -> Result<i64> {
    let lease = load_fix_lease(tx, key)?;
    let authorized = authorize_fix_cycle(&lease)?;
    store_fix_prepare(
        tx,
        FixPrepare {
            key,
            lease: &lease,
            operation_id,
            authorized,
            now,
        },
    )?;
    Ok(authorized)
}

fn load_fix_lease(tx: &Transaction<'_>, key: JobKey<'_>) -> Result<Lease> {
    let row = query_lease_tx(
        tx,
        "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
        params![key.owner, key.repo_name, key.job_id],
        "lookup lease for fix-cycle",
    )?;
    require_lease(row, "prepare fix-cycle", "lease not found".to_owned())
}

fn authorize_fix_cycle(lease: &Lease) -> Result<i64> {
    if lease.allocation_state != AllocationState::Active {
        return Err(store_error(
            "prepare fix-cycle",
            format!(
                "fix_cycles increment requires ACTIVE lease, found {}",
                lease.allocation_state.as_str()
            ),
        ));
    }
    if lease.pending_fix_op_id.is_some() {
        return Err(store_error(
            "prepare fix-cycle",
            "a pending fix_cycles increment already exists; reconcile it first",
        ));
    }
    Ok(recorded_fix_cycles(lease) + 1)
}

fn store_fix_prepare(tx: &Transaction<'_>, prep: FixPrepare<'_>) -> Result<()> {
    tx.execute(
        PREPARE_FIX_SQL,
        params![
            prep.authorized,
            prep.operation_id,
            prep.now,
            prep.key.owner,
            prep.key.repo_name,
            prep.key.job_id,
        ],
    )
    .map_err(|e| lease_err("prepare fix-cycle", e))?;
    if tx.changes() != 1 {
        return Err(store_error(
            "prepare fix-cycle",
            "released or tombstoned lease cannot increment fix_cycles",
        ));
    }
    insert_op(
        tx,
        OpRecord {
            operation_id: prep.operation_id,
            owner: prep.key.owner,
            repo_name: prep.key.repo_name,
            job_id: prep.key.job_id,
            kind: "INCREMENT_FIX_CYCLE",
            phase: "PREPARE",
            requested_start_point: Some(&prep.lease.requested_start_point),
            resolved_start_commit: Some(&prep.lease.start_commit),
            status: "IN_PROGRESS",
            now: prep.now,
        },
    )
}

fn commit_fix_in_tx(tx: &Transaction<'_>, operation_id: &str, now: i64) -> Result<i64> {
    let lease = load_pending(tx, operation_id, "commit fix-cycle")?;
    ensure_not_terminal(&lease)?;
    let committed = lease
        .pending_fix_cycles
        .unwrap_or(recorded_fix_cycles(&lease));
    write_fix_commit(tx, operation_id, committed, now)?;
    Ok(committed)
}

fn load_pending(tx: &Transaction<'_>, operation_id: &str, context: &'static str) -> Result<Lease> {
    let row = query_lease_tx(
        tx,
        "WHERE pending_fix_op_id = ?1",
        params![operation_id],
        "lookup pending fix-cycle",
    )?;
    require_lease(
        row,
        context,
        format!("unknown pending fix-cycle {operation_id}"),
    )
}

fn write_fix_commit(
    tx: &Transaction<'_>,
    operation_id: &str,
    committed: i64,
    now: i64,
) -> Result<()> {
    tx.execute(COMMIT_FIX_SQL, params![committed, now, operation_id])
        .map_err(|e| lease_err("commit fix-cycle", e))?;
    if tx.changes() != 1 {
        return Err(store_error(
            "commit fix-cycle",
            "released or tombstoned lease cannot commit fix_cycles",
        ));
    }
    update_op_phase(
        tx,
        OpPhase {
            operation_id,
            phase: "COMMIT",
            status: "COMMITTED",
            now,
        },
    )
}

fn clear_pending_fix(tx: &Transaction<'_>, operation_id: &str, now: i64) -> Result<()> {
    tx.execute(ABORT_FIX_SQL, params![now, operation_id])
        .map_err(|e| lease_err("abort fix-cycle", e))?;
    Ok(())
}
