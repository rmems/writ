//! Announce persist, overlap scan, and send-mailbox routing.

use rusqlite::params;

use crate::error::{Error, PolicyCode, Result};
use crate::lease::{AgentIdentity, JobKey, LeaseStore};

use super::access::{
    NewMessage, insert_message, list_other_claims_tx, load_claim_tx, require_active_lease_tx,
    upsert_agent,
};
use super::declared_paths::encode_paths;
use super::types::{
    AckRequest, AnnounceRequest, CoordClaim, CoordMessage, MessageKind, PathOverlap, SendRequest,
};
use super::util::{coord_err, held_error};

pub(super) struct AnnounceTx<'a> {
    pub(super) key: JobKey<'a>,
    pub(super) request: &'a AnnounceRequest<'a>,
    pub(super) lease: &'a crate::lease::Lease,
    pub(super) paths: &'a [String],
    pub(super) now: i64,
}

pub(super) fn announce_tx(
    tx: &rusqlite::Transaction<'_>,
    row: AnnounceTx<'_>,
) -> Result<(CoordMessage, Vec<PathOverlap>)> {
    let lease = require_active_lease_tx(tx, row.key)?;
    if lease.operation_id != row.lease.operation_id {
        return Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimMissing,
            message: format!(
                "lease {}/{}/{} changed during announce (operation {} != {})",
                row.key.owner,
                row.key.repo_name,
                row.key.job_id,
                lease.operation_id,
                row.lease.operation_id
            ),
        });
    }
    upsert_agent(
        tx,
        AgentIdentity {
            agent_id: row.request.agent_id,
            agent_type: row.request.agent_type,
            session_id: row.request.session_id,
        },
        row.now,
    )?;
    let existing = load_claim_tx(tx, row.key)?;
    if let Some(existing) = existing.as_ref()
        && existing.agent_id != row.request.agent_id
    {
        return Err(held_error(existing));
    }
    persist_announce_tx(
        tx,
        AnnouncePersist {
            request: row.request,
            lease: &lease,
            generation: next_owner_generation(existing.as_ref(), row.request.session_id),
            paused_at: existing.as_ref().and_then(|claim| claim.paused_at),
            paths: row.paths,
            now: row.now,
        },
    )
}

pub(super) struct AnnouncePersist<'a> {
    request: &'a AnnounceRequest<'a>,
    lease: &'a crate::lease::Lease,
    generation: i64,
    paused_at: Option<i64>,
    paths: &'a [String],
    now: i64,
}

pub(super) fn persist_announce_claim(
    tx: &rusqlite::Transaction<'_>,
    row: &AnnouncePersist<'_>,
) -> Result<()> {
    let paths_json = encode_paths(row.paths)?;
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
            owner_generation = excluded.owner_generation,
            updated_at = excluded.updated_at
        ",
        params![
            row.request.owner,
            row.request.repo_name,
            row.request.job_id,
            row.lease.branch,
            row.lease.worktree_path,
            row.request.agent_id,
            row.request.session_id,
            row.request.intent,
            paths_json,
            row.generation,
            row.paused_at,
            row.now,
        ],
    )
    .map_err(|e| coord_err("upsert coord claim", e))?;
    Ok(())
}

pub(super) fn persist_announce_tx(
    tx: &rusqlite::Transaction<'_>,
    row: AnnouncePersist<'_>,
) -> Result<(CoordMessage, Vec<PathOverlap>)> {
    persist_announce_claim(tx, &row)?;
    let intent = insert_announce_intent(
        tx,
        IntentInsert {
            request: row.request,
            generation: row.generation,
            paths: row.paths,
            now: row.now,
        },
    )?;
    let others = list_other_claims_tx(
        tx,
        JobKey {
            owner: row.request.owner,
            repo_name: row.request.repo_name,
            job_id: row.request.job_id,
        },
    )?;
    let overlaps = record_advisory_overlaps(
        tx,
        OverlapScan {
            request: row.request,
            generation: row.generation,
            paths: row.paths,
            others: &others,
            now: row.now,
        },
    )?;
    Ok((intent, overlaps))
}

pub(super) struct IntentInsert<'a> {
    request: &'a AnnounceRequest<'a>,
    generation: i64,
    paths: &'a [String],
    now: i64,
}

pub(super) fn insert_announce_intent(
    tx: &rusqlite::Transaction<'_>,
    intent: IntentInsert<'_>,
) -> Result<CoordMessage> {
    let request = intent.request;
    insert_message(
        tx,
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
            owner_generation: intent.generation,
            body: request
                .intent
                .unwrap_or("announced identity and declared paths"),
            paths: intent.paths,
            ack_of: None,
            now: intent.now,
        },
    )
}

