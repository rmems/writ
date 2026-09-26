//! ACK recipient checks and generation-bound handoff transfer.

use rusqlite::params;

use crate::error::{Error, PolicyCode, Result};
use crate::lease::JobKey;

use super::access::load_claim_tx;
use super::types::{AckRequest, CoordClaim, CoordMessage, SendRequest};
use super::util::{coord_err, coord_missing, held_error, stale_error, terminal_allocation};

pub(super) fn require_complete_recipient(request: SendRequest<'_>) -> Result<()> {
    let parts = [
        request.to_owner,
        request.to_repo_name,
        request.to_job_id,
        request.to_agent_id,
    ];
    if parts.iter().all(Option::is_none) {
        return Ok(());
    }
    if recipient_job_is_complete(request) {
        return Ok(());
    }
    Err(Error::LeaseStore {
        context: "send coord message",
        message: "message recipient must be a complete owner/repository/job tuple or a broadcast"
            .to_owned(),
    })
}

fn recipient_job_is_complete(request: SendRequest<'_>) -> bool {
    request.to_owner.is_some() && request.to_repo_name.is_some() && request.to_job_id.is_some()
}

pub(super) fn require_ack_recipient(
    tx: &rusqlite::Transaction<'_>,
    message: &CoordMessage,
    request: AckRequest<'_>,
) -> Result<()> {
    let key = JobKey {
        owner: request.owner,
        repo_name: request.repo_name,
        job_id: request.job_id,
    };
    let claim = load_claim_tx(tx, key)?.ok_or_else(|| Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "no coordination claim for {}/{}/{} to acknowledge a message",
            key.owner, key.repo_name, key.job_id
        ),
    })?;
    if claim.agent_id != request.agent_id {
        return Err(held_error(&claim));
    }
    if ack_targets_request(message, request) {
        Ok(())
    } else {
        Err(ack_recipient_error(request))
    }
}

fn ack_targets_request(message: &CoordMessage, request: AckRequest<'_>) -> bool {
    if !broadcast_ack_in_scope(message, request) {
        return false;
    }
    let fields = [
        (message.to_owner.as_deref(), request.owner),
        (message.to_repo_name.as_deref(), request.repo_name),
        (message.to_job_id.as_deref(), request.job_id),
        (message.to_agent_id.as_deref(), request.agent_id),
    ];
    fields
        .iter()
        .all(|(expected, actual)| field_matches(*expected, actual))
}

fn broadcast_ack_in_scope(message: &CoordMessage, request: AckRequest<'_>) -> bool {
    if message.to_job_id.is_some() {
        return true;
    }
    if message.from_owner != request.owner {
        return false;
    }
    message.from_repo_name == request.repo_name
}

fn field_matches(expected: Option<&str>, actual: &str) -> bool {
    expected.is_none_or(|value| value == actual)
}

fn ack_recipient_error(request: AckRequest<'_>) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "job {}/{}/{} is not the recipient of message {}",
            request.owner, request.repo_name, request.job_id, request.message_id
        ),
    }
}

pub(super) fn transfer_on_handoff_ack(
    tx: &rusqlite::Transaction<'_>,
    handoff: &CoordMessage,
    request: AckRequest<'_>,
    now: i64,
) -> Result<CoordClaim> {
    require_handoff_ack_scope(handoff, request)?;
    require_handoff_target_agent(handoff, request)?;
    apply_handoff_transfer(tx, handoff, request, now)
}

fn require_handoff_ack_scope(handoff: &CoordMessage, request: AckRequest<'_>) -> Result<()> {
    if handoff_source_matches(handoff, request) {
        return Ok(());
    }
    Err(Error::PolicyViolation {
        code: PolicyCode::CoordClaimMissing,
        message: format!(
            "job {}/{}/{} is not the source of handoff {}",
            request.owner, request.repo_name, request.job_id, handoff.id
        ),
    })
}

fn handoff_source_matches(handoff: &CoordMessage, request: AckRequest<'_>) -> bool {
    [
        (handoff.from_owner.as_str(), request.owner),
        (handoff.from_repo_name.as_str(), request.repo_name),
        (handoff.from_job_id.as_str(), request.job_id),
    ]
    .iter()
    .all(|(expected, actual)| expected == actual)
}

fn require_handoff_target_agent(handoff: &CoordMessage, request: AckRequest<'_>) -> Result<()> {
    let expected_agent = handoff
        .to_agent_id
        .as_deref()
        .ok_or_else(|| Error::LeaseStore {
            context: "ack coord handoff",
            message: "handoff is missing a target agent".to_owned(),
        })?;
    if expected_agent == request.agent_id {
        return Ok(());
    }
    Err(Error::PolicyViolation {
        code: PolicyCode::CoordClaimHeld,
        message: format!(
            "handoff {} is addressed to `{expected_agent}`, not `{}`",
            handoff.id, request.agent_id
        ),
    })
}

fn apply_handoff_transfer(
    tx: &rusqlite::Transaction<'_>,
    handoff: &CoordMessage,
    request: AckRequest<'_>,
    now: i64,
) -> Result<CoordClaim> {
    let from_key = JobKey {
        owner: &handoff.from_owner,
        repo_name: &handoff.from_repo_name,
        job_id: &handoff.from_job_id,
    };
    let current = load_claim_tx(tx, from_key)?
        .ok_or_else(|| coord_missing("handoff source claim is missing"))?;
    refuse_dead_or_stale_handoff(&current, handoff)?;
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

fn refuse_dead_or_stale_handoff(current: &CoordClaim, handoff: &CoordMessage) -> Result<()> {
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
        return Err(stale_error(current, handoff.owner_generation));
    }
    Ok(())
}
