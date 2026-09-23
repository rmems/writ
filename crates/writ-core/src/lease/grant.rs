//! Grant, release, and tombstone writer locks.

use std::path::Path;

use rusqlite::{TransactionBehavior, params};

use super::query::{LIVE_PATH_LOOKUP, LeaseLookup, lookup_lease};
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
        let existing = lookup_lease(
            &tx,
            LeaseLookup {
                where_sql: "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
                sql_params: params![grant.owner, grant.repo_name, grant.job_id],
                context: "lookup lease before grant",
            },
        )?;
        reject_tombstoned(existing.as_ref())?;
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
        clear_released_claim(&tx, &grant, existing.as_ref())?;
        apply_grant_upsert(
            &tx,
            GrantRow {
                grant: &grant,
                repo: &repo,
                branch_ref: &branch_ref,
                worktree_path: &worktree_path,
                operation_id: &operation_id,
                now,
            },
        )?;
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
        let existing = lookup_lease(
            &tx,
            LeaseLookup {
                where_sql: LIVE_PATH_LOOKUP,
                sql_params: params![path],
                context: "lookup lease by path",
            },
        )?;
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
        write_finalize(
            &tx,
            FinalizeWrite {
                lease: &lease,
                state,
                released_at,
                tombstoned_at,
                kind,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit lease finalize", e))?;
        drop(conn);
        self.find_by_path(worktree_path)
    }
}

struct FinalizeWrite<'a> {
    lease: &'a Lease,
    state: AllocationState,
    released_at: Option<i64>,
    tombstoned_at: Option<i64>,
    kind: &'static str,
    now: i64,
}

fn write_finalize(tx: &rusqlite::Transaction<'_>, row: FinalizeWrite<'_>) -> Result<()> {
    tx.execute(
        "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, released_at = ?3, tombstoned_at = ?4,
                updated_at = ?5
            WHERE id = ?6 AND tombstoned_at IS NULL
            ",
        params![
            row.state.as_str(),
            LeaseMode::Unassigned.as_str(),
            row.released_at,
            row.tombstoned_at,
            row.now,
            row.lease.row_id,
        ],
    )
    .map_err(|e| lease_err("finalize lease", e))?;
    schema::insert_op(
        tx,
        schema::OpRecord {
            operation_id: &format!(
                "{}-{}",
                row.lease.operation_id,
                row.kind.to_ascii_lowercase()
            ),
            owner: &row.lease.owner,
            repo_name: &row.lease.repo_name,
            job_id: &row.lease.job_id,
            kind: row.kind,
            phase: "COMMIT",
            requested_start_point: Some(&row.lease.requested_start_point),
            resolved_start_commit: Some(&row.lease.start_commit),
            status: "COMMITTED",
            now: row.now,
        },
    )
}

fn reject_tombstoned(existing: Option<&Lease>) -> Result<()> {
    if let Some(existing) = existing
        && existing.allocation_state == AllocationState::Tombstoned
    {
        return Err(terminal_error(existing));
    }
    Ok(())
}

fn clear_released_claim(
    tx: &rusqlite::Transaction<'_>,
    grant: &LeaseGrant<'_>,
    existing: Option<&Lease>,
) -> Result<()> {
    if !existing.is_some_and(|lease| lease.allocation_state == AllocationState::Released) {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM coord_claims WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
        params![grant.owner, grant.repo_name, grant.job_id],
    )
    .map_err(|e| lease_err("clear stale coord claim on regrant", e))?;
    Ok(())
}

struct GrantRow<'a> {
    grant: &'a LeaseGrant<'a>,
    repo: &'a str,
    branch_ref: &'a str,
    worktree_path: &'a str,
    operation_id: &'a str,
    now: i64,
}

fn apply_grant_upsert(tx: &rusqlite::Transaction<'_>, row: GrantRow<'_>) -> Result<()> {
    let grant = row.grant;
    let changed = tx
        .execute(
            schema::GRANT_UPSERT,
            params![
                row.repo,
                grant.owner,
                grant.repo_name,
                grant.job_id,
                grant.branch,
                row.branch_ref,
                row.worktree_path,
                row.branch_ref,
                grant.start_commit,
                row.operation_id,
                AllocationState::Active.as_str(),
                LeaseMode::WriterLocked.as_str(),
                row.now,
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
    Ok(())
}
