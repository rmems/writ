//! Durable worktree ownership identity for verified reclaim.
//!
//! `WorktreeCreate` grants a writer lock; `WorktreeRemove` releases it without
//! deleting the row so a later create can prove the branch still belongs to
//! this owner/repo/job. Budget columns are reserved and nullable; they are not
//! enforced here. Crash-window reconciliation between git mutation and this
//! record is [#136](https://github.com/rmems/writ/issues/136).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// Lease admission modes reserved by #124. Verified reclaim uses
/// `WriterLocked` (held) and `Unassigned` (released, identity kept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn parse(value: &str) -> Self {
        match value {
            "WRITER_LOCKED" => Self::WriterLocked,
            "REVIEW_ONLY" => Self::ReviewOnly,
            "NEEDS_HUMAN" => Self::NeedsHuman,
            "BLOCKED" => Self::Blocked,
            "MERGE_READY" => Self::MergeReady,
            _ => Self::Unassigned,
        }
    }
}

/// One lease row. Budget fields are reserved and unused by enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub repo: String,
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub branch: String,
    pub branch_ref: String,
    pub worktree_path: String,
    pub start_commit: String,
    pub mode: LeaseMode,
    pub ttl: Option<i64>,
    pub heartbeat: Option<i64>,
    pub max_files: Option<i64>,
    pub max_churn: Option<i64>,
    pub max_fix_cycles: Option<i64>,
    pub fix_cycles: Option<i64>,
    pub released_at: Option<i64>,
}

/// Inputs required to grant or refresh a writer lock.
#[derive(Debug, Clone, Copy)]
pub struct LeaseGrant<'a> {
    pub repo: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
    pub worktree_path: &'a Path,
    pub start_commit: &'a str,
}

/// Durable resume identity key: owner/repo/job plus the branch name.
#[derive(Debug, Clone, Copy)]
pub struct ResumeKey<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
}

/// Owner/repo/job identity without the branch (unique lease row key).
#[derive(Debug, Clone, Copy)]
pub struct JobKey<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
}

const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, \
     worktree_path, start_commit, mode, ttl, heartbeat, max_files, max_churn, \
     max_fix_cycles, fix_cycles, released_at FROM leases";

/// SQLite-backed lease identity store.
#[derive(Debug)]
pub struct LeaseStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl LeaseStore {
    /// Open (or create) the store at `path` and apply the Phase 1 schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                context: "create lease store directory",
                source: e,
            })?;
        }
        let conn = Connection::open(&path).map_err(|e| lease_err("open lease store", e))?;
        conn.busy_timeout(Duration::from_millis(5000))
            .map_err(|e| lease_err("set lease busy timeout", e))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS leases (
                id INTEGER PRIMARY KEY,
                repo TEXT NOT NULL,
                owner TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                job_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                branch_ref TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                start_commit TEXT NOT NULL,
                mode TEXT NOT NULL,
                ttl INTEGER,
                heartbeat INTEGER,
                max_files INTEGER,
                max_churn INTEGER,
                max_fix_cycles INTEGER,
                fix_cycles INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                released_at INTEGER,
                UNIQUE(owner, repo_name, job_id)
            );
            ",
        )
        .map_err(|e| lease_err("initialize lease schema", e))?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    /// Filesystem path of this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Grant or refresh a writer lock, keeping reserved budget columns null.
    pub fn grant(&self, grant: LeaseGrant<'_>) -> Result<Lease> {
        let now = now_secs();
        let repo = path_text(grant.repo);
        let worktree_path = path_text(grant.worktree_path);
        let branch_ref = format!("refs/heads/{}", grant.branch);
        let conn = self.lock()?;
        conn.execute(
            "
            INSERT INTO leases (
                repo, owner, repo_name, job_id, branch, branch_ref, worktree_path,
                start_commit, mode, ttl, heartbeat, max_files, max_churn,
                max_fix_cycles, fix_cycles, created_at, updated_at, released_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, NULL, NULL, NULL, NULL, ?10, ?10, NULL)
            ON CONFLICT(owner, repo_name, job_id) DO UPDATE SET
                repo = excluded.repo,
                branch = excluded.branch,
                branch_ref = excluded.branch_ref,
                worktree_path = excluded.worktree_path,
                start_commit = excluded.start_commit,
                mode = excluded.mode,
                heartbeat = excluded.heartbeat,
                updated_at = excluded.updated_at,
                released_at = NULL
            ",
            params![
                repo,
                grant.owner,
                grant.repo_name,
                grant.job_id,
                grant.branch,
                branch_ref,
                worktree_path,
                grant.start_commit,
                LeaseMode::WriterLocked.as_str(),
                now,
            ],
        )
        .map_err(|e| lease_err("grant lease", e))?;
        drop(conn);
        self.find_job(JobKey {
            owner: grant.owner,
            repo_name: grant.repo_name,
            job_id: grant.job_id,
        })?
        .ok_or_else(|| Error::LeaseStore {
            context: "grant lease",
            message: "lease row missing after grant".to_owned(),
        })
    }

    /// Release a writer lock by worktree path, keeping the identity row.
    pub fn release_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        let now = now_secs();
        let path = path_text(worktree_path);
        let conn = self.lock()?;
        conn.execute(
            "
            UPDATE leases
            SET mode = ?1, released_at = ?2, updated_at = ?2
            WHERE worktree_path = ?3 AND released_at IS NULL
            ",
            params![LeaseMode::Unassigned.as_str(), now, path],
        )
        .map_err(|e| lease_err("release lease", e))?;
        drop(conn);
        self.find_by_path(worktree_path)
    }

    /// Durable resume identity for an owner/repo/job/branch, including released rows.
    pub fn find_resume(&self, key: ResumeKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3 AND branch = ?4",
            "lookup resume identity",
            params![key.owner, key.repo_name, key.job_id, key.branch],
        )
    }

    /// Look up a lease by owner/repo/job.
    pub fn find_job(&self, key: JobKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            "lookup lease",
            params![key.owner, key.repo_name, key.job_id],
        )
    }

    /// Look up a lease by worktree path.
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE worktree_path = ?1",
            "lookup lease by path",
            params![path_text(worktree_path)],
        )
    }

    fn query_lease(
        &self,
        where_sql: &str,
        context: &'static str,
        params: impl rusqlite::Params,
    ) -> Result<Option<Lease>> {
        let query = format!("{LEASE_SELECT} {where_sql}");
        let conn = self.lock()?;
        conn.query_row(&query, params, lease_from_row)
            .optional()
            .map_err(|e| lease_err(context, e))
    }

    /// True when the `leases` table has the reserved nullable budget columns.
    pub fn has_budget_columns(&self) -> Result<bool> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("PRAGMA table_info(leases)")
            .map_err(|e| lease_err("inspect lease schema", e))?;
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| lease_err("inspect lease schema", e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| lease_err("inspect lease schema", e))?;
        Ok(["max_files", "max_churn", "max_fix_cycles", "fix_cycles"]
            .into_iter()
            .all(|col| names.iter().any(|name| name == col)))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::LeaseStore {
            context: "lease store",
            message: "lease store mutex poisoned".to_owned(),
        })
    }
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        repo: row.get(0)?,
        owner: row.get(1)?,
        repo_name: row.get(2)?,
        job_id: row.get(3)?,
        branch: row.get(4)?,
        branch_ref: row.get(5)?,
        worktree_path: row.get(6)?,
        start_commit: row.get(7)?,
        mode: LeaseMode::parse(&row.get::<_, String>(8)?),
        ttl: row.get(9)?,
        heartbeat: row.get(10)?,
        max_files: row.get(11)?,
        max_churn: row.get(12)?,
        max_fix_cycles: row.get(13)?,
        fix_cycles: row.get(14)?,
        released_at: row.get(15)?,
    })
}

