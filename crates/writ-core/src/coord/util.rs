//! Shared coord error/time helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, PolicyCode};

use super::types::CoordClaim;

pub(super) fn terminal_allocation(state: &str) -> bool {
    matches!(state, "RELEASED" | "TOMBSTONED")
}

pub(super) fn held_error(claim: &CoordClaim) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordClaimHeld,
        message: format!(
            "job {}/{}/{} is owned by `{}` (generation {}); pause is not permission to seize WIP",
            claim.owner, claim.repo_name, claim.job_id, claim.agent_id, claim.owner_generation
        ),
    }
}

pub(super) fn stale_error(claim: &CoordClaim, expected: i64) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::CoordStaleGeneration,
        message: format!(
            "handoff generation {expected} is stale; live generation is {}",
            claim.owner_generation
        ),
    }
}

pub(super) fn coord_missing(message: &str) -> Error {
    Error::LeaseStore {
        context: "coord store",
        message: message.to_owned(),
    }
}

pub(super) fn coord_err(context: &'static str, err: rusqlite::Error) -> Error {
    Error::LeaseStore {
        context,
        message: err.to_string(),
    }
}

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
