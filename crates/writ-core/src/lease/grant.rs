//! Grant, release, and tombstone writer locks.

use std::path::Path;

use rusqlite::{TransactionBehavior, params};

use super::query::{LIVE_PATH_LOOKUP, query_lease_tx};
use super::{
    AllocationState, Error, JobKey, Lease, LeaseGrant, LeaseMode, LeaseStore, PolicyCode, Result,
    lease_err, new_operation_id, now_secs, occupant, path_text, schema, terminal_error,
};

impl LeaseStore {
    pub fn grant(&self, grant: LeaseGrant<'_>) -> Result<Lease> {
        let now = now_secs();
        let repo = path_text(grant.repo);
        let worktree_path = path_text(grant.worktree_path);
        let branch_ref = format!("refs/heads/{}", grant.branch);
        let operation_id = new_operation_id();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin grant", e))?;
        if let Some(existing) = query_lease_tx(
            &tx,
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![grant.owner, grant.repo_name, grant.job_id],
            "lookup lease before grant",
        )? && existing.allocation_state == AllocationState::Tombstoned
        {
            return Err(terminal_error(&existing));
        }
        occupant::reject_live_path_occupant(
            &tx,
            occupant::LivePathClaim {
                worktree_path: &worktree_path,
                owner: grant.owner,
                repo_name: grant.repo_name,
                job_id: grant.job_id,
            },
            "grant lease",
        )?;
        let changed = tx
            .execute(
                schema::GRANT_UPSERT,
                params![
                    repo,
                    grant.owner,
                    grant.repo_name,
                    grant.job_id,
                    grant.branch,
                    branch_ref,
                    worktree_path,
                    grant.start_commit,
                    operation_id,
                    AllocationState::Active.as_str(),
                    LeaseMode::WriterLocked.as_str(),
                    now,
                ],
            )
            .map_err(|e| occupant::map_live_path_constraint(e, "grant lease"))?;
        if changed == 0 {
            return Err(Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                message: format!(
                    "job `{}` already holds an active lease for a different worktree path",
                    grant.job_id
                ),
            });
        }
        tx.commit().map_err(|e| lease_err("commit grant", e))?;
        drop(conn);
        self.find_job(JobKey {
            owner: grant.owner,
            repo_name: grant.repo_name,
            job_id: grant.job_id,
        })?
        .ok_or_else(|| Error::LeaseStore {
            context: "grant lease",
            message: "lease row missing after grant".to_owned(),
        })
    }

    /// Durable resume identity for an owner/repo/job/branch, including released rows.
    pub fn release_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Released)
    }

    /// Permanently tombstone a lease so reconcile cannot revive it.
    pub fn tombstone_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Tombstoned)
    }

    fn finalize_by_path(
        &self,
        worktree_path: &Path,
        state: AllocationState,
    ) -> Result<Option<Lease>> {
        let now = now_secs();
        let path = path_text(worktree_path);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin lease finalize", e))?;
        let existing =
            query_lease_tx(&tx, LIVE_PATH_LOOKUP, params![path], "lookup lease by path")?;
        let Some(lease) = existing else {
            tx.commit()
                .map_err(|e| lease_err("commit lease finalize", e))?;
            return Ok(None);
        };
        if lease.allocation_state == AllocationState::Tombstoned || lease.allocation_state == state
        {
            tx.commit()
                .map_err(|e| lease_err("commit lease finalize", e))?;
            drop(conn);
            return self.find_by_path(worktree_path);
        }
        let (released_at, tombstoned_at, kind) = if state == AllocationState::Tombstoned {
            (lease.released_at, Some(now), "TOMBSTONE")
        } else {
            (Some(now), None, "RELEASE")
        };
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, released_at = ?3, tombstoned_at = ?4,
                updated_at = ?5
            WHERE id = ?6 AND tombstoned_at IS NULL
            ",
            params![
                state.as_str(),
                LeaseMode::Unassigned.as_str(),
                released_at,
                tombstoned_at,
                now,
                lease.row_id,
            ],
        )
        .map_err(|e| lease_err("finalize lease", e))?;
        schema::insert_op(
            &tx,
            schema::OpRecord {
                operation_id: &format!("{}-{}", lease.operation_id, kind.to_ascii_lowercase()),
                owner: &lease.owner,
                repo_name: &lease.repo_name,
                job_id: &lease.job_id,
                kind,
                phase: "COMMIT",
                requested_start_point: Some(&lease.requested_start_point),
                resolved_start_commit: Some(&lease.start_commit),
                status: "COMMITTED",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit lease finalize", e))?;
        drop(conn);
        self.find_by_path(worktree_path)
    }
}
