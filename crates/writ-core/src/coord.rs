//! Same-host coordination claims and messages on the existing lease store.
//!
//! This is an additive mailbox on `{state root}/leases.db`, not a second
//! database. Path overlap across distinct branches is advisory. Pause emits
//! help and never releases, tombstones, or deletes WIP. Ownership moves only
//! when a handoff is ACKed at the current `owner_generation`.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::error::{Error, PolicyCode, Result};
use crate::lease::{JobKey, Lease, LeaseStore};

/// Shared coordination event kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Intent,
    Overlap,
    Help,
    Handoff,
    Ack,
    Dependency,
}

impl MessageKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Overlap => "overlap",
            Self::Help => "help",
            Self::Handoff => "handoff",
            Self::Ack => "ack",
            Self::Dependency => "dependency",
        }
    }

    /// Parse a kind string. Unknown values fail closed.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "intent" => Ok(Self::Intent),
            "overlap" => Ok(Self::Overlap),
            "help" => Ok(Self::Help),
            "handoff" => Ok(Self::Handoff),
            "ack" => Ok(Self::Ack),
            "dependency" => Ok(Self::Dependency),
            other => Err(Error::LeaseStore {
                context: "parse coord message kind",
                message: format!("unknown message kind `{other}`"),
            }),
        }
    }
}

/// Declared owner of a leased job assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoordClaim {
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub branch: String,
    pub worktree_path: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub intent: Option<String>,
    pub declared_paths: Vec<String>,
    pub owner_generation: i64,
    pub paused_at: Option<i64>,
    pub allocation_state: String,
}

/// One inbox row. `ack_of` points at the message being acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoordMessage {
    pub id: i64,
    pub created_at: i64,
    pub kind: MessageKind,
    pub from_agent_id: String,
    pub from_owner: String,
    pub from_repo_name: String,
    pub from_job_id: String,
    pub to_agent_id: Option<String>,
    pub to_owner: Option<String>,
    pub to_repo_name: Option<String>,
    pub to_job_id: Option<String>,
    pub owner_generation: i64,
    pub body: String,
    pub paths: Vec<String>,
    pub ack_of: Option<i64>,
    pub acked_at: Option<i64>,
}

/// Advisory overlap against another live or paused claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathOverlap {
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub agent_id: String,
    pub branch: String,
    pub paths: Vec<String>,
    /// Always true: overlap does not veto writes on distinct branches.
    pub advisory: bool,
}

/// Identity + declared paths for `announce`.
#[derive(Debug, Clone, Copy)]
pub struct AnnounceRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub session_id: Option<&'a str>,
    pub agent_type: &'a str,
    pub intent: Option<&'a str>,
    pub paths: &'a [String],
}

/// Result of announcing identity on a live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnnounceResult {
    pub claim: CoordClaim,
    pub intent: CoordMessage,
    pub overlaps: Vec<PathOverlap>,
}

/// Inputs for a generation-stamped handoff offer.
#[derive(Debug, Clone, Copy)]
pub struct HandoffRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub from_agent_id: &'a str,
    pub to_agent_id: &'a str,
    pub to_job_id: Option<&'a str>,
    pub expected_generation: Option<i64>,
    pub body: &'a str,
}

/// Inputs for acknowledging a message, including a handoff transfer.
#[derive(Debug, Clone, Copy)]
pub struct AckRequest<'a> {
    pub message_id: i64,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub session_id: Option<&'a str>,
}

/// Generic mailbox send (help/overlap/intent/dependency; not a seize).
#[derive(Debug, Clone, Copy)]
pub struct SendRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub kind: MessageKind,
    pub body: &'a str,
    pub to_agent_id: Option<&'a str>,
    pub to_owner: Option<&'a str>,
    pub to_repo_name: Option<&'a str>,
    pub to_job_id: Option<&'a str>,
    pub paths: &'a [String],
    pub ack_of: Option<i64>,
}

