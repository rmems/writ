//! Reconcile promote / abort / attention steps.

use std::ops::ControlFlow;

use rusqlite::{Transaction, TransactionBehavior, params};

use super::query::query_lease_tx;
use super::{
    AllocationInspection, AllocationState, Error, EvidenceClass, InspectRequest, JobKey, Lease,
    LeaseMode, LeaseStore, ReconcileOutcome, Result, classify, lease_err, now_secs, schema,
    terminal_outcome,
};
use std::path::Path;

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
        let changed = apply_state(
            &tx,
            expected,
            now,
            StatePatch {
                next: AllocationState::Active,
                mode: LeaseMode::WriterLocked,
                set_heartbeat: true,
                context: "promote lease",
            },
        )?;
        if !changed {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(ReconcileOutcome::NeedsAttention {
                lease: current,
                inspection,
            });
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

    /// Report derived path, refs, HEAD, registration, and operation identity.
    ///
    /// This never mutates git state, never adopts a worktree, and never writes
    /// the lease store.
    pub fn inspect(&self, request: InspectRequest<'_>) -> Result<AllocationInspection> {
        let lease = self.find_job(JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        })?;
        Ok(classify::inspect_now(&lease, request))
    }

    /// Reconcile an interrupted allocation without destructive cleanup.
    pub fn reconcile(&self, key: JobKey<'_>, repo_root: &Path) -> Result<Option<ReconcileOutcome>> {
        let Some(lease) = self.find_job(key)? else {
            return Ok(None);
        };
        let request = InspectRequest {
            repo_root,
            owner: key.owner,
            repo_name: key.repo_name,
            job_id: key.job_id,
            worktree_path: Path::new(&lease.worktree_path),
            branch: Some(&lease.branch),
        };
        let inspection = classify::inspect_now(&Some(lease.clone()), request);
        if lease.allocation_state == AllocationState::Tombstoned {
            return Ok(Some(ReconcileOutcome::Tombstoned { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Released {
            return Ok(Some(ReconcileOutcome::Released { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Active {
            return Ok(Some(ReconcileOutcome::AlreadyActive { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Aborted {
            return Ok(Some(ReconcileOutcome::Retry {
                operation_id: lease.operation_id,
                inspection,
            }));
        }
        if lease.allocation_state == AllocationState::Unknown {
            return Ok(Some(ReconcileOutcome::NeedsAttention { lease, inspection }));
        }

        match inspection.classification {
            EvidenceClass::Matching => Ok(Some(self.promote_if_still_open(&lease, inspection)?)),
            EvidenceClass::Retryable => Ok(Some(
                self.recover_if_still_open(&lease, inspection, "abort")?,
            )),
            _ => Ok(Some(self.recover_if_still_open(
                &lease,
                inspection,
                "attention",
            )?)),
        }
    }

    fn recover_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
        label: &'static str,
    ) -> Result<ReconcileOutcome> {
        finish_open_recover(
            self,
            RecoverStep {
                expected,
                inspection,
                lookup: RecoverLookup::Operation,
                label,
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
        apply_state(
            &tx,
            step.expected,
            now,
            StatePatch {
                next: AllocationState::NeedsAttention,
                mode: LeaseMode::NeedsHuman,
                set_heartbeat: false,
                context: "mark needs attention",
            },
        )?
    };
    if !changed {
        tx.commit()
            .map_err(|e| lease_err(commit_ctx(step.label), e))?;
        return Ok(ReconcileOutcome::NeedsAttention {
            lease: current,
            inspection: step.inspection,
        });
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

struct StatePatch {
    next: AllocationState,
    mode: LeaseMode,
    set_heartbeat: bool,
    context: &'static str,
}

fn apply_state(
    tx: &Transaction<'_>,
    expected: &Lease,
    now: i64,
    patch: StatePatch,
) -> Result<bool> {
    let sql = if patch.set_heartbeat {
        "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            "
    } else {
        "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            "
    };
    tx.execute(
        sql,
        params![
            patch.next.as_str(),
            patch.mode.as_str(),
            now,
            expected.operation_id,
        ],
    )
    .map_err(|e| lease_err(patch.context, e))?;
    Ok(tx.changes() == 1)
}

fn apply_abort(tx: &Transaction<'_>, expected: &Lease) -> Result<bool> {
    tx.execute(
        "
            DELETE FROM leases
            WHERE operation_id = ?1
              AND allocation_state IN ('PREPARED', 'MUTATING', 'NEEDS_ATTENTION', 'ABORTED')
              AND released_at IS NULL AND tombstoned_at IS NULL
            ",
        params![expected.operation_id],
    )
    .map_err(|e| lease_err("abort reservation", e))?;
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
