//! Lease store open path and crash-consistency schema.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::{Error, Result};

use super::LeaseStore;
use super::util::lease_err;

const SCHEMA_SQL: &str = "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            CREATE TABLE IF NOT EXISTS leases (
                id INTEGER PRIMARY KEY,
                repo TEXT NOT NULL,
                owner TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                job_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                branch_ref TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                requested_start_point TEXT NOT NULL,
                start_commit TEXT NOT NULL,
                operation_id TEXT NOT NULL,
                allocation_state TEXT NOT NULL,
                mode TEXT NOT NULL,
                ttl INTEGER,
                heartbeat INTEGER,
                max_files INTEGER,
                max_churn INTEGER,
                max_fix_cycles INTEGER,
                fix_cycles INTEGER,
                pending_fix_cycles INTEGER,
                pending_fix_op_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                released_at INTEGER,
                tombstoned_at INTEGER,
                UNIQUE(owner, repo_name, job_id)
            );
            CREATE TABLE IF NOT EXISTS allocation_ops (
                operation_id TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                job_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                phase TEXT NOT NULL,
                requested_start_point TEXT,
                resolved_start_commit TEXT,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agents (
                agent_id TEXT PRIMARY KEY,
                agent_type TEXT NOT NULL,
                session_id TEXT,
                started_at INTEGER NOT NULL,
                stopped_at INTEGER
            );
            ";

const REQUIRED_COLUMNS: [&str; 6] = [
    "requested_start_point",
    "operation_id",
    "allocation_state",
    "pending_fix_cycles",
    "pending_fix_op_id",
    "tombstoned_at",
];

impl LeaseStore {
    /// Open (or create) the store at `path` and apply the crash-consistency schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        ensure_store_parent(&path)?;
        let conn = Connection::open(&path).map_err(|e| lease_err("open lease store", e))?;
        apply_schema(&conn)?;
        ensure_crash_consistency_columns(&conn)?;
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }
}

fn ensure_store_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::Io {
            context: "create lease store directory",
            source: e,
        })?;
    }
    Ok(())
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|e| lease_err("initialize lease schema", e))?;
    Ok(())
}

fn ensure_crash_consistency_columns(conn: &Connection) -> Result<()> {
    let names = lease_column_names(conn)?;
    for column in REQUIRED_COLUMNS {
        if !names.iter().any(|name| name == column) {
            return Err(Error::LeaseStore {
                context: "inspect lease schema",
                message: format!("leases table missing crash-consistency column {column}"),
            });
        }
    }
    Ok(())
}

fn lease_column_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(leases)")
        .map_err(|e| lease_err("inspect lease schema", e))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| lease_err("inspect lease schema", e))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err("inspect lease schema", e))
}