const CLAIM_SELECT: &str = "SELECT c.owner, c.repo_name, c.job_id, c.branch, c.worktree_path, \
     c.agent_id, c.session_id, c.intent, c.declared_paths, c.owner_generation, \
     c.paused_at, l.allocation_state \
     FROM coord_claims c \
     JOIN leases l ON l.owner = c.owner AND l.repo_name = c.repo_name AND l.job_id = c.job_id";

const MESSAGE_SELECT: &str = "SELECT id, created_at, kind, from_agent_id, from_owner, \
     from_repo_name, from_job_id, to_agent_id, to_owner, to_repo_name, to_job_id, \
     owner_generation, body, paths, ack_of, acked_at FROM coord_messages";

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
        let lease = live_lease(self, key)?;
        let paths = normalize_paths(request.paths);
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| coord_err("begin coord announce", e))?;
        upsert_agent(
            &tx,
            request.agent_id,
            request.agent_type,
            request.session_id,
            now,
        )?;
        let existing = load_claim_tx(&tx, key)?;
        if let Some(existing) = existing.as_ref()
            && existing.agent_id != request.agent_id
        {
            return Err(held_error(existing));
        }
        let generation = existing.as_ref().map_or(1, |claim| claim.owner_generation);
        let paused_at = existing.as_ref().and_then(|claim| claim.paused_at);
        let paths_json = encode_paths(&paths)?;
        tx.execute(
            "
            INSERT INTO coord_claims (
                owner, repo_name, job_id, branch, worktree_path, agent_id, session_id,
                intent, declared_paths, owner_generation, paused_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
            ON CONFLICT(owner, repo_name, job_id) DO UPDATE SET
                branch = excluded.branch,
                worktree_path = excluded.worktree_path,
                session_id = excluded.session_id,
                intent = excluded.intent,
                declared_paths = excluded.declared_paths,
                updated_at = excluded.updated_at
            ",
            params![
                request.owner,
                request.repo_name,
                request.job_id,
                lease.branch,
                lease.worktree_path,
                request.agent_id,
                request.session_id,
                request.intent,
                paths_json,
                generation,
                paused_at,
                now,
            ],
        )
        .map_err(|e| coord_err("upsert coord claim", e))?;
        let intent_body = request
            .intent
            .unwrap_or("announced identity and declared paths");
        let intent = insert_message(
            &tx,
            NewMessage {
                kind: MessageKind::Intent,
                from_agent_id: request.agent_id,
                from_owner: request.owner,
                from_repo_name: request.repo_name,
                from_job_id: request.job_id,
                to_agent_id: None,
                to_owner: None,
                to_repo_name: None,
                to_job_id: None,
                owner_generation: generation,
                body: intent_body,
                paths: &paths,
                ack_of: None,
                now,
            },
        )?;
        let others = list_other_claims_tx(&tx, key)?;
        let mut overlaps = Vec::new();
        for other in others {
            let shared = overlapping_paths(&paths, &other.declared_paths);
            if shared.is_empty() {
                continue;
            }
            let overlap = PathOverlap {
                owner: other.owner.clone(),
                repo_name: other.repo_name.clone(),
                job_id: other.job_id.clone(),
                agent_id: other.agent_id.clone(),
                branch: other.branch.clone(),
                paths: shared.clone(),
                advisory: true,
            };
            insert_message(
                &tx,
                NewMessage {
                    kind: MessageKind::Overlap,
                    from_agent_id: request.agent_id,
                    from_owner: request.owner,
                    from_repo_name: request.repo_name,
                    from_job_id: request.job_id,
                    to_agent_id: Some(other.agent_id.as_str()),
                    to_owner: Some(other.owner.as_str()),
                    to_repo_name: Some(other.repo_name.as_str()),
                    to_job_id: Some(other.job_id.as_str()),
                    owner_generation: generation,
                    body: "advisory declared-path overlap",
                    paths: &shared,
                    ack_of: None,
                    now,
                },
            )?;
            overlaps.push(overlap);
        }
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
                 OR (to_job_id IS NULL AND kind IN ('intent', 'help', 'overlap', 'dependency')) \
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
        if request.kind == MessageKind::Handoff {
            return Err(Error::LeaseStore {
                context: "send coord message",
                message: "handoff requires propose_handoff so owner_generation is bound".to_owned(),
            });
        }
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
            NewMessage {
                kind: request.kind,
                from_agent_id: request.agent_id,
                from_owner: request.owner,
                from_repo_name: request.repo_name,
                from_job_id: request.job_id,
                to_agent_id: request.to_agent_id,
                to_owner: request.to_owner,
                to_repo_name: request.to_repo_name,
                to_job_id: request.to_job_id,
                owner_generation: claim.owner_generation,
                body: request.body,
                paths: &paths,
                ack_of: request.ack_of,
                now,
            },
        )?;
        tx.commit().map_err(|e| coord_err("commit coord send", e))?;
        Ok(message)
    }

    /// Mark the current owner paused and emit help. Never deletes WIP.
    pub fn pause_claim(
        &self,
        key: JobKey<'_>,
        agent_id: &str,
        body: Option<&str>,
    ) -> Result<(CoordClaim, CoordMessage)> {
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
                body: body.unwrap_or("owner paused; help needed without seizing WIP"),
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

struct NewMessage<'a> {
    kind: MessageKind,
    from_agent_id: &'a str,
    from_owner: &'a str,
    from_repo_name: &'a str,
    from_job_id: &'a str,
    to_agent_id: Option<&'a str>,
    to_owner: Option<&'a str>,
    to_repo_name: Option<&'a str>,
    to_job_id: Option<&'a str>,
    owner_generation: i64,
    body: &'a str,
    paths: &'a [String],
    ack_of: Option<i64>,
    now: i64,
}

