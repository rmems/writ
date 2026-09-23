//! Minimal SQLite lease store for Phase 1 hook admission.
//!
//! This is a skeleton, not the full MCP coordination store. `WorktreeCreate`
//! writes a row; `WorktreeRemove` releases it without deleting the identity so
//! verified reclaim can prove ownership. Budget columns are reserved and
//! nullable; they are not enforced here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

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
    /// Stored value is not one of the known modes. Not treated as released.
    Unknown,
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
            Self::Unknown => "UNKNOWN",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "WRITER_LOCKED" => Self::WriterLocked,
            "REVIEW_ONLY" => Self::ReviewOnly,
            "NEEDS_HUMAN" => Self::NeedsHuman,
            "BLOCKED" => Self::Blocked,
            "MERGE_READY" => Self::MergeReady,
            "UNASSIGNED" => Self::Unassigned,
            _ => Self::Unknown,
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
    /// Stored mode text before mapping; unrecognized values survive verbatim.
    pub mode_raw: String,
    pub ttl: Option<i64>,
    pub heartbeat: Option<i64>,
    pub max_files: Option<i64>,
    pub max_churn: Option<i64>,
    pub max_fix_cycles: Option<i64>,
    pub fix_cycles: Option<i64>,
    pub released_at: Option<i64>,
    /// SQLite row id. Identity for the lease record, not an ownership generation.
    pub row_id: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One `agents` registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRecord {
    pub agent_id: String,
    pub agent_type: String,
    pub session_id: Option<String>,
    pub started_at: i64,
    pub stopped_at: Option<i64>,
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

const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, worktree_path, start_commit, mode, ttl, heartbeat, max_files, max_churn, max_fix_cycles, fix_cycles, released_at, id, created_at, updated_at FROM leases";

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
        configure_writer_connection(&conn)?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    /// Open an existing store without creating files or applying schema.
    pub fn open_read_only(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| lease_err("open lease store read-only", e))?;
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
    /// when the existing row is released or names the same `worktree_path`. An
    /// active lease held by a different checkout path is never seized. A
    /// different job cannot take a checkout path that already has an unreleased
    /// lease. Either conflict is [`PolicyCode::LeaseConflict`].
    ///
    /// Live-path uniqueness is enforced in one `BEGIN IMMEDIATE` transaction
    /// plus a partial unique index, so two independent connections cannot both
    /// become the live owner. The store mutex only serializes this process.
    pub fn grant(&self, grant: LeaseGrant<'_>) -> Result<Lease> {
        let mut conn = self.lock()?;
        grant_on(&mut conn, grant)?;
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
        let conn = self.lock()?;
        list_leases_on(
            &conn,
            "WHERE released_at IS NULL ORDER BY id",
            "list active leases",
        )
    }

    /// All lease identity rows, including released ones, in insertion order.
    pub fn list_all(&self) -> Result<Vec<Lease>> {
        let conn = self.lock()?;
        list_leases_on(&conn, "ORDER BY id", "list leases")
    }

    /// Agent registry rows, live first, then by start time.
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        let conn = self.lock()?;
        list_agents_on(&conn)
    }

    /// Leases and agents from one read transaction.
    ///
    /// The mutex serializes this connection only; the transaction keeps other
    /// processes' writes from splitting the two reads.
    pub fn snapshot(&self) -> Result<(Vec<Lease>, Vec<AgentRecord>)> {
        let conn = self.lock()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| lease_err("snapshot transaction", e))?;
        let leases = list_leases_on(&tx, "ORDER BY id", "list leases")?;
        let agents = list_agents_on(&tx)?;
        tx.commit().map_err(|e| lease_err("snapshot commit", e))?;
        Ok((leases, agents))
    }

    /// Look up a lease by worktree path.
    ///
    /// Prefers the live (unreleased) row when one exists. After sequential
    /// jobs have reused a checkout, released identity rows may share the path.
    /// rusqlite `query_row` returns the first row and ignores the rest;
    /// `query_one` is the API that errors on extra rows. `ORDER BY` + `LIMIT 1`
    /// picks the live holder, else the most recently released identity.
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            LeaseSql {
                where_sql: "WHERE worktree_path = ?1
                    ORDER BY CASE WHEN released_at IS NULL THEN 0 ELSE 1 END,
                             COALESCE(released_at, 0) DESC,
                             id DESC
                    LIMIT 1",
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

const WRITER_SCHEMA: &str = "
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
            CREATE UNIQUE INDEX IF NOT EXISTS leases_live_worktree_path
                ON leases(worktree_path) WHERE released_at IS NULL;
            ";

const GRANT_INSERT_SQL: &str = "
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
            ";

fn configure_writer_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_millis(5_000))
        .map_err(|e| lease_err("open lease store", e))?;
    conn.execute_batch(WRITER_SCHEMA)
        .map_err(|e| lease_err("initialize lease schema", e))?;
    Ok(())
}

