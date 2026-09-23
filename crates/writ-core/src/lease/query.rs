//! Lease row queries.

use rusqlite::{Connection, OptionalExtension};

use super::{AgentRecord, AllocationState, Lease, LeaseMode, Result, lease_err};

pub(super) const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, \
     worktree_path, requested_start_point, start_commit, operation_id, \
     allocation_state, mode, ttl, heartbeat, max_files, max_churn, \
     max_fix_cycles, fix_cycles, pending_fix_cycles, pending_fix_op_id, \
     created_at, updated_at, released_at, tombstoned_at, id FROM leases";

pub(super) const LIVE_PATH_LOOKUP: &str = "WHERE worktree_path = ?1
                ORDER BY CASE
                    WHEN released_at IS NULL AND tombstoned_at IS NULL THEN 0
                    ELSE 1
                END,
                COALESCE(released_at, tombstoned_at, 0) DESC,
                id DESC
                LIMIT 1";

pub(super) fn query_lease_locked(
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

pub(crate) fn query_lease_tx(
    tx: &rusqlite::Transaction<'_>,
    where_sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<Option<Lease>> {
    let query = format!("{LEASE_SELECT} {where_sql}");
    tx.query_row(&query, sql_params, lease_from_row)
        .optional()
        .map_err(|e| lease_err(context, e))
}

pub(super) fn list_leases_on(
    conn: &Connection,
    suffix: &str,
    context: &'static str,
) -> Result<Vec<Lease>> {
    let query = format!("{LEASE_SELECT} {suffix}");
    let mut stmt = conn.prepare(&query).map_err(|e| lease_err(context, e))?;
    let rows = stmt
        .query_map([], lease_from_row)
        .map_err(|e| lease_err(context, e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err(context, e))
}

pub(super) fn list_agents_on(conn: &Connection) -> Result<Vec<AgentRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT agent_id, agent_type, session_id, started_at, stopped_at
             FROM agents
             ORDER BY (stopped_at IS NOT NULL), started_at, agent_id",
        )
        .map_err(|e| lease_err("list agents", e))?;
    let rows = stmt
        .query_map([], agent_from_row)
        .map_err(|e| lease_err("list agents", e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err("list agents", e))
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let mode_raw: String = row.get(11)?;
    Ok(Lease {
        repo: row.get(0)?,
        owner: row.get(1)?,
        repo_name: row.get(2)?,
        job_id: row.get(3)?,
        branch: row.get(4)?,
        branch_ref: row.get(5)?,
        worktree_path: row.get(6)?,
        requested_start_point: row.get(7)?,
        start_commit: row.get(8)?,
        operation_id: row.get(9)?,
        allocation_state: AllocationState::parse(&row.get::<_, String>(10)?),
        mode: LeaseMode::parse(&mode_raw),
        mode_raw,
        ttl: row.get(12)?,
        heartbeat: row.get(13)?,
        max_files: row.get(14)?,
        max_churn: row.get(15)?,
        max_fix_cycles: row.get(16)?,
        fix_cycles: row.get(17)?,
        pending_fix_cycles: row.get(18)?,
        pending_fix_op_id: row.get(19)?,
        released_at: row.get(22)?,
        tombstoned_at: row.get(23)?,
        row_id: row.get(24)?,
        created_at: row.get(20)?,
        updated_at: row.get(21)?,
    })
}

fn agent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    Ok(AgentRecord {
        agent_id: row.get(0)?,
        agent_type: row.get(1)?,
        session_id: row.get(2)?,
        started_at: row.get(3)?,
        stopped_at: row.get(4)?,
    })
}