fn live_lease(store: &LeaseStore, key: JobKey<'_>) -> Result<Lease> {
    let lease = store.find_job(key)?.ok_or_else(|| Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "no lease exists for {}/{}/{} to attach a coordination claim",
            key.owner, key.repo_name, key.job_id
        ),
    })?;
    if lease.allocation_state.is_terminal() {
        return Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message: format!(
                "lease {}/{}/{} is {}; coordination claims stay on live assignments",
                lease.owner,
                lease.repo_name,
                lease.job_id,
                lease.allocation_state.as_str()
            ),
        });
    }
    Ok(lease)
}

fn require_agent_claim(store: &LeaseStore, key: JobKey<'_>, agent_id: &str) -> Result<CoordClaim> {
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

fn transfer_on_handoff_ack(
    tx: &rusqlite::Transaction<'_>,
    handoff: &CoordMessage,
    request: AckRequest<'_>,
    now: i64,
) -> Result<CoordClaim> {
    let expected_agent = handoff
        .to_agent_id
        .as_deref()
        .ok_or_else(|| Error::LeaseStore {
            context: "ack coord handoff",
            message: "handoff is missing a target agent".to_owned(),
        })?;
    if expected_agent != request.agent_id {
        return Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimHeld,
            message: format!(
                "handoff {} is addressed to `{expected_agent}`, not `{}`",
                handoff.id, request.agent_id
            ),
        });
    }
    let from_key = JobKey {
        owner: &handoff.from_owner,
        repo_name: &handoff.from_repo_name,
        job_id: &handoff.from_job_id,
    };
    let current = load_claim_tx(tx, from_key)?
        .ok_or_else(|| coord_missing("handoff source claim is missing"))?;
    if terminal_allocation(&current.allocation_state) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message: format!(
                "lease {}/{}/{} is {}; refusing to transfer a dead assignment",
                current.owner, current.repo_name, current.job_id, current.allocation_state
            ),
        });
    }
    if current.owner_generation != handoff.owner_generation {
        return Err(stale_error(&current, handoff.owner_generation));
    }
    let next_generation = current.owner_generation + 1;
    tx.execute(
        "
        UPDATE coord_claims
        SET agent_id = ?1, session_id = ?2, owner_generation = ?3, paused_at = NULL, updated_at = ?4
        WHERE owner = ?5 AND repo_name = ?6 AND job_id = ?7 AND owner_generation = ?8
        ",
        params![
            request.agent_id,
            request.session_id,
            next_generation,
            now,
            current.owner,
            current.repo_name,
            current.job_id,
            current.owner_generation,
        ],
    )
    .map_err(|e| coord_err("transfer coord claim", e))?;
    if tx.changes() != 1 {
        return Err(stale_error(&current, handoff.owner_generation));
    }
    load_claim_tx(tx, from_key)?.ok_or_else(|| coord_missing("claim missing after handoff ACK"))
}

