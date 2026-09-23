//! Lease SQLite schema, migrations, and allocation-op journal.

use rusqlite::{Connection, OptionalExtension, params};

use super::{Error, Result, lease_err};

pub(super) const INITIAL_SCHEMA: &str = r"
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

pub(super) const GRANT_UPSERT: &str = r"
            INSERT INTO leases (
                repo, owner, repo_name, job_id, branch, branch_ref, worktree_path,
                requested_start_point, start_commit, operation_id, allocation_state,
                mode, ttl, heartbeat, max_files, max_churn, max_fix_cycles, fix_cycles,
                pending_fix_cycles, pending_fix_op_id, created_at, updated_at,
                released_at, tombstoned_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13,
                NULL, NULL, NULL, 0, NULL, NULL, ?13, ?13, NULL, NULL
            )
            ON CONFLICT(owner, repo_name, job_id) DO UPDATE SET
                repo = excluded.repo,
                branch = excluded.branch,
                branch_ref = excluded.branch_ref,
                worktree_path = excluded.worktree_path,
                start_commit = excluded.start_commit,
                requested_start_point = CASE
                    WHEN leases.released_at IS NOT NULL
                        OR leases.requested_start_point = ''
                    THEN excluded.requested_start_point
                    ELSE leases.requested_start_point
                END,
                pending_fix_cycles = CASE
                    WHEN leases.released_at IS NOT NULL THEN NULL
                    ELSE leases.pending_fix_cycles
                END,
                pending_fix_op_id = CASE
                    WHEN leases.released_at IS NOT NULL THEN NULL
                    ELSE leases.pending_fix_op_id
                END,
                operation_id = CASE
                    WHEN leases.released_at IS NOT NULL OR leases.operation_id = ''
                    THEN excluded.operation_id
                    ELSE leases.operation_id
                END,
                allocation_state = excluded.allocation_state,
                mode = excluded.mode,
                heartbeat = excluded.heartbeat,
                updated_at = excluded.updated_at,
                released_at = NULL
            WHERE leases.tombstoned_at IS NULL
                AND (
                    leases.released_at IS NOT NULL
                    OR leases.worktree_path = excluded.worktree_path
                )
            ";

pub(super) const PREPARE_INSERT: &str = r"
            INSERT INTO leases (
                repo, owner, repo_name, job_id, branch, branch_ref, worktree_path,
                requested_start_point, start_commit, operation_id, allocation_state,
                mode, ttl, heartbeat, max_files, max_churn, max_fix_cycles, fix_cycles,
                pending_fix_cycles, pending_fix_op_id, created_at, updated_at,
                released_at, tombstoned_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                NULL, NULL, NULL, 0, NULL, NULL, ?14, ?14, NULL, NULL
            )
            ";

pub(super) fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(INITIAL_SCHEMA)
        .map_err(|e| lease_err("initialize lease schema", e))?;
    ensure_crash_consistency_columns(conn)?;
    ensure_live_path_unique_index(conn)?;
    crate::coord::ensure_schema(conn)
}

pub(super) fn table_column_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(leases)")
        .map_err(|e| lease_err("inspect lease schema", e))?;
    stmt.query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| lease_err("inspect lease schema", e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err("inspect lease schema", e))
}

fn ensure_crash_consistency_columns(conn: &Connection) -> Result<()> {
    let names = table_column_names(conn)?;
    let additions = [
        (
            "requested_start_point",
            "requested_start_point TEXT NOT NULL DEFAULT ''",
        ),
        ("operation_id", "operation_id TEXT NOT NULL DEFAULT ''"),
        (
            "allocation_state",
            "allocation_state TEXT NOT NULL DEFAULT 'ACTIVE'",
        ),
        ("pending_fix_cycles", "pending_fix_cycles INTEGER"),
        ("pending_fix_op_id", "pending_fix_op_id TEXT"),
        ("tombstoned_at", "tombstoned_at INTEGER"),
    ];
    for (column, ddl) in additions {
        if !names.iter().any(|name| name == column) {
            conn.execute(&format!("ALTER TABLE leases ADD COLUMN {ddl}"), [])
                .map_err(|e| lease_err("migrate lease schema", e))?;
        }
    }
    conn.execute(
        "
        UPDATE leases
        SET requested_start_point = start_commit
        WHERE requested_start_point = ''
        ",
        [],
    )
    .map_err(|e| lease_err("backfill requested_start_point", e))?;
    conn.execute(
        "
        UPDATE leases
        SET operation_id = 'legacy-' || id
        WHERE operation_id = ''
        ",
        [],
    )
    .map_err(|e| lease_err("backfill operation_id", e))?;
    conn.execute(
        "
        UPDATE leases
        SET allocation_state = CASE
            WHEN tombstoned_at IS NOT NULL THEN 'TOMBSTONED'
            WHEN released_at IS NOT NULL THEN 'RELEASED'
            ELSE 'ACTIVE'
        END
        WHERE allocation_state = '' OR (
            allocation_state = 'ACTIVE' AND released_at IS NOT NULL
        )
        ",
        [],
    )
    .map_err(|e| lease_err("backfill allocation_state", e))?;
    Ok(())
}

pub(super) fn has_crash_consistency_columns(conn: &Connection) -> Result<bool> {
    let names = table_column_names(conn)?;
    Ok([
        "requested_start_point",
        "operation_id",
        "allocation_state",
        "pending_fix_cycles",
        "pending_fix_op_id",
        "tombstoned_at",
    ]
    .into_iter()
    .all(|column| names.iter().any(|name| name == column)))
}

