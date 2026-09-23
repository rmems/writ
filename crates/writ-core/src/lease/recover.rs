//! Reconcile promote / abort / attention steps.

use std::ops::ControlFlow;

use rusqlite::{Transaction, TransactionBehavior, params};

use super::query::query_lease_tx;
use super::{
    AllocationInspection, AllocationState, Error, Lease, LeaseMode, LeaseStore, ReconcileOutcome,
    Result, lease_err, now_secs, schema, terminal_outcome,
};

#[derive(Clone, Copy)]
enum RecoverLookup {
    Job,
    Operation,
}

impl LeaseStore {
    pub(super) fn promote_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_recover(&mut conn, "promote")?;
        let current = match load_open_lease(&tx, expected, &inspection, RecoverLookup::Job)? {
            ControlFlow::Break(outcome) => {
                tx.commit().map_err(|e| lease_err("commit promote", e))?;
                return Ok(outcome);
            }
            ControlFlow::Continue(current) => current,
        };
        if current.allocation_state == AllocationState::Active {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(ReconcileOutcome::AlreadyActive {
                lease: current,
                inspection,
            });
        }
        let changed = apply_promote(&tx, expected, now)?;
        if !changed {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        schema::update_op_phase(
            &tx,
            schema::OpPhase {
                operation_id: &expected.operation_id,
                phase: "COMMIT",
                status: "COMMITTED",
                now,
            },
        )?;
        tx.commit().map_err(|e| lease_err("commit promote", e))?;
        drop(conn);
        let lease = self
            .find_by_operation(&expected.operation_id)?
            .ok_or_else(|| Error::LeaseStore {
                context: "promote lease",
                message: "lease row missing after promote".to_owned(),
            })?;
        Ok(ReconcileOutcome::Promoted { lease, inspection })
    }

    pub(super) fn abort_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        finish_open_recover(
            self,
            RecoverStep {
                expected,
                inspection,
                lookup: RecoverLookup::Operation,
                label: "abort",
            },
        )
    }

    pub(super) fn attention_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        finish_open_recover(
            self,
            RecoverStep {
                expected,
                inspection,
                lookup: RecoverLookup::Operation,
                label: "attention",
            },
        )
    }
}

struct RecoverStep<'a> {
    expected: &'a Lease,
    inspection: AllocationInspection,
    lookup: RecoverLookup,
    label: &'static str,
}

fn finish_open_recover(store: &LeaseStore, step: RecoverStep<'_>) -> Result<ReconcileOutcome> {
    let now = now_secs();
    let mut conn = store.lock()?;
    let tx = begin_recover(&mut conn, step.label)?;
    let current = match load_open_lease(&tx, step.expected, &step.inspection, step.lookup)? {
        ControlFlow::Break(outcome) => {
            tx.commit()
                .map_err(|e| lease_err(commit_ctx(step.label), e))?;
            return Ok(outcome);
        }
        ControlFlow::Continue(current) => current,
    };
    let changed = if step.label == "abort" {
        apply_abort(&tx, step.expected)?
    } else {
        apply_attention(&tx, step.expected, now)?
    };
    if !changed {
        tx.commit()
            .map_err(|e| lease_err(commit_ctx(step.label), e))?;
        return Ok(terminal_outcome(current, step.inspection));
    }
    write_recover_op(&tx, &step, now)?;
    tx.commit()
        .map_err(|e| lease_err(commit_ctx(step.label), e))?;
    if step.label == "abort" {
        return Ok(ReconcileOutcome::Retry {
            operation_id: step.expected.operation_id.clone(),
            inspection: step.inspection,
        });
    }
    drop(conn);
    let lease = store
        .find_by_operation(&step.expected.operation_id)?
        .ok_or_else(|| Error::LeaseStore {
            context: "mark needs attention",
            message: "lease row missing after attention".to_owned(),
        })?;
    Ok(ReconcileOutcome::NeedsAttention {
        lease,
        inspection: step.inspection,
    })
}

fn write_recover_op(tx: &Transaction<'_>, step: &RecoverStep<'_>, now: i64) -> Result<()> {
    let (phase, status) = if step.label == "abort" {
        ("PREPARE", "ABORTED")
    } else {
        ("MUTATE", "NEEDS_ATTENTION")
    };
    schema::update_op_phase(
        tx,
        schema::OpPhase {
            operation_id: &step.expected.operation_id,
            phase,
            status,
            now,
        },
    )
}

fn begin_recover<'c>(
    conn: &'c mut rusqlite::Connection,
    label: &'static str,
) -> Result<Transaction<'c>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| lease_err(begin_ctx(label), e))
}

fn load_open_lease(
    tx: &Transaction<'_>,
    expected: &Lease,
    inspection: &AllocationInspection,
    lookup: RecoverLookup,
) -> Result<ControlFlow<ReconcileOutcome, Lease>> {
    let (where_sql, ctx) = match lookup {
        RecoverLookup::Job => (
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            "re-read lease before promote",
        ),
        RecoverLookup::Operation => ("WHERE operation_id = ?1", "re-read lease before recover"),
    };
    let current = match lookup {
        RecoverLookup::Job => query_lease_tx(
            tx,
            where_sql,
            params![expected.owner, expected.repo_name, expected.job_id],
            ctx,
        )?,
        RecoverLookup::Operation => {
            query_lease_tx(tx, where_sql, params![expected.operation_id], ctx)?
        }
    };
    Ok(match current {
        None => ControlFlow::Break(ReconcileOutcome::Retry {
            operation_id: expected.operation_id.clone(),
            inspection: inspection.clone(),
        }),
        Some(current) if current.allocation_state.is_terminal() => {
            ControlFlow::Break(terminal_outcome(current, inspection.clone()))
        }
        Some(current) => ControlFlow::Continue(current),
    })
}

fn apply_promote(tx: &Transaction<'_>, expected: &Lease, now: i64) -> Result<bool> {
    tx.execute(
        "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
        params![
            AllocationState::Active.as_str(),
            LeaseMode::WriterLocked.as_str(),
            now,
            expected.operation_id,
        ],
    )
    .map_err(|e| lease_err("promote lease", e))?;
    Ok(tx.changes() == 1)
}

fn apply_abort(tx: &Transaction<'_>, expected: &Lease) -> Result<bool> {
    tx.execute(
        "
            DELETE FROM leases
            WHERE operation_id = ?1
              AND allocation_state IN ('PREPARED', 'MUTATING', 'ABORTED')
              AND released_at IS NULL AND tombstoned_at IS NULL
            ",
        params![expected.operation_id],
    )
    .map_err(|e| lease_err("abort reservation", e))?;
    Ok(tx.changes() == 1)
}

fn apply_attention(tx: &Transaction<'_>, expected: &Lease, now: i64) -> Result<bool> {
    tx.execute(
        "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
        params![
            AllocationState::NeedsAttention.as_str(),
            LeaseMode::NeedsHuman.as_str(),
            now,
            expected.operation_id,
        ],
    )
    .map_err(|e| lease_err("mark needs attention", e))?;
    Ok(tx.changes() == 1)
}

fn begin_ctx(label: &'static str) -> &'static str {
    match label {
        "promote" => "begin promote",
        "abort" => "begin abort",
        _ => "begin attention",
    }
}

fn commit_ctx(label: &'static str) -> &'static str {
    match label {
        "promote" => "commit promote",
        "abort" => "commit abort",
        _ => "commit attention",
    }
}
