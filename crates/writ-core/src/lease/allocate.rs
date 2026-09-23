//! PREPARE / MUTATE / COMMIT allocation journal.

use rusqlite::{TransactionBehavior, params};

use super::query::query_lease_tx;
use super::{
    AllocateRequest, AllocationState, Error, JobKey, Lease, LeaseMode, LeaseStore, PolicyCode,
    Result, lease_err, new_operation_id, now_secs, occupant, path_text, schema, terminal_error,
};

impl LeaseStore {
    pub fn prepare_allocate(&self, request: AllocateRequest<'_>) -> Result<Lease> {
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        if let Some(existing) = self.find_job(key)? {
            return Err(prepare_blocked(existing));
        }
        let now = now_secs();
        let operation_id = new_operation_id();
        let branch_ref = format!("refs/heads/{}", request.branch);
        let worktree_path = path_text(request.worktree_path);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin allocate prepare", e))?;
        occupant::reject_live_path_occupant(
            &tx,
            occupant::LivePathClaim {
                worktree_path: &worktree_path,
                owner: request.owner,
                repo_name: request.repo_name,
                job_id: request.job_id,
            },
            "prepare allocate",
        )?;
        insert_prepared_row(
            &tx,
            PreparedInsert {
                request,
                branch_ref: &branch_ref,
                worktree_path: &worktree_path,
                operation_id: &operation_id,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit allocate prepare", e))?;
        drop(conn);
        self.find_job(key)?.ok_or_else(|| Error::LeaseStore {
            context: "prepare allocate",
            message: "lease row missing after prepare".to_owned(),
        })
    }

    /// Record that git mutation is authorized for this operation.
    pub fn mark_mutating(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Mutating, "MUTATE")
    }

    /// Promote a matching allocation after git mutation succeeded.
    pub fn commit_allocate(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Active, "COMMIT")
    }

    fn advance_allocate(
        &self,
        operation_id: &str,
        next: AllocationState,
        phase: &'static str,
    ) -> Result<Lease> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin allocate advance", e))?;
        let current = query_lease_tx(
            &tx,
            "WHERE operation_id = ?1",
            params![operation_id],
            "lookup allocate operation",
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "advance allocate",
            message: format!("unknown operation {operation_id}"),
        })?;
        if current.allocation_state.is_terminal() {
            return Err(terminal_error(&current));
        }
        let expected = match (current.allocation_state, next) {
            (AllocationState::Prepared, AllocationState::Mutating) => AllocationState::Prepared,
            (AllocationState::Mutating, AllocationState::Active) => AllocationState::Mutating,
            _ => {
                return Err(Error::LeaseStore {
                    context: "advance allocate",
                    message: format!(
                        "invalid allocation transition {} -> {}",
                        current.allocation_state.as_str(),
                        next.as_str()
                    ),
                });
            }
        };
        let mode = if next == AllocationState::Active {
            LeaseMode::WriterLocked.as_str()
        } else {
            current.mode.as_str()
        };
        let status = if next == AllocationState::Active {
            "COMMITTED"
        } else {
            "IN_PROGRESS"
        };
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND allocation_state = ?5
              AND tombstoned_at IS NULL AND released_at IS NULL
            ",
            params![next.as_str(), mode, now, operation_id, expected.as_str()],
        )
        .map_err(|e| lease_err("advance allocate", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "advance allocate",
                message: "released or tombstoned lease cannot be advanced".to_owned(),
            });
        }
        schema::update_op_phase(
            &tx,
            schema::OpPhase {
                operation_id,
                phase,
                status,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit allocate advance", e))?;
        drop(conn);
        self.find_by_operation(operation_id)?
            .ok_or_else(|| Error::LeaseStore {
                context: "advance allocate",
                message: "lease row missing after advance".to_owned(),
            })
    }
}

struct PreparedInsert<'a> {
    request: AllocateRequest<'a>,
    branch_ref: &'a str,
    worktree_path: &'a str,
    operation_id: &'a str,
    now: i64,
}

fn insert_prepared_row(tx: &rusqlite::Transaction<'_>, row: PreparedInsert<'_>) -> Result<()> {
    let request = row.request;
    tx.execute(
        schema::PREPARE_INSERT,
        params![
            path_text(request.repo),
            request.owner,
            request.repo_name,
            request.job_id,
            request.branch,
            row.branch_ref,
            row.worktree_path,
            request.requested_start_point,
            request.start_commit,
            row.operation_id,
            AllocationState::Prepared.as_str(),
            LeaseMode::Unassigned.as_str(),
            request.ttl,
            row.now,
        ],
    )
    .map_err(|e| occupant::map_live_path_constraint(e, "prepare allocate"))?;
    schema::insert_op(
        tx,
        schema::OpRecord {
            operation_id: row.operation_id,
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
            kind: "ALLOCATE",
            phase: "PREPARE",
            requested_start_point: Some(request.requested_start_point),
            resolved_start_commit: Some(request.start_commit),
            status: "IN_PROGRESS",
            now: row.now,
        },
    )
}

fn prepare_blocked(existing: Lease) -> Error {
    match existing.allocation_state {
        AllocationState::Released => Error::PolicyViolation {
            code: PolicyCode::LeaseReleased,
            message: format!(
                "refusing to resurrect released lease for {}/{}/{}",
                existing.owner, existing.repo_name, existing.job_id
            ),
        },
        AllocationState::Tombstoned => Error::PolicyViolation {
            code: PolicyCode::LeaseTombstoned,
            message: format!(
                "refusing to resurrect tombstoned lease for {}/{}/{}",
                existing.owner, existing.repo_name, existing.job_id
            ),
        },
        _ => Error::LeaseStore {
            context: "prepare allocate",
            message: format!(
                "lease already exists in state {} (operation {})",
                existing.allocation_state.as_str(),
                existing.operation_id
            ),
        },
    }
}
