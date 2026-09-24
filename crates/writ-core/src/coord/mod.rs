//! Same-host coordination claims and messages on the existing lease store.
//!
//! This is an additive mailbox on `{state root}/leases.db`, not a second
//! database. Path overlap across distinct branches is advisory. Pause emits
//! help and never releases, tombstones, or deletes WIP. Ownership moves only
//! when a handoff is ACKed at the current `owner_generation`.

use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::{Error, PolicyCode, Result};
use crate::lease::{JobKey, LeaseStore};

mod access;
mod announce;
mod declared_paths;
mod types;
mod util;

#[cfg(test)]
mod tests;

pub use types::{
    AckRequest, AnnounceRequest, AnnounceResult, CoordClaim, CoordMessage, HandoffRequest,
    MessageKind, PathOverlap, PauseRequest, SendRequest,
};

use access::{
    CLAIM_SELECT, MESSAGE_SELECT, NewMessage, claim_from_row, insert_message, live_lease,
    load_claim_locked, load_claim_tx, load_message_tx, message_from_row, require_ack_recipient,
    require_active_lease_tx, require_agent_claim, require_complete_recipient,
    transfer_on_handoff_ack,
};
use announce::{AnnounceTx, MailboxDraft, announce_tx, mailbox_from_send, route_generic_ack};
use declared_paths::normalize_paths;
use util::{coord_err, coord_missing, held_error, now_secs, stale_error, terminal_allocation};

/// Create the additive coordination tables if they are missing.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS coord_claims (
            owner TEXT NOT NULL,
            repo_name TEXT NOT NULL,
            job_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            worktree_path TEXT NOT NULL,
            agent_id TEXT NOT NULL,
            session_id TEXT,
            intent TEXT,
            declared_paths TEXT NOT NULL DEFAULT '[]',
            owner_generation INTEGER NOT NULL DEFAULT 1,
            paused_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (owner, repo_name, job_id)
        );
        CREATE TABLE IF NOT EXISTS coord_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL,
            kind TEXT NOT NULL,
            from_agent_id TEXT NOT NULL,
            from_owner TEXT NOT NULL,
            from_repo_name TEXT NOT NULL,
            from_job_id TEXT NOT NULL,
            to_agent_id TEXT,
            to_owner TEXT,
            to_repo_name TEXT,
            to_job_id TEXT,
            owner_generation INTEGER NOT NULL,
            body TEXT NOT NULL,
            paths TEXT NOT NULL DEFAULT '[]',
            ack_of INTEGER,
            acked_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS coord_messages_to_job
            ON coord_messages (to_owner, to_repo_name, to_job_id, id);
        ",
    )
    .map_err(|e| coord_err("initialize coord schema", e))
}

impl LeaseStore {
    /// Announce task/agent/branch identity and declared paths.
    ///
    /// Overlaps are returned as advisory records and inbox messages. A live or
    /// paused claim owned by a different agent is not overwritten.
    pub fn announce(&self, request: AnnounceRequest<'_>) -> Result<AnnounceResult> {
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        let paths = normalize_paths(request.paths);
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord announce", e))?;
        let lease = require_active_lease_tx(&tx, key)?;
        let (intent, overlaps) = announce_tx(
            &tx,
            AnnounceTx {
                key,
                request: &request,
                lease: &lease,
                paths: &paths,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| coord_err("commit coord announce", e))?;
        drop(conn);
        let claim = self
            .find_claim(key)?
            .ok_or_else(|| coord_missing("claim missing after announce"))?;
        Ok(AnnounceResult {
            claim,
            intent,
            overlaps,
        })
    }

    /// Read one claim without mutating it.
    pub fn find_claim(&self, key: JobKey<'_>) -> Result<Option<CoordClaim>> {
        let conn = self.lock()?;
        load_claim_locked(&conn, key)
    }

    /// List claims joined to non-deleted lease rows.
    pub fn list_claims(&self) -> Result<Vec<CoordClaim>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&format!(
                "{CLAIM_SELECT} WHERE l.released_at IS NULL AND l.tombstoned_at IS NULL \
                 ORDER BY c.owner, c.repo_name, c.job_id"
            ))
            .map_err(|e| coord_err("list coord claims", e))?;
        let rows = stmt
            .query_map([], claim_from_row)
            .map_err(|e| coord_err("list coord claims", e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| coord_err("list coord claims", e))
    }

