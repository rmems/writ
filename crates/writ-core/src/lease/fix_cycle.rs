//! Pending `fix_cycles` journal.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::query::{LeaseLookup, lookup_lease};
use super::{
    AllocationState, Error, FixCycleReconcile, FixCycleToken, JobKey, Lease, LeaseStore, Result,
    lease_err, new_operation_id, now_secs, schema, terminal_error,
};

impl LeaseStore {
    /// Persist intent to increment `fix_cycles` before the authorized mutation.
    pub fn prepare_fix_cycle(&self, key: JobKey<'_>) -> Result<FixCycleToken> {
        let now = now_secs();
        let operation_id = new_operation_id();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle prepare", e))?;
        let lease = lookup_lease(
            &tx,
            LeaseLookup {
                where_sql: "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
                sql_params: params![key.owner, key.repo_name, key.job_id],
                context: "lookup lease for fix-cycle",
            },
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "prepare fix-cycle",
            message: "lease not found".to_owned(),
        })?;
        require_active_fix_cycle(&lease)?;
        let authorized = lease.fix_cycles.unwrap_or(0) + 1;
        write_pending_fix(
            &tx,
            PendingFix {
                key,
                lease: &lease,
                operation_id: &operation_id,
                now,
                authorized,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle prepare", e))?;
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
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle commit", e))?;
        let lease = pending_fix_lease(&tx, operation_id, "commit fix-cycle")?;
        if lease.allocation_state.is_terminal() {
            return Err(terminal_error(&lease));
        }
        let committed = lease
            .pending_fix_cycles
            .unwrap_or(lease.fix_cycles.unwrap_or(0));
        tx.execute(
            "
            UPDATE leases
            SET fix_cycles = ?1, pending_fix_cycles = NULL, pending_fix_op_id = NULL,
                updated_at = ?2
            WHERE pending_fix_op_id = ?3 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![committed, now, operation_id],
        )
        .map_err(|e| lease_err("commit fix-cycle", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "commit fix-cycle",
                message: "released or tombstoned lease cannot commit fix_cycles".to_owned(),
            });
        }
        schema::update_op_phase(
            &tx,
            schema::OpPhase {
                operation_id,
                phase: "COMMIT",
                status: "COMMITTED",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle commit", e))?;
        Ok(committed)
    }

    fn set_fix_cycle_phase(&self, operation_id: &str, phase: &'static str) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle phase", e))?;
        let lease = pending_fix_lease(&tx, operation_id, "advance fix-cycle")?;
        if lease.allocation_state.is_terminal() {
            return Err(terminal_error(&lease));
        }
        schema::update_op_phase(
            &tx,
            schema::OpPhase {
                operation_id,
                phase,
                status: "IN_PROGRESS",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle phase", e))?;
        Ok(())
    }

    /// Recover a pending `fix_cycles` increment without double-counting or losing it.
    pub fn reconcile_fix_cycle(
        &self,
        key: JobKey<'_>,
        mutation_proven: Option<bool>,
    ) -> Result<FixCycleReconcile> {
        let lease = self.find_job(key)?.ok_or_else(|| Error::LeaseStore {
            context: "reconcile fix-cycle",
            message: "lease not found".to_owned(),
        })?;
        let Some(operation_id) = lease.pending_fix_op_id.clone() else {
            return Ok(FixCycleReconcile::Idle {
                fix_cycles: lease.fix_cycles.unwrap_or(0),
            });
        };
        let phase = self.op_phase(&operation_id)?;
        match mutation_proven {
            Some(true) => {
                let fix_cycles = self.commit_fix_cycle(&operation_id)?;
                Ok(FixCycleReconcile::Committed { fix_cycles })
            }
            Some(false) => {
                if self.abort_fix_cycle(&operation_id)? {
                    Ok(FixCycleReconcile::Aborted {
                        fix_cycles: lease.fix_cycles.unwrap_or(0),
                    })
                } else {
                    Ok(FixCycleReconcile::NeedsAttention {
                        pending: lease.pending_fix_cycles.unwrap_or(0),
                        operation_id,
                    })
                }
            }
            None if phase.as_deref() == Some("PREPARE") => {
                if self.abort_fix_cycle(&operation_id)? {
                    Ok(FixCycleReconcile::Aborted {
                        fix_cycles: lease.fix_cycles.unwrap_or(0),
                    })
                } else {
                    Ok(FixCycleReconcile::NeedsAttention {
                        pending: lease.pending_fix_cycles.unwrap_or(0),
                        operation_id,
                    })
                }
            }
            None => Ok(FixCycleReconcile::NeedsAttention {
                pending: lease.pending_fix_cycles.unwrap_or(0),
                operation_id,
            }),
        }
    }

    fn abort_fix_cycle(&self, operation_id: &str) -> Result<bool> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle abort", e))?;
        tx.execute(
            "
            UPDATE allocation_ops
            SET phase = 'PREPARE', status = 'ABORTED', updated_at = ?1
            WHERE operation_id = ?2 AND phase = 'PREPARE'
            ",
            params![now, operation_id],
        )
        .map_err(|e| lease_err("abort fix-cycle", e))?;
        if tx.changes() != 1 {
            return Ok(false);
        }
        tx.execute(
            "
            UPDATE leases
            SET pending_fix_cycles = NULL, pending_fix_op_id = NULL, updated_at = ?1
            WHERE pending_fix_op_id = ?2 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![now, operation_id],
        )
        .map_err(|e| lease_err("abort fix-cycle", e))?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle abort", e))?;
        Ok(true)
    }

    fn op_phase(&self, operation_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT phase FROM allocation_ops WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| lease_err("lookup operation phase", e))
    }
}

struct PendingFix<'a> {
    key: JobKey<'a>,
    lease: &'a Lease,
    operation_id: &'a str,
    now: i64,
    authorized: i64,
}

fn write_pending_fix(tx: &rusqlite::Transaction<'_>, row: PendingFix<'_>) -> Result<()> {
    tx.execute(
        "
            UPDATE leases
            SET pending_fix_cycles = ?1, pending_fix_op_id = ?2, updated_at = ?3
            WHERE owner = ?4 AND repo_name = ?5 AND job_id = ?6
              AND allocation_state = 'ACTIVE' AND released_at IS NULL AND tombstoned_at IS NULL
            ",
        params![
            row.authorized,
            row.operation_id,
            row.now,
            row.key.owner,
            row.key.repo_name,
            row.key.job_id
        ],
    )
    .map_err(|e| lease_err("prepare fix-cycle", e))?;
    if tx.changes() != 1 {
        return Err(Error::LeaseStore {
            context: "prepare fix-cycle",
            message: "released or tombstoned lease cannot increment fix_cycles".to_owned(),
        });
    }
    schema::insert_op(
        tx,
        schema::OpRecord {
            operation_id: row.operation_id,
            owner: row.key.owner,
            repo_name: row.key.repo_name,
            job_id: row.key.job_id,
            kind: "INCREMENT_FIX_CYCLE",
            phase: "PREPARE",
            requested_start_point: Some(&row.lease.requested_start_point),
            resolved_start_commit: Some(&row.lease.start_commit),
            status: "IN_PROGRESS",
            now: row.now,
        },
    )
}

fn pending_fix_lease(
    tx: &rusqlite::Transaction<'_>,
    operation_id: &str,
    context: &'static str,
) -> Result<Lease> {
    lookup_lease(
        tx,
        LeaseLookup {
            where_sql: "WHERE pending_fix_op_id = ?1",
            sql_params: params![operation_id],
            context: "lookup pending fix-cycle",
        },
    )?
    .ok_or_else(|| Error::LeaseStore {
        context,
        message: format!("unknown pending fix-cycle {operation_id}"),
    })
}

fn require_active_fix_cycle(lease: &Lease) -> Result<()> {
    if lease.allocation_state != AllocationState::Active {
        return Err(Error::LeaseStore {
            context: "prepare fix-cycle",
            message: format!(
                "fix_cycles increment requires ACTIVE lease, found {}",
                lease.allocation_state.as_str()
            ),
        });
    }
    if lease.pending_fix_op_id.is_some() {
        return Err(Error::LeaseStore {
            context: "prepare fix-cycle",
            message: "a pending fix_cycles increment already exists; reconcile it first".to_owned(),
        });
    }
    Ok(())
}
