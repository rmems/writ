//! Crash-consistent lease records for harness-owned checkout registration.
//!
//! Persist the requested symbolic identity and resolved canonical start commit
//! *before* the ownership mutation (lease grant / `writ worktree register`).
//! Writ does not create or delete checkouts; the harness (or plain git) owns
//! that lifecycle. Inspect reports observed identity without adopting anything.
//! Reconcile is deterministic: promote a fully matching interrupted
//! registration, retry only when no ownership mutation occurred, and keep a
//! needs-attention state for partial or conflicting evidence. Released and
//! tombstoned rows are never resurrected by reconcile.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use rusqlite::{Connection, OpenFlags, params};

use crate::error::{Error, PolicyCode, Result};

mod agents;
mod allocate;
mod classify;
mod fix_cycle;
mod grant;
mod occupant;
mod query;
mod recover;
mod schema;
mod types;
mod util;

pub use types::*;
pub use util::attention_error;
pub(super) use util::*;

use query::{LIVE_PATH_LOOKUP, LeaseLookup, list_leases_on, lookup_lease};

pub(super) static OPERATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// SQLite-backed lease store with a durable allocation journal.
pub struct LeaseStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for LeaseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LeaseStore {
    /// Open (or create) the store at `path` and apply the crash-consistency schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                context: "create lease store directory",
                source: e,
            })?;
        }
        let conn = Connection::open(&path).map_err(|e| lease_err("open lease store", e))?;
        schema::apply(&conn)?;
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

    /// Durable resume identity for an owner/repo/job/branch, including released rows.
    pub fn find_resume(&self, key: ResumeKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3 AND branch = ?4",
            params![key.owner, key.repo_name, key.job_id, key.branch],
            "lookup resume identity",
        )
    }

    /// All currently held (active, unreleased) leases, in insertion order.
    pub fn list_active(&self) -> Result<Vec<Lease>> {
        let conn = self.lock()?;
        list_leases_on(
            &conn,
            "WHERE released_at IS NULL AND tombstoned_at IS NULL \
             AND allocation_state = 'ACTIVE' ORDER BY id",
            "list active leases",
        )
    }

    /// Unreleased, non-tombstoned leases, including interrupted allocation states.
    pub fn list_live(&self) -> Result<Vec<Lease>> {
        let conn = self.lock()?;
        list_leases_on(
            &conn,
            "WHERE released_at IS NULL AND tombstoned_at IS NULL ORDER BY id",
            "list live leases",
        )
    }

    /// All lease identity rows, including released ones, in insertion order.
    pub fn list_all(&self) -> Result<Vec<Lease>> {
        let conn = self.lock()?;
        list_leases_on(&conn, "ORDER BY id", "list leases")
    }

    /// True when the `leases` table has the reserved nullable budget columns.
    pub fn has_budget_columns(&self) -> Result<bool> {
        let conn = self.lock()?;
        let names = schema::table_column_names(&conn)?;
        Ok(["max_files", "max_churn", "max_fix_cycles", "fix_cycles"]
            .into_iter()
            .all(|col| names.iter().any(|name| name == col)))
    }

    /// Look up a lease by owner/repo/job, including released and tombstoned rows.
    pub fn find_job(&self, key: JobKey<'_>) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![key.owner, key.repo_name, key.job_id],
            "lookup lease",
        )
    }

    /// Look up a lease by worktree path.
    ///
    /// Prefers the live (unreleased, untombstoned) row when one exists. After
    /// sequential jobs have reused a checkout, released identity rows may share
    /// the path; `query_row` would fail on that ambiguity, so this returns the
    /// active holder or else the most recently released row.
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            LIVE_PATH_LOOKUP,
            params![path_text(worktree_path)],
            "lookup lease by path",
        )
    }

    /// Look up a lease by operation identity.
    pub fn find_by_operation(&self, operation_id: &str) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE operation_id = ?1",
            params![operation_id],
            "lookup lease by operation",
        )
    }

    fn query_lease(
        &self,
        where_sql: &str,
        sql_params: impl rusqlite::Params,
        context: &'static str,
    ) -> Result<Option<Lease>> {
        let conn = self.lock()?;
        lookup_lease(
            &conn,
            LeaseLookup {
                where_sql,
                sql_params,
                context,
            },
        )
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::LeaseStore {
            context: "lease store",
            message: "lease store mutex poisoned".to_owned(),
        })
    }
}

/// Look up a job row on an already-open connection (including a write txn).
pub(crate) fn lookup_job(conn: &Connection, key: JobKey<'_>) -> Result<Option<Lease>> {
    lookup_lease(
        conn,
        LeaseLookup {
            where_sql: "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            sql_params: params![key.owner, key.repo_name, key.job_id],
            context: "lookup lease",
        },
    )
}

#[cfg(test)]
mod tests;
