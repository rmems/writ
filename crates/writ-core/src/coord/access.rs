//! Claim/message row access and ACK transfer helpers.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, PolicyCode, Result};
use crate::lease::{AgentIdentity, AllocationState, JobKey, Lease, LeaseStore};

use super::declared_paths::{decode_paths_row, encode_paths};
use super::types::{CoordClaim, CoordMessage, MessageKind};
use super::util::{coord_err, coord_missing, held_error};

pub(super) const CLAIM_SELECT: &str = "SELECT c.owner, c.repo_name, c.job_id, c.branch, c.worktree_path, \
     c.agent_id, c.session_id, c.intent, c.declared_paths, c.owner_generation, \
     c.paused_at, l.allocation_state \
     FROM coord_claims c \
     JOIN leases l ON l.owner = c.owner AND l.repo_name = c.repo_name AND l.job_id = c.job_id";

pub(super) const MESSAGE_SELECT: &str = "SELECT id, created_at, kind, from_agent_id, from_owner, \
     from_repo_name, from_job_id, to_agent_id, to_owner, to_repo_name, to_job_id, \
     owner_generation, body, paths, ack_of, acked_at FROM coord_messages";

pub(super) struct NewMessage<'a> {
    pub(super) kind: MessageKind,
    pub(super) from_agent_id: &'a str,
    pub(super) from_owner: &'a str,
    pub(super) from_repo_name: &'a str,
    pub(super) from_job_id: &'a str,
    pub(super) to_agent_id: Option<&'a str>,
    pub(super) to_owner: Option<&'a str>,
    pub(super) to_repo_name: Option<&'a str>,
    pub(super) to_job_id: Option<&'a str>,
    pub(super) owner_generation: i64,
    pub(super) body: &'a str,
    pub(super) paths: &'a [String],
    pub(super) ack_of: Option<i64>,
    pub(super) now: i64,
}

pub(super) fn require_active_lease_tx(
    tx: &rusqlite::Transaction<'_>,
    key: JobKey<'_>,
) -> Result<Lease> {
    let lease = crate::lease::lookup_job(tx, key)?.ok_or_else(|| missing_lease_error(key))?;
    require_active(lease)
}

pub(super) fn live_lease(store: &LeaseStore, key: JobKey<'_>) -> Result<Lease> {
    let lease = store
        .find_job(key)?
        .ok_or_else(|| missing_lease_error(key))?;
    require_active(lease)
}

fn missing_lease_error(key: JobKey<'_>) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "no lease exists for {}/{}/{} to attach a coordination claim",
            key.owner, key.repo_name, key.job_id
        ),
    }
}

fn require_active(lease: Lease) -> Result<Lease> {
    if lease.allocation_state == AllocationState::Active {
        return Ok(lease);
    }
    Err(Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "lease {}/{}/{} is {}; coordination claims require an ACTIVE assignment",
            lease.owner,
            lease.repo_name,
            lease.job_id,
            lease.allocation_state.as_str()
        ),
    })
}

pub(super) fn require_agent_claim(
    store: &LeaseStore,
    key: JobKey<'_>,
    agent_id: &str,
) -> Result<CoordClaim> {
    let claim = store
        .find_claim(key)?
        .ok_or_else(|| Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message: format!(
                "no coordination claim for {}/{}/{}",
                key.owner, key.repo_name, key.job_id
            ),
        })?;
    if claim.agent_id != agent_id {
        return Err(held_error(&claim));
    }
    Ok(claim)
}

pub(super) fn upsert_agent(
    tx: &rusqlite::Transaction<'_>,
    identity: AgentIdentity<'_>,
    now: i64,
) -> Result<()> {
    tx.execute(
        "
        INSERT INTO agents (agent_id, agent_type, session_id, started_at, stopped_at)
        VALUES (?1, ?2, ?3, ?4, NULL)
        ON CONFLICT(agent_id) DO UPDATE SET
            agent_type = excluded.agent_type,
            session_id = excluded.session_id,
            started_at = excluded.started_at,
            stopped_at = NULL
        ",
        params![
            identity.agent_id,
            identity.agent_type,
            identity.session_id,
            now
        ],
    )
    .map_err(|e| coord_err("upsert agent", e))?;
    Ok(())
}

