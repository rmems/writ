//! Minimal SQLite lease store for Phase 1 hook admission.
//!
//! This is a skeleton, not the full MCP coordination store. `WorktreeCreate`
//! writes a row; `WorktreeRemove` releases it without deleting the identity so
//! verified reclaim can prove ownership. Budget columns are reserved and
//! nullable; they are not enforced here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, PolicyCode, Result};

/// Lease admission modes reserved by #124. Phase 1 only uses
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

/// Agent-registry upsert payload.
#[derive(Debug, Clone, Copy)]
pub struct AgentIdentity<'a> {
    pub agent_id: &'a str,
    pub agent_type: &'a str,
    pub session_id: Option<&'a str>,
}

struct LeaseSql {
    where_sql: &'static str,
    context: &'static str,
}

const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, worktree_path, start_commit, mode, ttl, heartbeat, max_files, max_churn, max_fix_cycles, fix_cycles, released_at FROM leases";

/// SQLite-backed lease and agent registry.
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
        conn.execute_batch(
            "
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
            CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY,
                agent_type TEXT NOT NULL,
                session_id TEXT,
                started_at INTEGER NOT NULL,
                stopped_at INTEGER
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
    ///
    /// The conflict target `(owner, repo_name, job_id)` refreshes in place only
    /// when the existing row is released or names the same `worktree_path`; an
    /// active lease held by a different checkout path is never seized — the
    /// grant fails with [`PolicyCode::LeaseConflict`].
    pub fn grant(&self, grant: LeaseGrant<'_>) -> Result<Lease> {
        let now = now_secs();
        let repo = path_text(grant.repo);
        let worktree_path = path_text(grant.worktree_path);
        let branch_ref = format!("refs/heads/{}", grant.branch);
        let conn = self.lock()?;
        let changed = conn.execute(
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
            WHERE leases.released_at IS NOT NULL
                OR leases.worktree_path = excluded.worktree_path
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
        if changed == 0 {
            return Err(Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                message: format!(
                    "job `{}` already holds an active lease for a different worktree path",
                    grant.job_id
                ),
            });
        }
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
            LeaseSql {
                where_sql: "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3 AND branch = ?4",
                context: "lookup resume identity",
            },
            params![key.owner, key.repo_name, key.job_id, key.branch],
        )
    }

    /// Look up a lease by owner/repo/job.
    pub fn find_job(&self, key: JobKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            LeaseSql {
                where_sql: "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
                context: "lookup lease",
            },
            params![key.owner, key.repo_name, key.job_id],
        )
    }

    /// All currently held (unreleased) leases, in insertion order.
    pub fn list_active(&self) -> Result<Vec<Lease>> {
        let query = format!("{LEASE_SELECT} WHERE released_at IS NULL ORDER BY id");
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| lease_err("list active leases", e))?;
        let rows = stmt
            .query_map([], lease_from_row)
            .map_err(|e| lease_err("list active leases", e))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| lease_err("list active leases", e))
    }

    /// Look up a lease by worktree path.
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            LeaseSql {
                where_sql: "WHERE worktree_path = ?1",
                context: "lookup lease by path",
            },
            params![path_text(worktree_path)],
        )
    }

    fn query_lease(&self, sql: LeaseSql, params: impl rusqlite::Params) -> Result<Option<Lease>> {
        let query = format!("{LEASE_SELECT} {}", sql.where_sql);
        let conn = self.lock()?;
        conn.query_row(&query, params, lease_from_row)
            .optional()
            .map_err(|e| lease_err(sql.context, e))
    }

    /// Upsert a live agent-registry row.
    pub fn upsert_agent(&self, identity: AgentIdentity<'_>) -> Result<()> {
        let now = now_secs();
        let conn = self.lock()?;
        conn.execute(
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
        .map_err(|e| lease_err("upsert agent", e))?;
        Ok(())
    }

    /// Retire an agent-registry row.
    pub fn retire_agent(&self, agent_id: &str) -> Result<()> {
        let now = now_secs();
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET stopped_at = ?1 WHERE agent_id = ?2",
            params![now, agent_id],
        )
        .map_err(|e| lease_err("retire agent", e))?;
        Ok(())
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
    let identity = lease_identity(row)?;
    let limits = lease_limits(row)?;
    Ok(Lease {
        repo: identity.repo,
        owner: identity.owner,
        repo_name: identity.repo_name,
        job_id: identity.job_id,
        branch: identity.branch,
        branch_ref: identity.branch_ref,
        worktree_path: identity.worktree_path,
        start_commit: identity.start_commit,
        mode: identity.mode,
        ttl: limits.ttl,
        heartbeat: limits.heartbeat,
        max_files: limits.max_files,
        max_churn: limits.max_churn,
        max_fix_cycles: limits.max_fix_cycles,
        fix_cycles: limits.fix_cycles,
        released_at: limits.released_at,
    })
}

