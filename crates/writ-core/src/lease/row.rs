//! Lease row decoding and keyed lookups.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::error::Result;

use super::LeaseStore;
use super::types::{AllocationState, JobKey, Lease, LeaseMode};
use super::util::{lease_err, path_text};

pub(in crate::lease) const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, \
     worktree_path, requested_start_point, start_commit, operation_id, \
     allocation_state, mode, ttl, heartbeat, max_files, max_churn, \
     max_fix_cycles, fix_cycles, pending_fix_cycles, pending_fix_op_id, \
     created_at, updated_at, released_at, tombstoned_at FROM leases";

impl LeaseStore {
    /// Look up a lease by owner/repo/job, including released and tombstoned rows.
    pub fn find_job(&self, key: JobKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![key.owner, key.repo_name, key.job_id],
            "lookup lease",
        )
    }

    /// Look up a lease by worktree path.
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE worktree_path = ?1",
            params![path_text(worktree_path)],
            "lookup lease by path",
        )
    }

    /// Look up a lease by operation identity.
    pub fn find_by_operation(&self, operation_id: &str) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE operation_id = ?1",
            params![operation_id],
            "lookup lease by operation",
        )
    }

    fn query_lease(
        &self,
        where_sql: &str,
        sql_params: impl rusqlite::Params,
        context: &'static str,
    ) -> Result<Option<Lease>> {
        let conn = self.lock()?;
        query_lease_locked(&conn, where_sql, sql_params, context)
    }

    pub(in crate::lease) fn op_phase(&self, operation_id: &str) -> Result<Option<String>> {
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

pub(in crate::lease) fn query_lease_tx(
    tx: &Transaction<'_>,
    where_sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<Option<Lease>> {
    let query = format!("{LEASE_SELECT} {where_sql}");
    tx.query_row(&query, sql_params, lease_from_row)
        .optional()
        .map_err(|e| lease_err(context, e))
}

fn query_lease_locked(
    conn: &Connection,
    where_sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<Option<Lease>> {
    let query = format!("{LEASE_SELECT} {where_sql}");
    conn.query_row(&query, sql_params, lease_from_row)
        .optional()
        .map_err(|e| lease_err(context, e))
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let (repo, owner, repo_name, job_id) = row_names(row)?;
    let (branch, branch_ref) = row_refs(row)?;
    let (worktree_path, requested_start_point, start_commit, operation_id) = row_binding(row)?;
    let (allocation_state, mode) = row_state(row)?;
    let limits = row_limits(row)?;
    let counters = row_counters(row)?;
    Ok(Lease {
        repo,
        owner,
        repo_name,
        job_id,
        branch,
        branch_ref,
        worktree_path,
        requested_start_point,
        start_commit,
        operation_id,
        allocation_state,
        mode,
        ttl: limits.ttl,
        heartbeat: limits.heartbeat,
        max_files: limits.max_files,
        max_churn: limits.max_churn,
        max_fix_cycles: limits.max_fix_cycles,
        fix_cycles: counters.fix_cycles,
        pending_fix_cycles: counters.pending_fix_cycles,
        pending_fix_op_id: counters.pending_fix_op_id,
        released_at: counters.released_at,
        tombstoned_at: counters.tombstoned_at,
    })
}

fn row_names(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn row_refs(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get(4)?, row.get(5)?))
}

fn row_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, String)> {
    Ok((row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?))
}

fn row_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<(AllocationState, LeaseMode)> {
    let allocation = row.get::<_, String>(10)?;
    let mode = row.get::<_, String>(11)?;
    Ok((AllocationState::parse(&allocation), LeaseMode::parse(&mode)))
}

struct RowLimits {
    ttl: Option<i64>,
    heartbeat: Option<i64>,
    max_files: Option<i64>,
    max_churn: Option<i64>,
    max_fix_cycles: Option<i64>,
}

struct RowCounters {
    fix_cycles: Option<i64>,
    pending_fix_cycles: Option<i64>,
    pending_fix_op_id: Option<String>,
    released_at: Option<i64>,
    tombstoned_at: Option<i64>,
}

fn row_limits(row: &rusqlite::Row<'_>) -> rusqlite::Result<RowLimits> {
    Ok(RowLimits {
        ttl: row.get(12)?,
        heartbeat: row.get(13)?,
        max_files: row.get(14)?,
        max_churn: row.get(15)?,
        max_fix_cycles: row.get(16)?,
    })
}

fn row_counters(row: &rusqlite::Row<'_>) -> rusqlite::Result<RowCounters> {
    Ok(RowCounters {
        fix_cycles: row.get(17)?,
        pending_fix_cycles: row.get(18)?,
        pending_fix_op_id: row.get(19)?,
        released_at: row.get(22)?,
        tombstoned_at: row.get(23)?,
    })
}