pub(super) fn insert_message(
    tx: &rusqlite::Transaction<'_>,
    message: NewMessage<'_>,
) -> Result<CoordMessage> {
    let paths_json = encode_paths(message.paths)?;
    tx.execute(
        "
        INSERT INTO coord_messages (
            created_at, kind, from_agent_id, from_owner, from_repo_name, from_job_id,
            to_agent_id, to_owner, to_repo_name, to_job_id, owner_generation, body, paths,
            ack_of, acked_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, NULL)
        ",
        params![
            message.now,
            message.kind.as_str(),
            message.from_agent_id,
            message.from_owner,
            message.from_repo_name,
            message.from_job_id,
            message.to_agent_id,
            message.to_owner,
            message.to_repo_name,
            message.to_job_id,
            message.owner_generation,
            message.body,
            paths_json,
            message.ack_of,
        ],
    )
    .map_err(|e| coord_err("insert coord message", e))?;
    let id = tx.last_insert_rowid();
    load_message_tx(tx, id)?.ok_or_else(|| coord_missing("message missing after insert"))
}

pub(super) fn load_claim_locked(conn: &Connection, key: JobKey<'_>) -> Result<Option<CoordClaim>> {
    let query = format!("{CLAIM_SELECT} WHERE c.owner = ?1 AND c.repo_name = ?2 AND c.job_id = ?3");
    conn.query_row(
        &query,
        params![key.owner, key.repo_name, key.job_id],
        claim_from_row,
    )
    .optional()
    .map_err(|e| coord_err("lookup coord claim", e))
}

pub(super) fn load_claim_tx(
    tx: &rusqlite::Transaction<'_>,
    key: JobKey<'_>,
) -> Result<Option<CoordClaim>> {
    load_claim_locked(tx, key)
}

pub(super) fn list_other_claims_tx(
    tx: &rusqlite::Transaction<'_>,
    key: JobKey<'_>,
) -> Result<Vec<CoordClaim>> {
    let query = format!(
        "{CLAIM_SELECT} WHERE l.released_at IS NULL AND l.tombstoned_at IS NULL \
         AND c.owner = ?1 AND c.repo_name = ?2 AND c.job_id != ?3"
    );
    let mut stmt = tx
        .prepare(&query)
        .map_err(|e| coord_err("list other coord claims", e))?;
    let rows = stmt
        .query_map(
            params![key.owner, key.repo_name, key.job_id],
            claim_from_row,
        )
        .map_err(|e| coord_err("list other coord claims", e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| coord_err("list other coord claims", e))
}

pub(super) fn load_message_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
) -> Result<Option<CoordMessage>> {
    let query = format!("{MESSAGE_SELECT} WHERE id = ?1");
    tx.query_row(&query, params![id], message_from_row)
        .optional()
        .map_err(|e| coord_err("lookup coord message", e))
}

pub(super) fn claim_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoordClaim> {
    Ok(CoordClaim {
        owner: row.get(0)?,
        repo_name: row.get(1)?,
        job_id: row.get(2)?,
        branch: row.get(3)?,
        worktree_path: row.get(4)?,
        agent_id: row.get(5)?,
        session_id: row.get(6)?,
        intent: row.get(7)?,
        declared_paths: decode_paths_row(row.get(8)?)?,
        owner_generation: row.get(9)?,
        paused_at: row.get(10)?,
        allocation_state: row.get(11)?,
    })
}

pub(super) fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoordMessage> {
    let kind: String = row.get(2)?;
    Ok(CoordMessage {
        id: row.get(0)?,
        created_at: row.get(1)?,
        kind: MessageKind::parse(&kind).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(error.to_string())),
            )
        })?,
        from_agent_id: row.get(3)?,
        from_owner: row.get(4)?,
        from_repo_name: row.get(5)?,
        from_job_id: row.get(6)?,
        to_agent_id: row.get(7)?,
        to_owner: row.get(8)?,
        to_repo_name: row.get(9)?,
        to_job_id: row.get(10)?,
        owner_generation: row.get(11)?,
        body: row.get(12)?,
        paths: decode_paths_row(row.get(13)?)?,
        ack_of: row.get(14)?,
        acked_at: row.get(15)?,
    })
}
