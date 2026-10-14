//! Prepare, advance, and finalize allocation rows.

use std::path::Path;

use rusqlite::{Transaction, params};

use crate::error::{Error, PolicyCode, Result};

use super::LeaseStore;
use super::row::query_lease_tx;
use super::tx::{OpPhase, OpRecord, begin_immediate, commit_tx, insert_op, update_op_phase};
use super::types::{AllocateRequest, AllocationState, JobKey, Lease, LeaseMode};
use super::util::{
    ensure_not_terminal, lease_err, new_operation_id, now_secs, path_text, require_lease,
};

const INSERT_LEASE: &str = "
            INSERT INTO leases (
                repo, owner, repo_name, job_id, branch, branch_ref, worktree_path,
                requested_start_point, start_commit, operation_id, allocation_state,
                mode, ttl, heartbeat, max_files, max_churn, max_fix_cycles, fix_cycles,
                pending_fix_cycles, pending_fix_op_id, created_at, updated_at,
                released_at, tombstoned_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                NULL, NULL, NULL, 0, NULL, NULL, ?14, ?14, NULL, NULL
            )
            ";

const ADVANCE_SQL: &str = "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND tombstoned_at IS NULL AND released_at IS NULL
            ";

const FINALIZE_SQL: &str = "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, released_at = ?3, tombstoned_at = ?4,
                updated_at = ?5
            WHERE worktree_path = ?6 AND tombstoned_at IS NULL
            ";

struct Advance<'a> {
    operation_id: &'a str,
    next: AllocationState,
    mode: LeaseMode,
    phase: &'a str,
    now: i64,
}

struct FinalizeStamp {
    released_at: Option<i64>,
    tombstoned_at: Option<i64>,
    kind: &'static str,
}

enum FinalizeWrite {
    Missing,
    AlreadyTerminal,
    Updated,
}

impl LeaseStore {
    /// Persist requested + resolved identity before any git mutation.
    pub fn prepare_allocate(&self, request: AllocateRequest<'_>) -> Result<Lease> {
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        if let Some(existing) = self.find_job(key)? {
            return Err(prepare_blocked(existing));
        }
        self.insert_prepared(&request, key)
    }

    /// Record that git mutation is authorized for this operation.
    pub fn mark_mutating(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Mutating, "MUTATE")
    }

    /// Promote a matching allocation after git mutation succeeded.
    pub fn commit_allocate(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Active, "COMMIT")
    }

    /// Release a writer lock by worktree path without deleting identity.
    pub fn release_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Released)
    }

    /// Permanently tombstone a lease so reconcile cannot revive it.
    pub fn tombstone_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Tombstoned)
    }

    fn insert_prepared(&self, request: &AllocateRequest<'_>, key: JobKey<'_>) -> Result<Lease> {
        let now = now_secs();
        let operation_id = new_operation_id();
        self.write_prepared(request, &operation_id, now)?;
        self.prepared_row(key)
    }

    fn write_prepared(
        &self,
        request: &AllocateRequest<'_>,
        operation_id: &str,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin allocate prepare")?;
        insert_prepared_row(&tx, request, operation_id, now)?;
        insert_op(
            &tx,
            OpRecord {
                operation_id,
                owner: request.owner,
                repo_name: request.repo_name,
                job_id: request.job_id,
                kind: "ALLOCATE",
                phase: "PREPARE",
                requested_start_point: Some(request.requested_start_point),
                resolved_start_commit: Some(request.start_commit),
                status: "IN_PROGRESS",
                now,
            },
        )?;
        commit_tx(tx, "commit allocate prepare")
    }

    fn prepared_row(&self, key: JobKey<'_>) -> Result<Lease> {
        let row = self.find_job(key)?;
        require_lease(
            row,
            "prepare allocate",
            "lease row missing after prepare".to_owned(),
        )
    }

    fn advance_allocate(
        &self,
        operation_id: &str,
        next: AllocationState,
        phase: &'static str,
    ) -> Result<Lease> {
        self.write_advance(operation_id, next, phase)?;
        self.advanced_row(operation_id)
    }

    fn write_advance(
        &self,
        operation_id: &str,
        next: AllocationState,
        phase: &'static str,
    ) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin allocate advance")?;
        let current = load_operation(&tx, operation_id)?;
        ensure_not_terminal(&current)?;
        apply_advance(
            &tx,
            Advance {
                operation_id,
                next,
                mode: current.mode,
                phase,
                now,
            },
        )?;
        commit_tx(tx, "commit allocate advance")
    }

    fn advanced_row(&self, operation_id: &str) -> Result<Lease> {
        let row = self.find_by_operation(operation_id)?;
        require_lease(
            row,
            "advance allocate",
            "lease row missing after advance".to_owned(),
        )
    }

    fn finalize_by_path(
        &self,
        worktree_path: &Path,
        state: AllocationState,
    ) -> Result<Option<Lease>> {
        let outcome = self.finalize_row(worktree_path, state)?;
        self.finish_finalize(worktree_path, outcome)
    }

    fn finalize_row(&self, worktree_path: &Path, state: AllocationState) -> Result<FinalizeWrite> {
        let now = now_secs();
        let path = path_text(worktree_path);
        let mut conn = self.lock()?;
        let tx = begin_immediate(&mut conn, "begin lease finalize")?;
        let existing = query_lease_tx(
            &tx,
            "WHERE worktree_path = ?1",
            params![path],
            "lookup lease by path",
        )?;
        finalize_existing(
            tx,
            existing,
            FinalizePlan {
                path: &path,
                state,
                now,
            },
        )
    }

    fn finish_finalize(
        &self,
        worktree_path: &Path,
        outcome: FinalizeWrite,
    ) -> Result<Option<Lease>> {
        match outcome {
            FinalizeWrite::Missing => Ok(None),
            FinalizeWrite::AlreadyTerminal | FinalizeWrite::Updated => {
                self.find_by_path(worktree_path)
            }
        }
    }
}