struct LeaseIdentity {
    repo: String,
    owner: String,
    repo_name: String,
    job_id: String,
    branch: String,
    branch_ref: String,
    worktree_path: String,
    start_commit: String,
    mode: LeaseMode,
}

struct LeaseLimits {
    ttl: Option<i64>,
    heartbeat: Option<i64>,
    max_files: Option<i64>,
    max_churn: Option<i64>,
    max_fix_cycles: Option<i64>,
    fix_cycles: Option<i64>,
    released_at: Option<i64>,
}

fn lease_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseIdentity> {
    let names = lease_names(row)?;
    let refs = lease_refs(row)?;
    Ok(LeaseIdentity {
        repo: names.0,
        owner: names.1,
        repo_name: names.2,
        job_id: names.3,
        branch: names.4,
        branch_ref: refs.0,
        worktree_path: refs.1,
        start_commit: refs.2,
        mode: refs.3,
    })
}

fn lease_names(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn lease_refs(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, LeaseMode)> {
    Ok((
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        LeaseMode::parse(&row.get::<_, String>(8)?),
    ))
}

fn lease_limits(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseLimits> {
    let reserved = lease_budget_cells(row)?;
    Ok(LeaseLimits {
        ttl: row.get(9)?,
        heartbeat: row.get(10)?,
        max_files: reserved.max_files,
        max_churn: reserved.max_churn,
        max_fix_cycles: reserved.max_fix_cycles,
        fix_cycles: reserved.fix_cycles,
        released_at: row.get(15)?,
    })
}

struct BudgetCells {
    max_files: Option<i64>,
    max_churn: Option<i64>,
    max_fix_cycles: Option<i64>,
    fix_cycles: Option<i64>,
}

fn lease_budget_cells(row: &rusqlite::Row<'_>) -> rusqlite::Result<BudgetCells> {
    Ok(BudgetCells {
        max_files: row.get(11)?,
        max_churn: row.get(12)?,
        max_fix_cycles: row.get(13)?,
        fix_cycles: row.get(14)?,
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
        assert_eq!(held.max_files, None);
        assert_eq!(held.fix_cycles, None);
        assert!(held.released_at.is_none());

        let released = store.release_by_path(&wt).unwrap().unwrap();
        assert_eq!(released.mode, LeaseMode::Unassigned);
        assert!(released.released_at.is_some());

        let active = store.list_active().unwrap();
        assert!(active.is_empty(), "released lease must not list as active");

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
    }

    #[test]
    fn grant_refuses_active_lease_held_by_different_path() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt_a = tmp.path().join("checkouts/a");
        let wt_b = tmp.path().join("checkouts/b");
        fn grant_for<'a>(repo: &'a Path, wt: &'a Path) -> LeaseGrant<'a> {
            LeaseGrant {
                repo,
                owner: "local",
                repo_name: "repo",
                job_id: "job-1",
                branch: "hive/job-1",
                worktree_path: wt,
                start_commit: "abc123",
            }
        }

        store.grant(grant_for(&repo, &wt_a)).unwrap();

        // A different path for the same job id is a conflict, not a seize.
        let err = store.grant(grant_for(&repo, &wt_b)).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                ..
            }
        ));

        // Re-granting the same path refreshes in place.
        let refreshed = store.grant(grant_for(&repo, &wt_a)).unwrap();
        assert!(refreshed.released_at.is_none());
        assert_eq!(refreshed.worktree_path, wt_a);

        // After release, the other path can take the identity.
        store.release_by_path(&wt_a).unwrap();
        let moved = store.grant(grant_for(&repo, &wt_b)).unwrap();
        assert_eq!(moved.worktree_path, wt_b);
    }

    #[test]
    fn agent_upsert_and_retire() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        store
            .upsert_agent(AgentIdentity {
                agent_id: "agent-1",
                agent_type: "Explore",
                session_id: Some("session-1"),
            })
            .unwrap();
        store.retire_agent("agent-1").unwrap();
        let conn = store.conn.lock().unwrap();
        let stopped: Option<i64> = conn
            .query_row(
                "SELECT stopped_at FROM agents WHERE agent_id = ?1",
                params!["agent-1"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stopped.is_some());
    }
}