fn grant_on(conn: &mut Connection, grant: LeaseGrant<'_>) -> Result<()> {
    let now = now_secs();
    let repo = path_text(grant.repo);
    let worktree_path = path_text(grant.worktree_path);
    let branch_ref = format!("refs/heads/{}", grant.branch);
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| lease_err("grant lease", e))?;
    if let Some(job_id) = live_path_occupant(&tx, &worktree_path, grant)? {
        return Err(path_held_conflict(&job_id));
    }
    let changed = insert_or_refresh_writer(&tx, grant, now, &repo, &worktree_path, &branch_ref)?;
    if changed == 0 {
        return Err(Error::PolicyViolation {
            code: PolicyCode::LeaseConflict,
            message: format!(
                "job `{}` already holds an active lease for a different worktree path",
                grant.job_id
            ),
        });
    }
    tx.commit().map_err(|e| lease_err("grant lease", e))?;
    Ok(())
}

fn live_path_occupant(
    conn: &Connection,
    worktree_path: &str,
    grant: LeaseGrant<'_>,
) -> Result<Option<String>> {
    conn.query_row(
        "
                SELECT job_id FROM leases
                WHERE worktree_path = ?1
                  AND released_at IS NULL
                  AND NOT (owner = ?2 AND repo_name = ?3 AND job_id = ?4)
                ",
        params![worktree_path, grant.owner, grant.repo_name, grant.job_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| lease_err("grant lease", e))
}

fn insert_or_refresh_writer(
    conn: &Connection,
    grant: LeaseGrant<'_>,
    now: i64,
    repo: &str,
    worktree_path: &str,
    branch_ref: &str,
) -> Result<usize> {
    match conn.execute(
        GRANT_INSERT_SQL,
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
    ) {
        Ok(changed) => Ok(changed),
        Err(err) if is_unique_constraint(&err) => {
            let occupant = live_path_occupant(conn, worktree_path, grant)?;
            Err(path_held_conflict(occupant.as_deref().unwrap_or("unknown")))
        }
        Err(err) => Err(lease_err("grant lease", err)),
    }
}

fn is_unique_constraint(err: &rusqlite::Error) -> bool {
    err.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation)
}

fn path_held_conflict(job_id: &str) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::LeaseConflict,
        message: format!("checkout already holds an active lease for job `{job_id}`"),
    }
}

fn list_leases_on(conn: &Connection, suffix: &str, context: &'static str) -> Result<Vec<Lease>> {
    let query = format!("{LEASE_SELECT} {suffix}");
    let mut stmt = conn.prepare(&query).map_err(|e| lease_err(context, e))?;
    let rows = stmt
        .query_map([], lease_from_row)
        .map_err(|e| lease_err(context, e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err(context, e))
}

fn list_agents_on(conn: &Connection) -> Result<Vec<AgentRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT agent_id, agent_type, session_id, started_at, stopped_at
             FROM agents
             ORDER BY (stopped_at IS NOT NULL), started_at, agent_id",
        )
        .map_err(|e| lease_err("list agents", e))?;
    let rows = stmt
        .query_map([], agent_from_row)
        .map_err(|e| lease_err("list agents", e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err("list agents", e))
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    let identity = lease_identity(row)?;
    let limits = lease_limits(row)?;
    let meta = lease_meta(row)?;
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
        mode_raw: identity.mode_raw,
        ttl: limits.ttl,
        heartbeat: limits.heartbeat,
        max_files: limits.max_files,
        max_churn: limits.max_churn,
        max_fix_cycles: limits.max_fix_cycles,
        fix_cycles: limits.fix_cycles,
        released_at: limits.released_at,
        row_id: meta.row_id,
        created_at: meta.created_at,
        updated_at: meta.updated_at,
    })
}