fn ensure_live_path_unique_index(conn: &Connection) -> Result<()> {
    let duplicate = conn
        .query_row(
            "
            SELECT worktree_path
            FROM leases
            WHERE released_at IS NULL AND tombstoned_at IS NULL
            GROUP BY worktree_path
            HAVING COUNT(*) > 1
            ORDER BY worktree_path
            LIMIT 1
            ",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| lease_err("inspect live worktree path uniqueness", e))?;
    if let Some(path) = duplicate {
        return Err(Error::LeaseStore {
            context: "ensure live worktree path uniqueness",
            message: format!("duplicate live worktree_path `{path}`"),
        });
    }
    conn.execute(
        "
        CREATE UNIQUE INDEX IF NOT EXISTS leases_live_worktree_path
            ON leases(worktree_path)
            WHERE released_at IS NULL AND tombstoned_at IS NULL
        ",
        [],
    )
    .map_err(|e| lease_err("ensure live worktree path uniqueness", e))?;
    Ok(())
}

pub(super) struct OpRecord<'a> {
    pub operation_id: &'a str,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub kind: &'a str,
    pub phase: &'a str,
    pub requested_start_point: Option<&'a str>,
    pub resolved_start_commit: Option<&'a str>,
    pub status: &'a str,
    pub now: i64,
}

pub(super) fn insert_op(tx: &rusqlite::Transaction<'_>, op: OpRecord<'_>) -> Result<()> {
    tx.execute(
        "
        INSERT INTO allocation_ops (
            operation_id, owner, repo_name, job_id, kind, phase,
            requested_start_point, resolved_start_commit, status, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
        ",
        params![
            op.operation_id,
            op.owner,
            op.repo_name,
            op.job_id,
            op.kind,
            op.phase,
            op.requested_start_point,
            op.resolved_start_commit,
            op.status,
            op.now,
        ],
    )
    .map_err(|e| lease_err("insert allocation op", e))?;
    Ok(())
}

pub(super) struct OpPhase<'a> {
    pub operation_id: &'a str,
    pub phase: &'a str,
    pub status: &'a str,
    pub now: i64,
}

pub(super) fn update_op_phase(tx: &rusqlite::Transaction<'_>, phase: OpPhase<'_>) -> Result<()> {
    tx.execute(
        "
        UPDATE allocation_ops
        SET phase = ?1, status = ?2, updated_at = ?3
        WHERE operation_id = ?4
        ",
        params![phase.phase, phase.status, phase.now, phase.operation_id],
    )
    .map_err(|e| lease_err("update allocation op", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_legacy_leases(conn: &Connection) {
        conn.execute_batch(
            "
            CREATE TABLE leases (
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
        .unwrap();
    }

    fn insert_legacy_lease(conn: &Connection, job_id: &str, path: &str) {
        conn.execute(
            "
            INSERT INTO leases (
                repo, owner, repo_name, job_id, branch, branch_ref, worktree_path,
                start_commit, mode, fix_cycles, created_at, updated_at, released_at
            ) VALUES (
                '/repo', 'acme', 'sample', ?1, 'branch', 'refs/heads/branch', ?2,
                'abc123', 'WRITER_LOCKED', 0, 1, 1, NULL
            )
            ",
            params![job_id, path],
        )
        .unwrap();
    }

    #[test]
    fn legacy_schema_migrates_before_live_path_index_is_created() {
        let conn = Connection::open_in_memory().unwrap();
        create_legacy_leases(&conn);
        insert_legacy_lease(&conn, "job-a", "/worktrees/a");

        apply(&conn).unwrap();

        let columns = table_column_names(&conn).unwrap();
        assert!(columns.iter().any(|column| column == "tombstoned_at"));
        let index_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'leases_live_worktree_path')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(index_exists);
    }

    #[test]
    fn duplicate_live_paths_report_the_conflicting_path() {
        let conn = Connection::open_in_memory().unwrap();
        create_legacy_leases(&conn);
        insert_legacy_lease(&conn, "job-a", "/worktrees/shared");
        insert_legacy_lease(&conn, "job-b", "/worktrees/shared");

        let err = apply(&conn).unwrap_err();
        match err {
            Error::LeaseStore { context, message } => {
                assert_eq!(context, "ensure live worktree path uniqueness");
                assert!(message.contains("/worktrees/shared"));
            }
            other => panic!("expected lease-store error, got {other:?}"),
        }
    }

    #[test]
    fn read_only_open_reads_parent_schema_without_migrating() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("leases.db");
        {
            let conn = Connection::open(&path).unwrap();
            create_legacy_leases(&conn);
            insert_legacy_lease(&conn, "job-a", "/worktrees/a");
        }
        let store = crate::lease::LeaseStore::open_read_only(&path).unwrap();
        let leases = store.list_all().unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].requested_start_point, "abc123");
        assert_eq!(leases[0].start_commit, "abc123");
        assert!(leases[0].operation_id.starts_with("legacy-"));
        assert_eq!(
            leases[0].allocation_state,
            crate::lease::AllocationState::Active
        );
        assert!(!has_crash_consistency_columns(&Connection::open(&path).unwrap()).unwrap());
    }
}