fn insert_prepared_row(
    tx: &Transaction<'_>,
    request: &AllocateRequest<'_>,
    operation_id: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        INSERT_LEASE,
        params![
            path_text(request.repo),
            request.owner,
            request.repo_name,
            request.job_id,
            request.branch,
            format!("refs/heads/{}", request.branch),
            path_text(request.worktree_path),
            request.requested_start_point,
            request.start_commit,
            operation_id,
            AllocationState::Prepared.as_str(),
            LeaseMode::Unassigned.as_str(),
            request.ttl,
            now,
        ],
    )
    .map_err(|e| lease_err("prepare allocate", e))?;
    Ok(())
}

fn load_operation(tx: &Transaction<'_>, operation_id: &str) -> Result<Lease> {
    let row = query_lease_tx(
        tx,
        "WHERE operation_id = ?1",
        params![operation_id],
        "lookup allocate operation",
    )?;
    require_lease(
        row,
        "advance allocate",
        format!("unknown operation {operation_id}"),
    )
}

fn apply_advance(tx: &Transaction<'_>, advance: Advance<'_>) -> Result<()> {
    tx.execute(
        ADVANCE_SQL,
        params![
            advance.next.as_str(),
            advanced_mode(advance.next, advance.mode),
            advance.now,
            advance.operation_id,
        ],
    )
    .map_err(|e| lease_err("advance allocate", e))?;
    if tx.changes() != 1 {
        return Err(Error::LeaseStore {
            context: "advance allocate",
            message: "released or tombstoned lease cannot be advanced".to_owned(),
        });
    }
    update_op_phase(
        tx,
        OpPhase {
            operation_id: advance.operation_id,
            phase: advance.phase,
            status: advanced_status(advance.next),
            now: advance.now,
        },
    )
}

fn advanced_mode(next: AllocationState, current: LeaseMode) -> &'static str {
    if next == AllocationState::Active {
        LeaseMode::WriterLocked.as_str()
    } else {
        current.as_str()
    }
}

fn advanced_status(next: AllocationState) -> &'static str {
    if next == AllocationState::Active {
        "COMMITTED"
    } else {
        "IN_PROGRESS"
    }
}

struct FinalizePlan<'a> {
    path: &'a str,
    state: AllocationState,
    now: i64,
}

fn finalize_existing(
    tx: Transaction<'_>,
    existing: Option<Lease>,
    plan: FinalizePlan<'_>,
) -> Result<FinalizeWrite> {
    let Some(lease) = existing else {
        return commit_missing(tx);
    };
    if lease.allocation_state.is_terminal() {
        return commit_terminal(tx);
    }
    write_finalize(
        &tx,
        FinalizeRow {
            lease: &lease,
            path: plan.path,
            state: plan.state,
            now: plan.now,
        },
    )?;
    commit_tx(tx, "commit lease finalize")?;
    Ok(FinalizeWrite::Updated)
}

fn commit_missing(tx: Transaction<'_>) -> Result<FinalizeWrite> {
    commit_tx(tx, "commit lease finalize")?;
    Ok(FinalizeWrite::Missing)
}

fn commit_terminal(tx: Transaction<'_>) -> Result<FinalizeWrite> {
    commit_tx(tx, "commit lease finalize")?;
    Ok(FinalizeWrite::AlreadyTerminal)
}

struct FinalizeRow<'a> {
    lease: &'a Lease,
    path: &'a str,
    state: AllocationState,
    now: i64,
}

fn write_finalize(tx: &Transaction<'_>, row: FinalizeRow<'_>) -> Result<()> {
    let stamp = finalize_stamp(row.lease, row.state, row.now);
    tx.execute(
        FINALIZE_SQL,
        params![
            row.state.as_str(),
            LeaseMode::Unassigned.as_str(),
            stamp.released_at,
            stamp.tombstoned_at,
            row.now,
            row.path,
        ],
    )
    .map_err(|e| lease_err("finalize lease", e))?;
    insert_finalize_op(tx, row.lease, &stamp, row.now)
}

fn insert_finalize_op(
    tx: &Transaction<'_>,
    lease: &Lease,
    stamp: &FinalizeStamp,
    now: i64,
) -> Result<()> {
    insert_op(
        tx,
        OpRecord {
            operation_id: &format!("{}-{}", lease.operation_id, stamp.kind.to_ascii_lowercase()),
            owner: &lease.owner,
            repo_name: &lease.repo_name,
            job_id: &lease.job_id,
            kind: stamp.kind,
            phase: "COMMIT",
            requested_start_point: Some(&lease.requested_start_point),
            resolved_start_commit: Some(&lease.start_commit),
            status: "COMMITTED",
            now,
        },
    )
}

fn finalize_stamp(lease: &Lease, state: AllocationState, now: i64) -> FinalizeStamp {
    if state == AllocationState::Tombstoned {
        FinalizeStamp {
            released_at: lease.released_at,
            tombstoned_at: Some(now),
            kind: "TOMBSTONE",
        }
    } else {
        FinalizeStamp {
            released_at: Some(now),
            tombstoned_at: None,
            kind: "RELEASE",
        }
    }
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