pub(super) fn overlapping_paths(left: &[String], right: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for path in left {
        collect_path_overlaps(path, right, &mut hits);
    }
    hits
}

pub(super) fn collect_path_overlaps(path: &str, others: &[String], hits: &mut Vec<String>) {
    for other in others {
        if let Some(shared) = shorter_overlap(path, other) {
            push_unique_overlap(hits, shared);
        }
    }
}

pub(super) fn shorter_overlap(left: &str, right: &str) -> Option<String> {
    if !paths_overlap(left, right) {
        return None;
    }
    if left.len() <= right.len() {
        Some(left.to_owned())
    } else {
        Some(right.to_owned())
    }
}

pub(super) fn push_unique_overlap(hits: &mut Vec<String>, shared: String) {
    if !hits.iter().any(|existing| existing == &shared) {
        hits.push(shared);
    }
}

pub(super) struct OverlapScan<'a> {
    request: &'a AnnounceRequest<'a>,
    generation: i64,
    paths: &'a [String],
    others: &'a [CoordClaim],
    now: i64,
}

pub(super) fn record_advisory_overlaps(
    tx: &rusqlite::Transaction<'_>,
    scan: OverlapScan<'_>,
) -> Result<Vec<PathOverlap>> {
    let mut overlaps = Vec::new();
    for other in scan.others {
        if let Some(overlap) = advisory_overlap_with(tx, &scan, other)? {
            overlaps.push(overlap);
        }
    }
    Ok(overlaps)
}

pub(super) fn advisory_overlap_with(
    tx: &rusqlite::Transaction<'_>,
    scan: &OverlapScan<'_>,
    other: &CoordClaim,
) -> Result<Option<PathOverlap>> {
    let shared = overlapping_paths(scan.paths, &other.declared_paths);
    if shared.is_empty() {
        return Ok(None);
    }
    let request = scan.request;
    insert_message(
        tx,
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
            owner_generation: scan.generation,
            body: "advisory declared-path overlap",
            paths: &shared,
            ack_of: None,
            now: scan.now,
        },
    )?;
    Ok(Some(PathOverlap {
        owner: other.owner.clone(),
        repo_name: other.repo_name.clone(),
        job_id: other.job_id.clone(),
        agent_id: other.agent_id.clone(),
        branch: other.branch.clone(),
        paths: shared,
        advisory: true,
    }))
}

pub(super) fn paths_overlap(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    if left == "." || right == "." {
        return true;
    }
    left == right
        || left.starts_with(&(right.to_owned() + "/"))
        || right.starts_with(&(left.to_owned() + "/"))
}

pub(super) fn next_owner_generation(
    existing: Option<&CoordClaim>,
    session_id: Option<&str>,
) -> i64 {
    match existing {
        None => 1,
        Some(claim) if claim.session_id.as_deref() != session_id => claim.owner_generation + 1,
        Some(claim) => claim.owner_generation,
    }
}

pub(super) fn route_generic_ack(
    store: &LeaseStore,
    request: SendRequest<'_>,
) -> Result<Option<CoordMessage>> {
    if request.kind == MessageKind::Handoff {
        return Err(Error::LeaseStore {
            context: "send coord message",
            message: "handoff requires propose_handoff so owner_generation is bound".to_owned(),
        });
    }
    if request.kind != MessageKind::Ack {
        return Ok(None);
    }
    let Some(message_id) = request.ack_of else {
        return Err(Error::LeaseStore {
            context: "send coord message",
            message: "ack requires --ack-of so coord ack can update the original message"
                .to_owned(),
        });
    };
    let (ack, _) = store.ack_message(AckRequest {
        message_id,
        owner: request.owner,
        repo_name: request.repo_name,
        job_id: request.job_id,
        agent_id: request.agent_id,
        session_id: None,
    })?;
    Ok(Some(ack))
}

pub(super) struct MailboxDraft<'a> {
    pub(super) request: &'a SendRequest<'a>,
    pub(super) owner_generation: i64,
    pub(super) paths: &'a [String],
    pub(super) now: i64,
}

pub(super) fn mailbox_from_send(draft: MailboxDraft<'_>) -> NewMessage<'_> {
    let request = draft.request;
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
        owner_generation: draft.owner_generation,
        body: request.body,
        paths: draft.paths,
        ack_of: request.ack_of,
        now: draft.now,
    }
}
