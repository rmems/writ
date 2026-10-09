//! Live checkout-path uniqueness checks.

use rusqlite::{Connection, OptionalExtension, params};

use super::{Error, PolicyCode, Result, lease_err};

pub(super) struct LivePathClaim<'a> {
    pub worktree_path: &'a str,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
}

fn live_path_held_error(job_id: &str) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::LeaseConflict,
        message: format!("checkout already holds an active lease for job `{job_id}`"),
    }
}

fn is_live_path_unique_violation(err: &rusqlite::Error) -> bool {
    let text = err.to_string();
    text.contains("UNIQUE constraint failed")
        && (text.contains("leases_live_worktree_path") || text.contains("leases.worktree_path"))
}

pub(super) fn map_live_path_constraint(err: rusqlite::Error, context: &'static str) -> Error {
    if is_live_path_unique_violation(&err) {
        Error::PolicyViolation {
            code: PolicyCode::LeaseConflict,
            message: "checkout already holds an active lease for another job".to_owned(),
        }
    } else {
        lease_err(context, err)
    }
}

pub(super) fn reject_live_path_occupant(
    conn: &Connection,
    claim: LivePathClaim<'_>,
    context: &'static str,
) -> Result<()> {
    let occupant: Option<String> = conn
        .query_row(
            "
            SELECT job_id FROM leases
            WHERE worktree_path = ?1
              AND released_at IS NULL
              AND tombstoned_at IS NULL
              AND NOT (owner = ?2 AND repo_name = ?3 AND job_id = ?4)
            ",
            params![
                claim.worktree_path,
                claim.owner,
                claim.repo_name,
                claim.job_id
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| lease_err(context, e))?;
    if let Some(held_by) = occupant {
        return Err(live_path_held_error(&held_by));
    }
    Ok(())
}