fn lease_err(context: &'static str, err: rusqlite::Error) -> Error {
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

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn grant_release_preserves_identity_and_budget_columns() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        assert!(store.has_budget_columns().unwrap());

        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("worktrees/acme/sample/gh-42");
        let grant = LeaseGrant {
            repo: &repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "gh-42",
            branch: "hive/gh-42",
            worktree_path: &wt,
            start_commit: "abc123",
        };
        let held = store.grant(grant).unwrap();
        assert_eq!(held.mode, LeaseMode::WriterLocked);
        assert_eq!(held.branch_ref, "refs/heads/hive/gh-42");
        assert_eq!(held.max_files, None);
        assert_eq!(held.fix_cycles, None);
        assert!(held.released_at.is_none());

        let released = store.release_by_path(&wt).unwrap().unwrap();
        assert_eq!(released.mode, LeaseMode::Unassigned);
        assert!(released.released_at.is_some());

        let resume = store
            .find_resume(ResumeKey {
                owner: "acme",
                repo_name: "sample",
                job_id: "gh-42",
                branch: "hive/gh-42",
            })
            .unwrap()
            .unwrap();
        assert_eq!(resume.start_commit, "abc123");
        assert_eq!(resume.branch_ref, "refs/heads/hive/gh-42");
        assert_eq!(resume.worktree_path, path_text(&wt));
        assert_eq!(resume.mode, LeaseMode::Unassigned);
    }

    #[test]
    fn open_sets_busy_timeout() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let conn = store.lock().unwrap();
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn resume_lookup_requires_the_same_job_and_branch() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("worktrees/acme/sample/gh-42");
        store
            .grant(LeaseGrant {
                repo: &repo,
                owner: "acme",
                repo_name: "sample",
                job_id: "gh-42",
                branch: "hive/gh-42",
                worktree_path: &wt,
                start_commit: "abc123",
            })
            .unwrap();

        assert!(
            store
                .find_resume(ResumeKey {
                    owner: "acme",
                    repo_name: "sample",
                    job_id: "gh-99",
                    branch: "hive/gh-42",
                })
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .find_resume(ResumeKey {
                    owner: "acme",
                    repo_name: "sample",
                    job_id: "gh-42",
                    branch: "hive/other",
                })
                .unwrap()
                .is_none()
        );
    }
}
