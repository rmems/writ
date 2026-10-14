//! Immediate transactions and the allocation-op journal.

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::error::Result;

use super::util::lease_err;

pub(in crate::lease) struct OpRecord<'a> {
    pub operation_id: &'a str,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub kind: &'a str,
    pub phase: &'a str,
    pub requested_start_point: Option<&'a str>,
    pub resolved_start_commit: Option<&'a str>,
    pub status: &'a str,
    pub now: i64,
}

pub(in crate::lease) struct OpPhase<'a> {
    pub operation_id: &'a str,
    pub phase: &'a str,
    pub status: &'a str,
    pub now: i64,
}

pub(in crate::lease) fn begin_immediate<'conn>(
    conn: &'conn mut Connection,
    context: &'static str,
) -> Result<Transaction<'conn>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| lease_err(context, e))
}

pub(in crate::lease) fn commit_tx(tx: Transaction<'_>, context: &'static str) -> Result<()> {
    tx.commit().map_err(|e| lease_err(context, e))?;
    Ok(())
}

pub(in crate::lease) fn commit_value<T>(
    tx: Transaction<'_>,
    context: &'static str,
    value: T,
) -> Result<T> {
    commit_tx(tx, context)?;
    Ok(value)
}

pub(in crate::lease) fn insert_op(tx: &Transaction<'_>, op: OpRecord<'_>) -> Result<()> {
    tx.execute(
        "
        INSERT INTO allocation_ops (
            operation_id, owner, repo_name, job_id, kind, phase,
            requested_start_point, resolved_start_commit, status, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
        ",
        params![
            op.operation_id,
            op.owner,
            op.repo_name,
            op.job_id,
            op.kind,
            op.phase,
            op.requested_start_point,
            op.resolved_start_commit,
            op.status,
            op.now,
        ],
    )
    .map_err(|e| lease_err("insert allocation op", e))?;
    Ok(())
}

pub(in crate::lease) fn update_op_phase(tx: &Transaction<'_>, op: OpPhase<'_>) -> Result<()> {
    tx.execute(
        "
        UPDATE allocation_ops
        SET phase = ?1, status = ?2, updated_at = ?3
        WHERE operation_id = ?4
        ",
        params![op.phase, op.status, op.now, op.operation_id],
    )
    .map_err(|e| lease_err("update allocation op", e))?;
    Ok(())
}