fn upsert_agent(
    tx: &rusqlite::Transaction<'_>,
    agent_id: &str,
    agent_type: &str,
    session_id: Option<&str>,
    now: i64,
) -> Result<()> {
    tx.execute(
        "
        INSERT INTO agents (agent_id, agent_type, session_id, started_at, stopped_at)
        VALUES (?1, ?2, ?3, ?4, NULL)
        ON CONFLICT(agent_id) DO UPDATE SET
            agent_type = excluded.agent_type,
            session_id = excluded.session_id,
            stopped_at = NULL
        ",
        params![agent_id, agent_type, session_id, now],
    )
    .map_err(|e| coord_err("upsert agent", e))?;
    Ok(())
}

fn insert_message(tx: &rusqlite::Transaction<'_>, message: NewMessage<'_>) -> Result<CoordMessage> {
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

fn load_claim_locked(conn: &Connection, key: JobKey<'_>) -> Result<Option<CoordClaim>> {
    let query = format!("{CLAIM_SELECT} WHERE c.owner = ?1 AND c.repo_name = ?2 AND c.job_id = ?3");
    conn.query_row(
        &query,
        params![key.owner, key.repo_name, key.job_id],
        claim_from_row,
    )
    .optional()
    .map_err(|e| coord_err("lookup coord claim", e))
}

fn load_claim_tx(tx: &rusqlite::Transaction<'_>, key: JobKey<'_>) -> Result<Option<CoordClaim>> {
    let query = format!("{CLAIM_SELECT} WHERE c.owner = ?1 AND c.repo_name = ?2 AND c.job_id = ?3");
    tx.query_row(
        &query,
        params![key.owner, key.repo_name, key.job_id],
        claim_from_row,
    )
    .optional()
    .map_err(|e| coord_err("lookup coord claim", e))
}

fn list_other_claims_tx(
    tx: &rusqlite::Transaction<'_>,
    key: JobKey<'_>,
) -> Result<Vec<CoordClaim>> {
    let query = format!(
        "{CLAIM_SELECT} WHERE l.released_at IS NULL AND l.tombstoned_at IS NULL \
         AND NOT (c.owner = ?1 AND c.repo_name = ?2 AND c.job_id = ?3)"
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

fn load_message_tx(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<Option<CoordMessage>> {
    let query = format!("{MESSAGE_SELECT} WHERE id = ?1");
    tx.query_row(&query, params![id], message_from_row)
        .optional()
        .map_err(|e| coord_err("lookup coord message", e))
}

fn claim_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoordClaim> {
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

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoordMessage> {
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

fn decode_paths_row(raw: String) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn encode_paths(paths: &[String]) -> Result<String> {
    serde_json::to_string(paths).map_err(|error| Error::LeaseStore {
        context: "encode declared paths",
        message: error.to_string(),
    })
}

fn normalize_paths(paths: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for path in paths {
        let cleaned = normalize_one(path);
        if cleaned.is_empty() || normalized.iter().any(|existing| existing == &cleaned) {
            continue;
        }
        normalized.push(cleaned);
    }
    normalized
}

fn normalize_one(path: &str) -> String {
    let trimmed = path.trim().replace('\\', "/");
    let mut stripped = trimmed.trim_start_matches("./").to_owned();
    while stripped.contains("//") {
        stripped = stripped.replace("//", "/");
    }
    stripped.trim_end_matches('/').to_owned()
}

fn overlapping_paths(left: &[String], right: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for a in left {
        for b in right {
            if paths_overlap(a, b) {
                let shared = if a.len() <= b.len() {
                    a.clone()
                } else {
                    b.clone()
                };
                if !hits.iter().any(|existing| existing == &shared) {
                    hits.push(shared);
                }
            }
        }
    }
    hits
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&(right.to_owned() + "/"))
        || right.starts_with(&(left.to_owned() + "/"))
}

fn terminal_allocation(state: &str) -> bool {
    matches!(state, "RELEASED" | "TOMBSTONED")
}

fn held_error(claim: &CoordClaim) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordClaimHeld,
        message: format!(
            "job {}/{}/{} is owned by `{}` (generation {}); pause is not permission to seize WIP",
            claim.owner, claim.repo_name, claim.job_id, claim.agent_id, claim.owner_generation
        ),
    }
}

fn stale_error(claim: &CoordClaim, expected: i64) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordStaleGeneration,
        message: format!(
            "handoff generation {expected} is stale; live generation is {}",
            claim.owner_generation
        ),
    }
}

fn coord_missing(message: &str) -> Error {
    Error::LeaseStore {
        context: "coord store",
        message: message.to_owned(),
    }
}

fn coord_err(context: &'static str, err: rusqlite::Error) -> Error {
    Error::LeaseStore {
        context,
        message: err.to_string(),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::{AllocateRequest, AllocationState};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    struct Harness {
        _temp: tempfile::TempDir,
        store: LeaseStore,
        path: std::path::PathBuf,
        repo: std::path::PathBuf,
        start: String,
    }

    impl Harness {
        fn new() -> Self {
            let temp = tempdir().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "user.name", "Test User"]);
            git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
            let start = git(&repo, &["rev-parse", "HEAD"]);
            let path = temp.path().join("leases.db");
            let store = LeaseStore::open(&path).unwrap();
            Self {
                _temp: temp,
                store,
                path,
                repo,
                start,
            }
        }

        fn seed_job(&self, job_id: &str, branch: &str) -> std::path::PathBuf {
            let worktree = self._temp.path().join("worktrees/acme/sample").join(job_id);
            fs::create_dir_all(&worktree).unwrap();
            let prepared = self
                .store
                .prepare_allocate(AllocateRequest {
                    repo: &self.repo,
                    owner: "acme",
                    repo_name: "sample",
                    job_id,
                    branch,
                    worktree_path: &worktree,
                    requested_start_point: "refs/heads/main",
                    start_commit: &self.start,
                    ttl: None,
                })
                .unwrap();
            self.store.mark_mutating(&prepared.operation_id).unwrap();
            self.store.commit_allocate(&prepared.operation_id).unwrap();
            worktree
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn announce<'a>(
        store: &'a LeaseStore,
        job_id: &'a str,
        agent: &'a str,
        paths: &'a [String],
    ) -> AnnounceResult {
        store
            .announce(AnnounceRequest {
                owner: "acme",
                repo_name: "sample",
                job_id,
                agent_id: agent,
                session_id: Some("session-a"),
                agent_type: "worker",
                intent: Some("edit shared contract"),
                paths,
            })
            .unwrap()
    }

    #[test]
    fn two_store_connections_exchange_advisory_overlap() {
        let harness = Harness::new();
        harness.seed_job("job-a", "hive/job-a");
        harness.seed_job("job-b", "hive/job-b");
        let peer = LeaseStore::open(&harness.path).unwrap();

        let first = announce(
            &harness.store,
            "job-a",
            "agent-a",
            &[String::from("crates/writ-core/src/coord.rs")],
        );
        assert!(first.overlaps.is_empty());
        let second = announce(
            &peer,
            "job-b",
            "agent-b",
            &[String::from("crates/writ-core/src")],
        );
        assert_eq!(second.overlaps.len(), 1);
        assert!(second.overlaps[0].advisory);
        assert_eq!(second.overlaps[0].job_id, "job-a");

        let inbox = peer
            .inbox(JobKey {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
            })
            .unwrap();
        assert!(
            inbox
                .iter()
                .any(|message| message.kind == MessageKind::Overlap
                    && message.from_job_id == "job-b")
        );
    }

    #[test]
    fn pause_does_not_release_or_delete_wip_and_requires_handoff_ack() {
        let harness = Harness::new();
        let worktree = harness.seed_job("job-a", "hive/job-a");
        let wip = worktree.join("wip.txt");
        fs::write(&wip, "keep me").unwrap();
        announce(
            &harness.store,
            "job-a",
            "agent-a",
            &[String::from("crates/writ-core/src/coord.rs")],
        );

        let peer = LeaseStore::open(&harness.path).unwrap();
        let key = JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
        };
        let (paused, help) = harness
            .store
            .pause_claim(key, "agent-a", Some("owner crashed"))
            .unwrap();
        assert!(paused.paused_at.is_some());
        assert_eq!(help.kind, MessageKind::Help);
        assert_eq!(
            harness
                .store
                .find_job(key)
                .unwrap()
                .unwrap()
                .allocation_state,
            AllocationState::Active
        );
        assert_eq!(fs::read_to_string(&wip).unwrap(), "keep me");

        let seize = peer.announce(AnnounceRequest {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-b",
            session_id: Some("session-b"),
            agent_type: "worker",
            intent: Some("take over"),
            paths: &[String::from("crates/writ-core/src/coord.rs")],
        });
        match seize {
            Err(Error::PolicyViolation {
                code: PolicyCode::CoordClaimHeld,
                ..
            }) => {}
            other => panic!("expected held claim, got {other:?}"),
        }

        let handoff = harness
            .store
            .propose_handoff(HandoffRequest {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                from_agent_id: "agent-a",
                to_agent_id: "agent-b",
                to_job_id: Some("job-a"),
                expected_generation: Some(1),
                body: "paused owner transferring assignment",
            })
            .unwrap();
        let stale = peer.ack_message(AckRequest {
            message_id: handoff.id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-b",
            session_id: Some("session-b"),
        });
        // First ACK should succeed; a second ACK of the same generation fails.
        let (ack, transferred) = stale.unwrap();
        assert_eq!(ack.kind, MessageKind::Ack);
        let transferred = transferred.expect("handoff ACK transfers the claim");
        assert_eq!(transferred.agent_id, "agent-b");
        assert_eq!(transferred.owner_generation, 2);
        assert!(transferred.paused_at.is_none());
        assert_eq!(fs::read_to_string(&wip).unwrap(), "keep me");
        assert_eq!(
            harness.store.find_job(key).unwrap().unwrap().worktree_path,
            worktree.to_string_lossy()
        );

        let replay = peer.ack_message(AckRequest {
            message_id: handoff.id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-b",
            session_id: Some("session-b"),
        });
        assert!(replay.is_err());
    }

    #[test]
    fn stale_generation_handoff_is_rejected() {
        let harness = Harness::new();
        harness.seed_job("job-a", "hive/job-a");
        announce(&harness.store, "job-a", "agent-a", &[]);
        let err = harness
            .store
            .propose_handoff(HandoffRequest {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                from_agent_id: "agent-a",
                to_agent_id: "agent-b",
                to_job_id: None,
                expected_generation: Some(99),
                body: "stale",
            })
            .unwrap_err();
        match err {
            Error::PolicyViolation {
                code: PolicyCode::CoordStaleGeneration,
                ..
            } => {}
            other => panic!("expected stale generation, got {other:?}"),
        }

        let peer = LeaseStore::open(&harness.path).unwrap();
        let first = harness
            .store
            .propose_handoff(HandoffRequest {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                from_agent_id: "agent-a",
                to_agent_id: "agent-b",
                to_job_id: None,
                expected_generation: Some(1),
                body: "first gen-1 offer",
            })
            .unwrap();
        let leftover = harness
            .store
            .propose_handoff(HandoffRequest {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                from_agent_id: "agent-a",
                to_agent_id: "agent-b",
                to_job_id: None,
                expected_generation: Some(1),
                body: "leftover gen-1 offer",
            })
            .unwrap();
        peer.ack_message(AckRequest {
            message_id: first.id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-b",
            session_id: Some("session-b"),
        })
        .unwrap();
        let stale_ack = peer.ack_message(AckRequest {
            message_id: leftover.id,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            agent_id: "agent-b",
            session_id: Some("session-b"),
        });
        match stale_ack {
            Err(Error::PolicyViolation {
                code: PolicyCode::CoordStaleGeneration,
                ..
            }) => {}
            other => panic!("expected stale generation on leftover handoff ACK, got {other:?}"),
        }
    }
}