    /// Inbox for a job: messages it sent, received, or that were broadcast.
    pub fn inbox(&self, key: JobKey<'_>) -> Result<Vec<CoordMessage>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&format!(
                "{MESSAGE_SELECT} WHERE \
                 (from_owner = ?1 AND from_repo_name = ?2 AND from_job_id = ?3) \
                 OR (to_owner = ?1 AND to_repo_name = ?2 AND to_job_id = ?3) \
                 OR (to_job_id IS NULL AND from_owner = ?1 AND from_repo_name = ?2 \
                     AND kind IN ('intent', 'help', 'overlap', 'dependency', 'blocker')) \
                 ORDER BY id"
            ))
            .map_err(|e| coord_err("list coord inbox", e))?;
        let rows = stmt
            .query_map(
                params![key.owner, key.repo_name, key.job_id],
                message_from_row,
            )
            .map_err(|e| coord_err("list coord inbox", e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| coord_err("list coord inbox", e))
    }

    /// Append a mailbox message. `handoff` must use [`Self::propose_handoff`].
    pub fn send_message(&self, request: SendRequest<'_>) -> Result<CoordMessage> {
        if let Some(ack) = route_generic_ack(self, request)? {
            return Ok(ack);
        }
        require_complete_recipient(request)?;
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        let claim = require_agent_claim(self, key, request.agent_id)?;
        let paths = normalize_paths(request.paths);
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord send", e))?;
        let message = insert_message(
            &tx,
            mailbox_from_send(MailboxDraft {
                request: &request,
                owner_generation: claim.owner_generation,
                paths: &paths,
                now,
            }),
        )?;
        tx.commit().map_err(|e| coord_err("commit coord send", e))?;
        Ok(message)
    }

    /// Mark the current owner paused and emit help. Never deletes WIP.
    pub fn pause_claim(&self, request: PauseRequest<'_>) -> Result<(CoordClaim, CoordMessage)> {
        let key = request.key;
        let agent_id = request.agent_id;
        let lease = live_lease(self, key)?;
        let claim = require_agent_claim(self, key, agent_id)?;
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord pause", e))?;
        tx.execute(
            "
            UPDATE coord_claims
            SET paused_at = ?1, updated_at = ?1
            WHERE owner = ?2 AND repo_name = ?3 AND job_id = ?4 AND agent_id = ?5
            ",
            params![now, key.owner, key.repo_name, key.job_id, agent_id],
        )
        .map_err(|e| coord_err("pause coord claim", e))?;
        if tx.changes() != 1 {
            return Err(coord_missing("pause did not update the expected claim"));
        }
        let help = insert_message(
            &tx,
            NewMessage {
                kind: MessageKind::Help,
                from_agent_id: agent_id,
                from_owner: key.owner,
                from_repo_name: key.repo_name,
                from_job_id: key.job_id,
                to_agent_id: None,
                to_owner: None,
                to_repo_name: None,
                to_job_id: None,
                owner_generation: claim.owner_generation,
                body: request
                    .body
                    .unwrap_or("owner paused; help needed without seizing WIP"),
                paths: &[],
                ack_of: None,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| coord_err("commit coord pause", e))?;
        drop(conn);
        let paused = self
            .find_claim(key)?
            .ok_or_else(|| coord_missing("claim missing after pause"))?;
        if paused.worktree_path != lease.worktree_path {
            return Err(Error::LeaseStore {
                context: "pause coord claim",
                message: "pause must not move the leased worktree path".to_owned(),
            });
        }
        Ok((paused, help))
    }

    /// Offer a generation-bound handoff. The claim does not move until ACK.
    pub fn propose_handoff(&self, request: HandoffRequest<'_>) -> Result<CoordMessage> {
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        let to_job = request.to_job_id.unwrap_or(request.job_id);
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord handoff", e))?;
        let claim = load_claim_tx(&tx, key)?
            .ok_or_else(|| coord_missing("no coordination claim for handoff"))?;
        if claim.agent_id != request.from_agent_id {
            return Err(held_error(&claim));
        }
        if terminal_allocation(&claim.allocation_state) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::CoordClaimMissing,
                message: format!(
                    "lease {}/{}/{} is {}; handoff stays on live assignments",
                    claim.owner, claim.repo_name, claim.job_id, claim.allocation_state
                ),
            });
        }
        if let Some(expected) = request.expected_generation
            && expected != claim.owner_generation
        {
            return Err(stale_error(&claim, expected));
        }
        let message = insert_message(
            &tx,
            NewMessage {
                kind: MessageKind::Handoff,
                from_agent_id: request.from_agent_id,
                from_owner: request.owner,
                from_repo_name: request.repo_name,
                from_job_id: request.job_id,
                to_agent_id: Some(request.to_agent_id),
                to_owner: Some(request.owner),
                to_repo_name: Some(request.repo_name),
                to_job_id: Some(to_job),
                owner_generation: claim.owner_generation,
                body: request.body,
                paths: &[],
                ack_of: None,
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| coord_err("commit coord handoff", e))?;
        Ok(message)
    }

    /// Acknowledge a message. A matching handoff ACK transfers the claim.
    pub fn ack_message(
        &self,
        request: AckRequest<'_>,
    ) -> Result<(CoordMessage, Option<CoordClaim>)> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord ack", e))?;
        let message =
            load_message_tx(&tx, request.message_id)?.ok_or_else(|| Error::LeaseStore {
                context: "ack coord message",
                message: format!("unknown message {}", request.message_id),
            })?;
        if message.acked_at.is_some() {
            return Err(Error::LeaseStore {
                context: "ack coord message",
                message: format!("message {} is already acknowledged", request.message_id),
            });
        }
        if message.kind != MessageKind::Handoff {
            require_ack_recipient(&tx, &message, request)?;
        }
        let transferred = if message.kind == MessageKind::Handoff {
            Some(transfer_on_handoff_ack(&tx, &message, request, now)?)
        } else {
            None
        };
        tx.execute(
            "UPDATE coord_messages SET acked_at = ?1 WHERE id = ?2 AND acked_at IS NULL",
            params![now, request.message_id],
        )
        .map_err(|e| coord_err("mark coord message acked", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "ack coord message",
                message: "message was acknowledged concurrently".to_owned(),
            });
        }
        let ack = insert_message(
            &tx,
            NewMessage {
                kind: MessageKind::Ack,
                from_agent_id: request.agent_id,
                from_owner: request.owner,
                from_repo_name: request.repo_name,
                from_job_id: request.job_id,
                to_agent_id: Some(message.from_agent_id.as_str()),
                to_owner: Some(message.from_owner.as_str()),
                to_repo_name: Some(message.from_repo_name.as_str()),
                to_job_id: Some(message.from_job_id.as_str()),
                owner_generation: transferred
                    .as_ref()
                    .map_or(message.owner_generation, |claim| claim.owner_generation),
                body: "acknowledged",
                paths: &[],
                ack_of: Some(request.message_id),
                now,
            },
        )?;
        tx.commit().map_err(|e| coord_err("commit coord ack", e))?;
        Ok((ack, transferred))
    }
}
