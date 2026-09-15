//! Queryable SQLite lease store used by the hook boundary.
//!
//! Schema is reserved for later coordination columns (`max_files`, `max_churn`,
//! `max_fix_cycles`, `fix_cycles`) so Phase 4 can add them without a rewrite.
//! GitHub #124 / #167.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// Lease modes recorded on a worktree row.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LeaseMode {
    Unassigned,
    WriterLocked,
    ReviewOnly,
    NeedsHuman,
    Blocked,
    MergeReady,
}

impl LeaseMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unassigned => "UNASSIGNED",
            Self::WriterLocked => "WRITER_LOCKED",
            Self::ReviewOnly => "REVIEW_ONLY",
            Self::NeedsHuman => "NEEDS_HUMAN",
            Self::Blocked => "BLOCKED",
            Self::MergeReady => "MERGE_READY",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "UNASSIGNED" => Some(Self::Unassigned),
            "WRITER_LOCKED" => Some(Self::WriterLocked),
            "REVIEW_ONLY" => Some(Self::ReviewOnly),
            "NEEDS_HUMAN" => Some(Self::NeedsHuman),
            "BLOCKED" => Some(Self::Blocked),
            "MERGE_READY" => Some(Self::MergeReady),
            _ => None,
        }
    }
}

/// One lease row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Lease {
    pub worktree_path: String,
    pub repo: String,
    pub branch: String,
    pub owner: String,
    pub mode: LeaseMode,
    pub ttl: Option<i64>,
    pub heartbeat: Option<i64>,
}

/// One agent-registry row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AgentRow {
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub session_id: Option<String>,
    pub retired_at: Option<i64>,
}

/// File-backed SQLite lease store.
#[derive(Debug)]
pub struct LeaseStore {
    path: PathBuf,
    conn: Connection,
}

impl LeaseStore {
    /// Open (or create) the store at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                context: "create lease store directory",
                source: e,
            })?;
        }
        let conn = Connection::open(&path).map_err(lease_io)?;
        conn.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(lease_io)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS leases (
                worktree_path TEXT PRIMARY KEY NOT NULL,
                repo TEXT NOT NULL,
                branch TEXT NOT NULL,
                owner TEXT NOT NULL,
                mode TEXT NOT NULL,
                ttl INTEGER,
                heartbeat INTEGER,
                max_files INTEGER,
                max_churn INTEGER,
                max_fix_cycles INTEGER,
                fix_cycles INTEGER
            );
            CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY NOT NULL,
                agent_type TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                retired_at INTEGER
            );",
        )
        .map_err(lease_io)?;
        Ok(Self { path, conn })
    }

    /// Filesystem path of this store, for direct SQL queries in tests.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert or replace a writer lease for `lease.worktree_path`.
    pub fn grant(&self, lease: &Lease) -> Result<()> {
        let now = unix_now();
        self.conn
            .execute(
                "INSERT INTO leases (
                    worktree_path, repo, branch, owner, mode, ttl, heartbeat,
                    max_files, max_churn, max_fix_cycles, fix_cycles
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, NULL)
                 ON CONFLICT(worktree_path) DO UPDATE SET
                    repo = excluded.repo,
                    branch = excluded.branch,
                    owner = excluded.owner,
                    mode = excluded.mode,
                    ttl = excluded.ttl,
                    heartbeat = excluded.heartbeat",
                params![
                    lease.worktree_path,
                    lease.repo,
                    lease.branch,
                    lease.owner,
                    lease.mode.as_str(),
                    lease.ttl,
                    lease.heartbeat.unwrap_or(now),
                ],
            )
            .map_err(lease_io)?;
        Ok(())
    }

    /// Delete the lease row for `worktree_path`. Missing rows are not an error.
    pub fn release(&self, worktree_path: &Path) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM leases WHERE worktree_path = ?1",
                params![path_key(worktree_path)],
            )
            .map_err(lease_io)?;
        Ok(())
    }

    /// Look up a lease by worktree path.
    pub fn get(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.conn
            .query_row(
                "SELECT worktree_path, repo, branch, owner, mode, ttl, heartbeat
                 FROM leases WHERE worktree_path = ?1",
                params![path_key(worktree_path)],
                lease_from_row,
            )
            .optional()
            .map_err(lease_io)
    }

    /// Upsert an agent-registry row on SubagentStart.
    pub fn upsert_agent(
        &self,
        agent_id: &str,
        agent_type: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        let now = unix_now();
        self.conn
            .execute(
                "INSERT INTO agents (agent_id, agent_type, session_id, created_at, retired_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)
                 ON CONFLICT(agent_id) DO UPDATE SET
                    agent_type = excluded.agent_type,
                    session_id = excluded.session_id,
                    retired_at = NULL",
                params![agent_id, agent_type, session_id, now],
            )
            .map_err(lease_io)?;
        Ok(())
    }

    /// Mark an agent-registry row retired on SubagentStop.
    pub fn retire_agent(&self, agent_id: &str) -> Result<()> {
        let now = unix_now();
        self.conn
            .execute(
                "UPDATE agents SET retired_at = ?1 WHERE agent_id = ?2",
                params![now, agent_id],
            )
            .map_err(lease_io)?;
        Ok(())
    }

    /// Read an agent row by id.
    pub fn get_agent(&self, agent_id: &str) -> Result<Option<AgentRow>> {
        self.conn
            .query_row(
                "SELECT agent_id, agent_type, session_id, retired_at
                 FROM agents WHERE agent_id = ?1",
                params![agent_id],
                |row| {
                    Ok(AgentRow {
                        agent_id: row.get(0)?,
                        agent_type: row.get(1)?,
                        session_id: row.get(2)?,
                        retired_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(lease_io)
    }
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let mode: String = row.get(4)?;
    Ok(Lease {
        worktree_path: row.get(0)?,
        repo: row.get(1)?,
        branch: row.get(2)?,
        owner: row.get(3)?,
        mode: LeaseMode::parse(&mode).unwrap_or(LeaseMode::Unassigned),
        ttl: row.get(5)?,
        heartbeat: row.get(6)?,
    })
}

pub(crate) fn path_key(path: &Path) -> String {
    crate::paths::canonicalize_for_tools(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lease_io(err: rusqlite::Error) -> Error {
    Error::Io {
        context: "lease store",
        source: std::io::Error::other(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn grant_get_release_round_trip() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::open(dir.path().join("leases.sqlite")).unwrap();
        let path = Path::new("/tmp/wt/job");
        store
            .grant(&Lease {
                worktree_path: path_key(path),
                repo: "sample".into(),
                branch: "feature/x".into(),
                owner: "acme".into(),
                mode: LeaseMode::WriterLocked,
                ttl: None,
                heartbeat: None,
            })
            .unwrap();
        let got = store.get(path).unwrap().expect("row");
        assert_eq!(got.branch, "feature/x");
        assert_eq!(got.mode, LeaseMode::WriterLocked);
        store.release(path).unwrap();
        assert!(store.get(path).unwrap().is_none());
    }

    #[test]
    fn agent_upsert_and_retire() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::open(dir.path().join("leases.sqlite")).unwrap();
        store
            .upsert_agent("agent-1", Some("worker"), Some("sess"))
            .unwrap();
        let row = store.get_agent("agent-1").unwrap().expect("row");
        assert_eq!(row.agent_type.as_deref(), Some("worker"));
        assert!(row.retired_at.is_none());
        store.retire_agent("agent-1").unwrap();
        let retired = store.get_agent("agent-1").unwrap().expect("row");
        assert!(retired.retired_at.is_some());
    }
}