fn agent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRecord> {
    Ok(AgentRecord {
        agent_id: row.get(0)?,
        agent_type: row.get(1)?,
        session_id: row.get(2)?,
        started_at: row.get(3)?,
        stopped_at: row.get(4)?,
    })
}

struct LeaseMeta {
    row_id: i64,
    created_at: i64,
    updated_at: i64,
}

fn lease_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseMeta> {
    Ok(LeaseMeta {
        row_id: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
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
    mode_raw: String,
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
        mode: LeaseMode::parse(&refs.3),
        mode_raw: refs.3,
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

fn lease_refs(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, String)> {
    Ok((row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?))
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
    use std::sync::{Arc, Barrier};
    use std::thread;
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
        assert!(held.row_id > 0);
        assert!(held.created_at > 0);

        let released = store.release_by_path(&wt).unwrap().unwrap();
        assert_eq!(released.mode, LeaseMode::Unassigned);
        assert!(released.released_at.is_some());

        let active = store.list_active().unwrap();
        assert!(active.is_empty(), "released lease must not list as active");
        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].mode, LeaseMode::Unassigned);
        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].released_at.is_some());

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
    fn grant_refuses_active_path_held_by_different_job() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkouts/shared");
        let grant_a = LeaseGrant {
            repo: &repo,
            owner: "local",
            repo_name: "repo",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &wt,
            start_commit: "abc123",
        };
        let grant_b = LeaseGrant {
            job_id: "job-b",
            branch: "hive/job-b",
            ..grant_a
        };

        store.grant(grant_a).unwrap();
        let err = store.grant(grant_b).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                ..
            }
        ));
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, "job-a");

        store.release_by_path(&wt).unwrap();
        let moved = store.grant(grant_b).unwrap();
        assert_eq!(moved.job_id, "job-b");
        assert!(moved.released_at.is_none());

        // Sequential reuse leaves a released row and an active row on the same
        // path. Lookup must return the live holder. rusqlite `query_row` would
        // silently keep the first (older) row; ORDER BY/LIMIT 1 is the
        // preference, not a workaround for QueryReturnedMoreThanOneRow.
        let found = store.find_by_path(&wt).unwrap().unwrap();
        assert_eq!(found.job_id, "job-b");
        assert!(found.released_at.is_none());

        let released = store.release_by_path(&wt).unwrap().unwrap();
        assert_eq!(released.job_id, "job-b");
        assert!(released.released_at.is_some());
    }

    #[test]
    fn query_row_keeps_first_row_query_one_rejects_extras() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkouts/shared");
        let grant_a = LeaseGrant {
            repo: &repo,
            owner: "local",
            repo_name: "repo",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &wt,
            start_commit: "abc123",
        };
        let grant_b = LeaseGrant {
            job_id: "job-b",
            branch: "hive/job-b",
            ..grant_a
        };
        store.grant(grant_a).unwrap();
        store.release_by_path(&wt).unwrap();
        store.grant(grant_b).unwrap();

        let conn = store.conn.lock().unwrap();
        let sql = "SELECT job_id FROM leases WHERE worktree_path = ?1 ORDER BY id";
        let first: String = conn
            .query_row(sql, params![path_text(&wt)], |row| row.get(0))
            .unwrap();
        assert_eq!(first, "job-a");
        let extra = conn.query_one(sql, params![path_text(&wt)], |row| row.get::<_, String>(0));
        assert!(matches!(
            extra,
            Err(rusqlite::Error::QueryReturnedMoreThanOneRow)
        ));
    }

    #[test]
    fn select_then_insert_without_constraint_admits_two_live_owners() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("race.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE leases (
                    id INTEGER PRIMARY KEY,
                    job_id TEXT NOT NULL,
                    worktree_path TEXT NOT NULL,
                    released_at INTEGER
                );",
            )
            .unwrap();
        }
        let barrier = Arc::new(Barrier::new(2));
        let spawn = |job: &'static str| {
            let db = db.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let conn = Connection::open(db).unwrap();
                let occupant: Option<String> = conn
                    .query_row(
                        "SELECT job_id FROM leases WHERE worktree_path = 'p' AND released_at IS NULL",
                        [],
                        |row| row.get(0),
                    )
                    .optional()
                    .unwrap();
                assert!(
                    occupant.is_none(),
                    "both connections must observe an empty path"
                );
                barrier.wait();
                conn.execute(
                    "INSERT INTO leases (job_id, worktree_path, released_at) VALUES (?1, 'p', NULL)",
                    [job],
                )
                .unwrap();
            })
        };
        let a = spawn("job-a");
        let b = spawn("job-b");
        a.join().unwrap();
        b.join().unwrap();
        let conn = Connection::open(&db).unwrap();
        let live: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM leases WHERE released_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            live, 2,
            "occupant SELECT then INSERT without a live-path constraint is not atomic"
        );
    }

    #[test]
    fn concurrent_grants_admit_one_live_owner() {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("leases.db");
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkouts/shared");
        drop(LeaseStore::open(&db).unwrap());

        let start = Arc::new(Barrier::new(2));
        let spawn = |job: &'static str, branch: &'static str| {
            let db = db.clone();
            let repo = repo.clone();
            let wt = wt.clone();
            let start = start.clone();
            thread::spawn(move || {
                let store = LeaseStore::open(db).unwrap();
                start.wait();
                store.grant(LeaseGrant {
                    repo: &repo,
                    owner: "local",
                    repo_name: "repo",
                    job_id: job,
                    branch,
                    worktree_path: &wt,
                    start_commit: "abc123",
                })
            })
        };
        let a = spawn("job-a", "hive/job-a");
        let b = spawn("job-b", "hive/job-b");
        let ra = a.join().expect("job-a thread");
        let rb = b.join().expect("job-b thread");
        let wins = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
        let losses = [&ra, &rb]
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(Error::PolicyViolation {
                        code: PolicyCode::LeaseConflict,
                        ..
                    })
                )
            })
            .count();
        assert_eq!(wins, 1, "exactly one grant must become the live owner");
        assert_eq!(losses, 1, "the other grant must be a typed LEASE_CONFLICT");

        let store = LeaseStore::open(&db).unwrap();
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert!(active[0].released_at.is_none());
        assert!(active[0].job_id == "job-a" || active[0].job_id == "job-b");
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
        store
            .upsert_agent(AgentIdentity {
                agent_id: "agent-2",
                agent_type: "Worker",
                session_id: None,
            })
            .unwrap();
        store.retire_agent("agent-1").unwrap();
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].agent_id, "agent-2");
        assert!(agents[0].stopped_at.is_none());
        assert_eq!(agents[1].agent_id, "agent-1");
        assert!(agents[1].stopped_at.is_some());
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

    #[test]
    fn unrecognized_mode_is_unknown_not_unassigned() {
        let tmp = tempdir().unwrap();
        let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
        let repo = tmp.path().join("repo");
        let wt = tmp.path().join("checkouts/a");
        store
            .grant(LeaseGrant {
                repo: &repo,
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
                branch: "hive/a",
                worktree_path: &wt,
                start_commit: "abc123",
            })
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE leases SET mode = 'NOT_A_MODE'", [])
                .unwrap();
        }
        let lease = store
            .find_job(JobKey {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-a",
            })
            .unwrap()
            .unwrap();
        assert_eq!(lease.mode, LeaseMode::Unknown);
        assert_eq!(lease.mode_raw, "NOT_A_MODE");
        assert_ne!(lease.mode, LeaseMode::Unassigned);
    }
}
