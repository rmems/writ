//! Read-only overlay for RM-825 `coord_claims` / `coord_messages`.
//!
//! Missing tables are not an error: this consumer never creates them.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::types::CoordOverlay;

#[derive(Debug, Clone, Default)]
pub(crate) struct CoordSnapshot {
    pub available: bool,
    claims: BTreeMap<JobId, ClaimRow>,
    messages: Vec<MessageRow>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct JobId {
    owner: String,
    repo_name: String,
    job_id: String,
}

#[derive(Debug, Clone)]
struct ClaimRow {
    agent_id: String,
    session_id: Option<String>,
    intent: Option<String>,
    owner_generation: i64,
    paused_at: Option<i64>,
    declared_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct MessageRow {
    kind: String,
    from_agent_id: String,
    from_job_id: String,
    to_owner: Option<String>,
    to_repo_name: Option<String>,
    to_job_id: Option<String>,
    body: String,
    acked_at: Option<i64>,
}

impl CoordSnapshot {
    pub(crate) fn overlay_for(&self, owner: &str, repo_name: &str, job_id: &str) -> CoordOverlay {
        let key = JobId {
            owner: owner.to_owned(),
            repo_name: repo_name.to_owned(),
            job_id: job_id.to_owned(),
        };
        let claim = self.claims.get(&key);
        let waiting_on = waiting_on_for(&self.messages, owner, repo_name, job_id);
        let overlaps = overlaps_for(&self.messages, owner, repo_name, job_id);
        CoordOverlay {
            agent_id: claim.map(|c| c.agent_id.clone()),
            session_id: claim.and_then(|c| c.session_id.clone()),
            intent: claim.and_then(|c| c.intent.clone()),
            owner_generation: claim.map(|c| c.owner_generation),
            paused: claim.and_then(|c| c.paused_at).is_some(),
            declared_paths: claim.map(|c| c.declared_paths.clone()).unwrap_or_default(),
            overlaps,
            waiting_on,
        }
    }
}

/// Open the lease DB read-only and load coord rows when the tables exist.
pub(crate) fn load_coord_snapshot(path: &Path) -> CoordSnapshot {
    let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return CoordSnapshot::default();
    };
    if !table_exists(&conn, "coord_claims") {
        return CoordSnapshot::default();
    }
    let claims = load_claims(&conn).unwrap_or_default();
    let messages = if table_exists(&conn, "coord_messages") {
        load_messages(&conn).unwrap_or_default()
    } else {
        Vec::new()
    };
    CoordSnapshot {
        available: true,
        claims,
        messages,
    }
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn load_claims(conn: &Connection) -> rusqlite::Result<BTreeMap<JobId, ClaimRow>> {
    let mut stmt = conn.prepare(
        "SELECT owner, repo_name, job_id, agent_id, session_id, intent,
                declared_paths, owner_generation, paused_at
         FROM coord_claims",
    )?;
    let rows = stmt.query_map([], |row| {
        let paths_json: String = row.get(6)?;
        Ok((
            JobId {
                owner: row.get(0)?,
                repo_name: row.get(1)?,
                job_id: row.get(2)?,
            },
            ClaimRow {
                agent_id: row.get(3)?,
                session_id: row.get(4)?,
                intent: row.get(5)?,
                declared_paths: parse_paths(&paths_json),
                owner_generation: row.get(7)?,
                paused_at: row.get(8)?,
            },
        ))
    })?;
    rows.collect()
}

fn load_messages(conn: &Connection) -> rusqlite::Result<Vec<MessageRow>> {
    let mut stmt = conn.prepare(
        "SELECT kind, from_agent_id, from_job_id, to_owner, to_repo_name,
                to_job_id, body, acked_at FROM coord_messages",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MessageRow {
            kind: row.get(0)?,
            from_agent_id: row.get(1)?,
            from_job_id: row.get(2)?,
            to_owner: row.get(3)?,
            to_repo_name: row.get(4)?,
            to_job_id: row.get(5)?,
            body: row.get(6)?,
            acked_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

fn parse_paths(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn waiting_on_for(
    messages: &[MessageRow],
    owner: &str,
    repo_name: &str,
    job_id: &str,
) -> Option<String> {
    messages.iter().find_map(|msg| {
        if msg.acked_at.is_some() || !targets_job(msg, owner, repo_name, job_id) {
            return None;
        }
        match msg.kind.as_str() {
            "help" | "handoff" | "dependency" => Some(format!(
                "{}:{}:{}",
                msg.kind, msg.from_agent_id, msg.from_job_id
            )),
            _ => None,
        }
    })
}

fn overlaps_for(
    messages: &[MessageRow],
    owner: &str,
    repo_name: &str,
    job_id: &str,
) -> Vec<String> {
    messages
        .iter()
        .filter(|msg| {
            msg.acked_at.is_none()
                && msg.kind == "overlap"
                && targets_job(msg, owner, repo_name, job_id)
        })
        .map(|msg| format!("overlap:{}:{}", msg.from_job_id, msg.body))
        .collect()
}

fn targets_job(msg: &MessageRow, owner: &str, repo_name: &str, job_id: &str) -> bool {
    msg.to_owner.as_deref() == Some(owner)
        && msg.to_repo_name.as_deref() == Some(repo_name)
        && msg.to_job_id.as_deref() == Some(job_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_tables_are_unavailable_not_an_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("leases.db");
        Connection::open(&path).unwrap();
        let snap = load_coord_snapshot(&path);
        assert!(!snap.available);
        let overlay = snap.overlay_for("acme", "sample", "job-1");
        assert!(!overlay.paused);
        assert!(overlay.agent_id.is_none());
    }

    #[test]
    fn reads_paused_claim_and_unacked_help() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("leases.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE coord_claims (
                owner TEXT, repo_name TEXT, job_id TEXT, agent_id TEXT,
                session_id TEXT, intent TEXT, declared_paths TEXT,
                owner_generation INTEGER, paused_at INTEGER
            );
            CREATE TABLE coord_messages (
                kind TEXT, from_agent_id TEXT, from_job_id TEXT,
                to_owner TEXT, to_repo_name TEXT, to_job_id TEXT,
                body TEXT, acked_at INTEGER
            );
            INSERT INTO coord_claims VALUES (
                'acme','sample','job-1','agent-a','sess-1','fix',
                '[\"crates/writ-core\"]', 2, 99
            );
            INSERT INTO coord_messages VALUES (
                'help','agent-b','job-2','acme','sample','job-1','need review', NULL
            );
            INSERT INTO coord_messages VALUES (
                'overlap','agent-b','job-2','acme','sample','job-1','crates/a', NULL
            );
            ",
        )
        .unwrap();
        drop(conn);
        let snap = load_coord_snapshot(&path);
        assert!(snap.available);
        let overlay = snap.overlay_for("acme", "sample", "job-1");
        assert_eq!(overlay.agent_id.as_deref(), Some("agent-a"));
        assert!(overlay.paused);
        assert_eq!(overlay.waiting_on.as_deref(), Some("help:agent-b:job-2"));
        assert_eq!(overlay.overlaps.len(), 1);
    }

    #[test]
    fn missing_file_fails_open_without_creating_db() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("absent.db");
        let snap = load_coord_snapshot(&path);
        assert!(!snap.available);
        assert!(!path.exists());
    }
}
