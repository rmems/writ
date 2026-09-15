//! Crash-consistent lease records for worktree allocation.
//!
//! Persist the requested symbolic identity and resolved canonical start commit
//! *before* git mutation. Inspect reports observed identity without adopting
//! anything. Reconcile is deterministic: promote a fully matching interrupted
//! allocation, retry only when no mutation occurred, and keep a needs-attention
//! state for partial or conflicting evidence. Released and tombstoned rows are
//! never resurrected.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::error::{Error, LeaseAttentionFailure, PolicyCode, Result};

static OPERATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Lease admission modes reserved by #124.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

/// Durable allocation phase for crash recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AllocationState {
    Prepared,
    Mutating,
    Active,
    NeedsAttention,
    Released,
    Tombstoned,
    Aborted,
}

impl AllocationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "PREPARED",
            Self::Mutating => "MUTATING",
            Self::Active => "ACTIVE",
            Self::NeedsAttention => "NEEDS_ATTENTION",
            Self::Released => "RELEASED",
            Self::Tombstoned => "TOMBSTONED",
            Self::Aborted => "ABORTED",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "PREPARED" => Self::Prepared,
            "MUTATING" => Self::Mutating,
            "ACTIVE" => Self::Active,
            "NEEDS_ATTENTION" => Self::NeedsAttention,
            "RELEASED" => Self::Released,
            "TOMBSTONED" => Self::Tombstoned,
            _ => Self::Aborted,
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Released | Self::Tombstoned)
    }

    fn is_in_progress(self) -> bool {
        matches!(self, Self::Prepared | Self::Mutating | Self::NeedsAttention)
    }
}

/// Classification of lease-vs-git evidence. Distinct from TTL expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    Matching,
    Retryable,
    NeedsAttention,
    OrphanedLease,
    MissingLease,
    Released,
    Tombstoned,
    Absent,
}

impl EvidenceClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matching => "matching",
            Self::Retryable => "retryable",
            Self::NeedsAttention => "needs_attention",
            Self::OrphanedLease => "orphaned_lease",
            Self::MissingLease => "missing_lease",
            Self::Released => "released",
            Self::Tombstoned => "tombstoned",
            Self::Absent => "absent",
        }
    }
}

/// One lease row, including crash-consistency identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub repo: String,
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub branch: String,
    pub branch_ref: String,
    pub worktree_path: String,
    pub requested_start_point: String,
    pub start_commit: String,
    pub operation_id: String,
    pub allocation_state: AllocationState,
    pub mode: LeaseMode,
    pub ttl: Option<i64>,
    pub heartbeat: Option<i64>,
    pub max_files: Option<i64>,
    pub max_churn: Option<i64>,
    pub max_fix_cycles: Option<i64>,
    pub fix_cycles: Option<i64>,
    pub pending_fix_cycles: Option<i64>,
    pub pending_fix_op_id: Option<String>,
    pub released_at: Option<i64>,
    pub tombstoned_at: Option<i64>,
}

/// Inputs required to persist an allocation before git mutation.
#[derive(Debug, Clone, Copy)]
pub struct AllocateRequest<'a> {
    pub repo: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub branch: &'a str,
    pub worktree_path: &'a Path,
    pub requested_start_point: &'a str,
    pub start_commit: &'a str,
    pub ttl: Option<i64>,
}

/// Owner/repo/job identity without the branch (unique lease row key).
#[derive(Debug, Clone, Copy)]
pub struct JobKey<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
}

/// Read-only inspection inputs. Never used to adopt or mutate git state.
#[derive(Debug, Clone, Copy)]
pub struct InspectRequest<'a> {
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub worktree_path: &'a Path,
    pub branch: Option<&'a str>,
}

/// Observed git + lease identity. Produced without mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllocationInspection {
    pub operation_id: Option<String>,
    pub allocation_state: Option<String>,
    pub requested_start_point: Option<String>,
    pub resolved_start_commit: Option<String>,
    pub derived_path: PathBuf,
    pub path_exists: bool,
    pub branch_ref: String,
    pub branch_commit: Option<String>,
    pub head_commit: Option<String>,
    pub worktree_registered: bool,
    pub repo_identity: String,
    pub lease_present: bool,
    pub ttl_expired: bool,
    pub classification: EvidenceClass,
    pub conflicts: Vec<String>,
}

/// Deterministic reconcile result. Never deletes unproven git state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Promoted {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Retry {
        operation_id: String,
        inspection: AllocationInspection,
    },
    NeedsAttention {
        lease: Lease,
        inspection: AllocationInspection,
    },
    AlreadyActive {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Released {
        lease: Lease,
        inspection: AllocationInspection,
    },
    Tombstoned {
        lease: Lease,
        inspection: AllocationInspection,
    },
}

impl ReconcileOutcome {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Promoted { .. } => "promoted",
            Self::Retry { .. } => "retry",
            Self::NeedsAttention { .. } => "needs_attention",
            Self::AlreadyActive { .. } => "already_active",
            Self::Released { .. } => "released",
            Self::Tombstoned { .. } => "tombstoned",
        }
    }
}

/// Authorization token for the one accumulated budget counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixCycleToken {
    pub operation_id: String,
    pub authorized: i64,
}

/// Crash recovery for `fix_cycles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixCycleReconcile {
    Committed { fix_cycles: i64 },
    Aborted { fix_cycles: i64 },
    NeedsAttention { pending: i64, operation_id: String },
    Idle { fix_cycles: i64 },
}

const LEASE_SELECT: &str = "SELECT repo, owner, repo_name, job_id, branch, branch_ref, \
     worktree_path, requested_start_point, start_commit, operation_id, \
     allocation_state, mode, ttl, heartbeat, max_files, max_churn, \
     max_fix_cycles, fix_cycles, pending_fix_cycles, pending_fix_op_id, \
     created_at, updated_at, released_at, tombstoned_at FROM leases";

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
        conn.execute_batch(
            "
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
            ",
        )
        .map_err(|e| lease_err("initialize lease schema", e))?;
        ensure_crash_consistency_columns(&conn)?;
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

    /// Persist requested + resolved identity before any git mutation.
    pub fn prepare_allocate(&self, request: AllocateRequest<'_>) -> Result<Lease> {
        let key = JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        };
        if let Some(existing) = self.find_job(key)? {
            return Err(prepare_blocked(existing));
        }
        let now = now_secs();
        let operation_id = new_operation_id();
        let branch_ref = format!("refs/heads/{}", request.branch);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin allocate prepare", e))?;
        tx.execute(
            "
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
            ",
            params![
                path_text(request.repo),
                request.owner,
                request.repo_name,
                request.job_id,
                request.branch,
                branch_ref,
                path_text(request.worktree_path),
                request.requested_start_point,
                request.start_commit,
                operation_id,
                AllocationState::Prepared.as_str(),
                LeaseMode::Unassigned.as_str(),
                request.ttl,
                now,
            ],
        )
        .map_err(|e| lease_err("prepare allocate", e))?;
        insert_op(
            &tx,
            OpRecord {
                operation_id: &operation_id,
                owner: request.owner,
                repo_name: request.repo_name,
                job_id: request.job_id,
                kind: "ALLOCATE",
                phase: "PREPARE",
                requested_start_point: Some(request.requested_start_point),
                resolved_start_commit: Some(request.start_commit),
                status: "IN_PROGRESS",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit allocate prepare", e))?;
        drop(conn);
        self.find_job(key)?.ok_or_else(|| Error::LeaseStore {
            context: "prepare allocate",
            message: "lease row missing after prepare".to_owned(),
        })
    }

    /// Record that git mutation is authorized for this operation.
    pub fn mark_mutating(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Mutating, "MUTATE")
    }

    /// Promote a matching allocation after git mutation succeeded.
    pub fn commit_allocate(&self, operation_id: &str) -> Result<Lease> {
        self.advance_allocate(operation_id, AllocationState::Active, "COMMIT")
    }

    fn advance_allocate(
        &self,
        operation_id: &str,
        next: AllocationState,
        phase: &'static str,
    ) -> Result<Lease> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin allocate advance", e))?;
        let current = query_lease_tx(
            &tx,
            "WHERE operation_id = ?1",
            params![operation_id],
            "lookup allocate operation",
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "advance allocate",
            message: format!("unknown operation {operation_id}"),
        })?;
        if current.allocation_state.is_terminal() {
            return Err(terminal_error(&current));
        }
        let mode = if next == AllocationState::Active {
            LeaseMode::WriterLocked.as_str()
        } else {
            current.mode.as_str()
        };
        let status = if next == AllocationState::Active {
            "COMMITTED"
        } else {
            "IN_PROGRESS"
        };
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND tombstoned_at IS NULL AND released_at IS NULL
            ",
            params![next.as_str(), mode, now, operation_id],
        )
        .map_err(|e| lease_err("advance allocate", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "advance allocate",
                message: "released or tombstoned lease cannot be advanced".to_owned(),
            });
        }
        update_op_phase(&tx, operation_id, phase, status, now)?;
        tx.commit()
            .map_err(|e| lease_err("commit allocate advance", e))?;
        drop(conn);
        self.find_by_operation(operation_id)?
            .ok_or_else(|| Error::LeaseStore {
                context: "advance allocate",
                message: "lease row missing after advance".to_owned(),
            })
    }

    /// Release a writer lock by worktree path without deleting identity.
    pub fn release_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Released)
    }

    /// Permanently tombstone a lease so reconcile cannot revive it.
    pub fn tombstone_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.finalize_by_path(worktree_path, AllocationState::Tombstoned)
    }

    fn finalize_by_path(
        &self,
        worktree_path: &Path,
        state: AllocationState,
    ) -> Result<Option<Lease>> {
        let now = now_secs();
        let path = path_text(worktree_path);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin lease finalize", e))?;
        let existing = query_lease_tx(
            &tx,
            "WHERE worktree_path = ?1",
            params![path],
            "lookup lease by path",
        )?;
        let Some(lease) = existing else {
            tx.commit()
                .map_err(|e| lease_err("commit lease finalize", e))?;
            return Ok(None);
        };
        if lease.allocation_state.is_terminal() {
            tx.commit()
                .map_err(|e| lease_err("commit lease finalize", e))?;
            drop(conn);
            return self.find_by_path(worktree_path);
        }
        let (released_at, tombstoned_at, kind) = if state == AllocationState::Tombstoned {
            (lease.released_at, Some(now), "TOMBSTONE")
        } else {
            (Some(now), None, "RELEASE")
        };
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, released_at = ?3, tombstoned_at = ?4,
                updated_at = ?5
            WHERE worktree_path = ?6 AND tombstoned_at IS NULL
            ",
            params![
                state.as_str(),
                LeaseMode::Unassigned.as_str(),
                released_at,
                tombstoned_at,
                now,
                path,
            ],
        )
        .map_err(|e| lease_err("finalize lease", e))?;
        insert_op(
            &tx,
            OpRecord {
                operation_id: &format!("{}-{}", lease.operation_id, kind.to_ascii_lowercase()),
                owner: &lease.owner,
                repo_name: &lease.repo_name,
                job_id: &lease.job_id,
                kind,
                phase: "COMMIT",
                requested_start_point: Some(&lease.requested_start_point),
                resolved_start_commit: Some(&lease.start_commit),
                status: "COMMITTED",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit lease finalize", e))?;
        drop(conn);
        self.find_by_path(worktree_path)
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
    pub fn find_by_path(&self, worktree_path: &Path) -> Result<Option<Lease>> {
        self.query_lease(
            "WHERE worktree_path = ?1",
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
        query_lease_locked(&conn, where_sql, sql_params, context)
    }

    /// Report derived path, refs, HEAD, registration, and operation identity.
    ///
    /// This never mutates git state, never adopts a worktree, and never writes
    /// the lease store.
    pub fn inspect(&self, request: InspectRequest<'_>) -> Result<AllocationInspection> {
        let lease = self.find_job(JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        })?;
        Ok(inspect_now(&lease, request))
    }

    /// Reconcile an interrupted allocation without destructive cleanup.
    pub fn reconcile(&self, key: JobKey<'_>, repo_root: &Path) -> Result<Option<ReconcileOutcome>> {
        let Some(lease) = self.find_job(key)? else {
            return Ok(None);
        };
        let request = InspectRequest {
            repo_root,
            owner: key.owner,
            repo_name: key.repo_name,
            job_id: key.job_id,
            worktree_path: Path::new(&lease.worktree_path),
            branch: Some(&lease.branch),
        };
        let inspection = inspect_now(&Some(lease.clone()), request);
        if lease.allocation_state == AllocationState::Tombstoned {
            return Ok(Some(ReconcileOutcome::Tombstoned { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Released {
            return Ok(Some(ReconcileOutcome::Released { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Active {
            return Ok(Some(ReconcileOutcome::AlreadyActive { lease, inspection }));
        }
        if lease.allocation_state == AllocationState::Aborted {
            return Ok(Some(ReconcileOutcome::Retry {
                operation_id: lease.operation_id,
                inspection,
            }));
        }

        match inspection.classification {
            EvidenceClass::Matching => Ok(Some(self.promote_if_still_open(&lease, inspection)?)),
            EvidenceClass::Retryable => Ok(Some(self.abort_if_still_open(&lease, inspection)?)),
            _ => Ok(Some(self.attention_if_still_open(&lease, inspection)?)),
        }
    }

    fn promote_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin promote", e))?;
        let current = query_lease_tx(
            &tx,
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![expected.owner, expected.repo_name, expected.job_id],
            "re-read lease before promote",
        )?;
        let Some(current) = current else {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(ReconcileOutcome::Retry {
                operation_id: expected.operation_id.clone(),
                inspection,
            });
        };
        if current.allocation_state.is_terminal() {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        if current.allocation_state == AllocationState::Active {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(ReconcileOutcome::AlreadyActive {
                lease: current,
                inspection,
            });
        }
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, heartbeat = ?3, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![
                AllocationState::Active.as_str(),
                LeaseMode::WriterLocked.as_str(),
                now,
                expected.operation_id,
            ],
        )
        .map_err(|e| lease_err("promote lease", e))?;
        if tx.changes() != 1 {
            tx.commit().map_err(|e| lease_err("commit promote", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        update_op_phase(&tx, &expected.operation_id, "COMMIT", "COMMITTED", now)?;
        tx.commit().map_err(|e| lease_err("commit promote", e))?;
        drop(conn);
        let lease = self
            .find_by_operation(&expected.operation_id)?
            .ok_or_else(|| Error::LeaseStore {
                context: "promote lease",
                message: "lease row missing after promote".to_owned(),
            })?;
        Ok(ReconcileOutcome::Promoted { lease, inspection })
    }

    fn abort_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin abort", e))?;
        let current = query_lease_tx(
            &tx,
            "WHERE operation_id = ?1",
            params![expected.operation_id],
            "re-read lease before abort",
        )?;
        let Some(current) = current else {
            tx.commit().map_err(|e| lease_err("commit abort", e))?;
            return Ok(ReconcileOutcome::Retry {
                operation_id: expected.operation_id.clone(),
                inspection,
            });
        };
        if current.allocation_state.is_terminal() {
            tx.commit().map_err(|e| lease_err("commit abort", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        tx.execute(
            "
            DELETE FROM leases
            WHERE operation_id = ?1
              AND allocation_state IN ('PREPARED', 'MUTATING', 'ABORTED')
              AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![expected.operation_id],
        )
        .map_err(|e| lease_err("abort reservation", e))?;
        if tx.changes() != 1 {
            tx.commit().map_err(|e| lease_err("commit abort", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        update_op_phase(&tx, &expected.operation_id, "PREPARE", "ABORTED", now)?;
        tx.commit().map_err(|e| lease_err("commit abort", e))?;
        Ok(ReconcileOutcome::Retry {
            operation_id: expected.operation_id.clone(),
            inspection,
        })
    }

    fn attention_if_still_open(
        &self,
        expected: &Lease,
        inspection: AllocationInspection,
    ) -> Result<ReconcileOutcome> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin attention", e))?;
        let current = query_lease_tx(
            &tx,
            "WHERE operation_id = ?1",
            params![expected.operation_id],
            "re-read lease before attention",
        )?;
        let Some(current) = current else {
            tx.commit().map_err(|e| lease_err("commit attention", e))?;
            return Ok(ReconcileOutcome::Retry {
                operation_id: expected.operation_id.clone(),
                inspection,
            });
        };
        if current.allocation_state.is_terminal() {
            tx.commit().map_err(|e| lease_err("commit attention", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        tx.execute(
            "
            UPDATE leases
            SET allocation_state = ?1, mode = ?2, updated_at = ?3
            WHERE operation_id = ?4 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![
                AllocationState::NeedsAttention.as_str(),
                LeaseMode::NeedsHuman.as_str(),
                now,
                expected.operation_id,
            ],
        )
        .map_err(|e| lease_err("mark needs attention", e))?;
        if tx.changes() != 1 {
            tx.commit().map_err(|e| lease_err("commit attention", e))?;
            return Ok(terminal_outcome(current, inspection));
        }
        update_op_phase(
            &tx,
            &expected.operation_id,
            "MUTATE",
            "NEEDS_ATTENTION",
            now,
        )?;
        tx.commit().map_err(|e| lease_err("commit attention", e))?;
        drop(conn);
        let lease = self
            .find_by_operation(&expected.operation_id)?
            .ok_or_else(|| Error::LeaseStore {
                context: "mark needs attention",
                message: "lease row missing after attention".to_owned(),
            })?;
        Ok(ReconcileOutcome::NeedsAttention { lease, inspection })
    }

    /// Persist intent to increment `fix_cycles` before the authorized mutation.
    pub fn prepare_fix_cycle(&self, key: JobKey<'_>) -> Result<FixCycleToken> {
        let now = now_secs();
        let operation_id = new_operation_id();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle prepare", e))?;
        let lease = query_lease_tx(
            &tx,
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![key.owner, key.repo_name, key.job_id],
            "lookup lease for fix-cycle",
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "prepare fix-cycle",
            message: "lease not found".to_owned(),
        })?;
        if lease.allocation_state != AllocationState::Active {
            return Err(Error::LeaseStore {
                context: "prepare fix-cycle",
                message: format!(
                    "fix_cycles increment requires ACTIVE lease, found {}",
                    lease.allocation_state.as_str()
                ),
            });
        }
        if lease.pending_fix_op_id.is_some() {
            return Err(Error::LeaseStore {
                context: "prepare fix-cycle",
                message: "a pending fix_cycles increment already exists; reconcile it first"
                    .to_owned(),
            });
        }
        let authorized = lease.fix_cycles.unwrap_or(0) + 1;
        tx.execute(
            "
            UPDATE leases
            SET pending_fix_cycles = ?1, pending_fix_op_id = ?2, updated_at = ?3
            WHERE owner = ?4 AND repo_name = ?5 AND job_id = ?6
              AND allocation_state = 'ACTIVE' AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![
                authorized,
                operation_id,
                now,
                key.owner,
                key.repo_name,
                key.job_id
            ],
        )
        .map_err(|e| lease_err("prepare fix-cycle", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "prepare fix-cycle",
                message: "released or tombstoned lease cannot increment fix_cycles".to_owned(),
            });
        }
        insert_op(
            &tx,
            OpRecord {
                operation_id: &operation_id,
                owner: key.owner,
                repo_name: key.repo_name,
                job_id: key.job_id,
                kind: "INCREMENT_FIX_CYCLE",
                phase: "PREPARE",
                requested_start_point: Some(&lease.requested_start_point),
                resolved_start_commit: Some(&lease.start_commit),
                status: "IN_PROGRESS",
                now,
            },
        )?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle prepare", e))?;
        Ok(FixCycleToken {
            operation_id,
            authorized,
        })
    }

    /// Record that the mutation authorized by a pending increment is in flight.
    pub fn mark_fix_cycle_mutating(&self, operation_id: &str) -> Result<()> {
        self.set_fix_cycle_phase(operation_id, "MUTATE")
    }

    /// Commit a pending increment after the authorized mutation.
    pub fn commit_fix_cycle(&self, operation_id: &str) -> Result<i64> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle commit", e))?;
        let lease = query_lease_tx(
            &tx,
            "WHERE pending_fix_op_id = ?1",
            params![operation_id],
            "lookup pending fix-cycle",
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "commit fix-cycle",
            message: format!("unknown pending fix-cycle {operation_id}"),
        })?;
        if lease.allocation_state.is_terminal() {
            return Err(terminal_error(&lease));
        }
        let committed = lease
            .pending_fix_cycles
            .unwrap_or(lease.fix_cycles.unwrap_or(0));
        tx.execute(
            "
            UPDATE leases
            SET fix_cycles = ?1, pending_fix_cycles = NULL, pending_fix_op_id = NULL,
                updated_at = ?2
            WHERE pending_fix_op_id = ?3 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![committed, now, operation_id],
        )
        .map_err(|e| lease_err("commit fix-cycle", e))?;
        if tx.changes() != 1 {
            return Err(Error::LeaseStore {
                context: "commit fix-cycle",
                message: "released or tombstoned lease cannot commit fix_cycles".to_owned(),
            });
        }
        update_op_phase(&tx, operation_id, "COMMIT", "COMMITTED", now)?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle commit", e))?;
        Ok(committed)
    }

    fn set_fix_cycle_phase(&self, operation_id: &str, phase: &'static str) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle phase", e))?;
        let lease = query_lease_tx(
            &tx,
            "WHERE pending_fix_op_id = ?1",
            params![operation_id],
            "lookup pending fix-cycle",
        )?
        .ok_or_else(|| Error::LeaseStore {
            context: "advance fix-cycle",
            message: format!("unknown pending fix-cycle {operation_id}"),
        })?;
        if lease.allocation_state.is_terminal() {
            return Err(terminal_error(&lease));
        }
        update_op_phase(&tx, operation_id, phase, "IN_PROGRESS", now)?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle phase", e))?;
        Ok(())
    }

    /// Recover a pending `fix_cycles` increment without double-counting or losing it.
    pub fn reconcile_fix_cycle(
        &self,
        key: JobKey<'_>,
        mutation_proven: Option<bool>,
    ) -> Result<FixCycleReconcile> {
        let lease = self.find_job(key)?.ok_or_else(|| Error::LeaseStore {
            context: "reconcile fix-cycle",
            message: "lease not found".to_owned(),
        })?;
        let Some(operation_id) = lease.pending_fix_op_id.clone() else {
            return Ok(FixCycleReconcile::Idle {
                fix_cycles: lease.fix_cycles.unwrap_or(0),
            });
        };
        let phase = self.op_phase(&operation_id)?;
        match mutation_proven {
            Some(true) => {
                let fix_cycles = self.commit_fix_cycle(&operation_id)?;
                Ok(FixCycleReconcile::Committed { fix_cycles })
            }
            Some(false) => {
                self.abort_fix_cycle(&operation_id)?;
                Ok(FixCycleReconcile::Aborted {
                    fix_cycles: lease.fix_cycles.unwrap_or(0),
                })
            }
            None if phase.as_deref() == Some("PREPARE") => {
                self.abort_fix_cycle(&operation_id)?;
                Ok(FixCycleReconcile::Aborted {
                    fix_cycles: lease.fix_cycles.unwrap_or(0),
                })
            }
            None => Ok(FixCycleReconcile::NeedsAttention {
                pending: lease.pending_fix_cycles.unwrap_or(0),
                operation_id,
            }),
        }
    }

    fn abort_fix_cycle(&self, operation_id: &str) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin fix-cycle abort", e))?;
        tx.execute(
            "
            UPDATE leases
            SET pending_fix_cycles = NULL, pending_fix_op_id = NULL, updated_at = ?1
            WHERE pending_fix_op_id = ?2 AND released_at IS NULL AND tombstoned_at IS NULL
            ",
            params![now, operation_id],
        )
        .map_err(|e| lease_err("abort fix-cycle", e))?;
        update_op_phase(&tx, operation_id, "PREPARE", "ABORTED", now)?;
        tx.commit()
            .map_err(|e| lease_err("commit fix-cycle abort", e))?;
        Ok(())
    }

    fn op_phase(&self, operation_id: &str) -> Result<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT phase FROM allocation_ops WHERE operation_id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| lease_err("lookup operation phase", e))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::LeaseStore {
            context: "lease store",
            message: "lease store mutex poisoned".to_owned(),
        })
    }
}

fn ensure_crash_consistency_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(leases)")
        .map_err(|e| lease_err("inspect lease schema", e))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| lease_err("inspect lease schema", e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| lease_err("inspect lease schema", e))?;
    let required = [
        "requested_start_point",
        "operation_id",
        "allocation_state",
        "pending_fix_cycles",
        "pending_fix_op_id",
        "tombstoned_at",
    ];
    for column in required {
        if !names.iter().any(|name| name == column) {
            return Err(Error::LeaseStore {
                context: "inspect lease schema",
                message: format!("leases table missing crash-consistency column {column}"),
            });
        }
    }
    Ok(())
}

struct OpRecord<'a> {
    operation_id: &'a str,
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
    kind: &'a str,
    phase: &'a str,
    requested_start_point: Option<&'a str>,
    resolved_start_commit: Option<&'a str>,
    status: &'a str,
    now: i64,
}

fn insert_op(tx: &rusqlite::Transaction<'_>, op: OpRecord<'_>) -> Result<()> {
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

fn update_op_phase(
    tx: &rusqlite::Transaction<'_>,
    operation_id: &str,
    phase: &str,
    status: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "
        UPDATE allocation_ops
        SET phase = ?1, status = ?2, updated_at = ?3
        WHERE operation_id = ?4
        ",
        params![phase, status, now, operation_id],
    )
    .map_err(|e| lease_err("update allocation op", e))?;
    Ok(())
}

fn query_lease_locked(
    conn: &Connection,
    where_sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<Option<Lease>> {
    let query = format!("{LEASE_SELECT} {where_sql}");
    conn.query_row(&query, sql_params, lease_from_row)
        .optional()
        .map_err(|e| lease_err(context, e))
}

fn query_lease_tx(
    tx: &rusqlite::Transaction<'_>,
    where_sql: &str,
    sql_params: impl rusqlite::Params,
    context: &'static str,
) -> Result<Option<Lease>> {
    let query = format!("{LEASE_SELECT} {where_sql}");
    tx.query_row(&query, sql_params, lease_from_row)
        .optional()
        .map_err(|e| lease_err(context, e))
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
        requested_start_point: row.get(7)?,
        start_commit: row.get(8)?,
        operation_id: row.get(9)?,
        allocation_state: AllocationState::parse(&row.get::<_, String>(10)?),
        mode: LeaseMode::parse(&row.get::<_, String>(11)?),
        ttl: row.get(12)?,
        heartbeat: row.get(13)?,
        max_files: row.get(14)?,
        max_churn: row.get(15)?,
        max_fix_cycles: row.get(16)?,
        fix_cycles: row.get(17)?,
        pending_fix_cycles: row.get(18)?,
        pending_fix_op_id: row.get(19)?,
        released_at: row.get(22)?,
        tombstoned_at: row.get(23)?,
    })
}

fn inspect_now(lease: &Option<Lease>, request: InspectRequest<'_>) -> AllocationInspection {
    let branch = lease
        .as_ref()
        .map(|row| row.branch.as_str())
        .or(request.branch)
        .unwrap_or("");
    let branch_ref = if branch.is_empty() {
        String::new()
    } else {
        format!("refs/heads/{branch}")
    };
    let evidence = inspect_git(request.repo_root, request.worktree_path, branch);
    let ttl_expired = lease.as_ref().is_some_and(ttl_is_expired);
    let (classification, conflicts) =
        classify(lease, &evidence, request.worktree_path, ttl_expired);
    AllocationInspection {
        operation_id: lease.as_ref().map(|row| row.operation_id.clone()),
        allocation_state: lease
            .as_ref()
            .map(|row| row.allocation_state.as_str().to_owned()),
        requested_start_point: lease.as_ref().map(|row| row.requested_start_point.clone()),
        resolved_start_commit: lease.as_ref().map(|row| row.start_commit.clone()),
        derived_path: request.worktree_path.to_path_buf(),
        path_exists: evidence.path_exists,
        branch_ref: if evidence.branch_ref.is_empty() {
            branch_ref
        } else {
            evidence.branch_ref
        },
        branch_commit: evidence.branch_commit,
        head_commit: evidence.head_commit,
        worktree_registered: evidence.worktree_registered,
        repo_identity: path_text(request.repo_root),
        lease_present: lease.is_some(),
        ttl_expired,
        classification,
        conflicts,
    }
}

struct GitEvidence {
    path_exists: bool,
    branch_ref: String,
    branch_commit: Option<String>,
    head_commit: Option<String>,
    worktree_registered: bool,
}

fn inspect_git(repo_root: &Path, worktree_path: &Path, branch: &str) -> GitEvidence {
    let branch_ref = if branch.is_empty() {
        String::new()
    } else {
        format!("refs/heads/{branch}")
    };
    let branch_commit = if branch_ref.is_empty() {
        None
    } else {
        optional_git_stdout(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{branch_ref}^{{commit}}"),
            ],
        )
    };
    let head_commit = optional_git_stdout(
        worktree_path,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
    );
    let worktree_registered = optional_git_stdout(repo_root, &["worktree", "list", "--porcelain"])
        .is_some_and(|listing| {
            listing.lines().any(|line| {
                line.strip_prefix("worktree ")
                    .is_some_and(|path| Path::new(path) == worktree_path)
            })
        });
    GitEvidence {
        path_exists: worktree_path.exists(),
        branch_ref,
        branch_commit,
        head_commit,
        worktree_registered,
    }
}

fn classify(
    lease: &Option<Lease>,
    evidence: &GitEvidence,
    _path: &Path,
    ttl_expired: bool,
) -> (EvidenceClass, Vec<String>) {
    let Some(lease) = lease else {
        if evidence.path_exists || evidence.branch_commit.is_some() || evidence.worktree_registered
        {
            return (
                EvidenceClass::MissingLease,
                vec!["live worktree or branch with no lease row".to_owned()],
            );
        }
        return (EvidenceClass::Absent, Vec::new());
    };
    if lease.allocation_state == AllocationState::Tombstoned {
        return (EvidenceClass::Tombstoned, Vec::new());
    }
    if lease.allocation_state == AllocationState::Released {
        return (EvidenceClass::Released, Vec::new());
    }
    if lease.allocation_state == AllocationState::Active && ttl_expired && evidence.path_exists {
        return (
            EvidenceClass::OrphanedLease,
            vec!["lease heartbeat/ttl expired while worktree is still live".to_owned()],
        );
    }

    let no_mutation = !evidence.path_exists
        && evidence.branch_commit.is_none()
        && evidence.head_commit.is_none()
        && !evidence.worktree_registered;
    if no_mutation && lease.allocation_state.is_in_progress() {
        return (EvidenceClass::Retryable, Vec::new());
    }

    let mut conflicts = Vec::new();
    if !evidence.path_exists {
        conflicts.push("derived path is missing".to_owned());
    }
    if !evidence.worktree_registered {
        conflicts.push("worktree is not registered".to_owned());
    }
    match evidence.branch_commit.as_deref() {
        Some(commit) if commit == lease.start_commit => {}
        Some(commit) => conflicts.push(format!(
            "branch commit {commit} != resolved start {}",
            lease.start_commit
        )),
        None => conflicts.push("branch commit is absent".to_owned()),
    }
    match evidence.head_commit.as_deref() {
        Some(commit) if commit == lease.start_commit => {}
        Some(commit) => conflicts.push(format!(
            "worker HEAD {commit} != resolved start {}",
            lease.start_commit
        )),
        None => conflicts.push("worker HEAD is absent".to_owned()),
    }
    if conflicts.is_empty() {
        (EvidenceClass::Matching, conflicts)
    } else {
        (EvidenceClass::NeedsAttention, conflicts)
    }
}

fn ttl_is_expired(lease: &Lease) -> bool {
    let (Some(ttl), Some(heartbeat)) = (lease.ttl, lease.heartbeat) else {
        return false;
    };
    now_secs().saturating_sub(heartbeat) > ttl
}

fn prepare_blocked(existing: Lease) -> Error {
    match existing.allocation_state {
        AllocationState::Released => Error::PolicyViolation {
            code: PolicyCode::LeaseReleased,
            message: format!(
                "refusing to resurrect released lease for {}/{}/{}",
                existing.owner, existing.repo_name, existing.job_id
            ),
        },
        AllocationState::Tombstoned => Error::PolicyViolation {
            code: PolicyCode::LeaseTombstoned,
            message: format!(
                "refusing to resurrect tombstoned lease for {}/{}/{}",
                existing.owner, existing.repo_name, existing.job_id
            ),
        },
        _ => Error::LeaseStore {
            context: "prepare allocate",
            message: format!(
                "lease already exists in state {} (operation {})",
                existing.allocation_state.as_str(),
                existing.operation_id
            ),
        },
    }
}

fn terminal_error(lease: &Lease) -> Error {
    let code = if lease.allocation_state == AllocationState::Tombstoned {
        PolicyCode::LeaseTombstoned
    } else {
        PolicyCode::LeaseReleased
    };
    Error::PolicyViolation {
        code,
        message: format!(
            "refusing to resurrect {} lease {}",
            lease.allocation_state.as_str(),
            lease.operation_id
        ),
    }
}

fn terminal_outcome(lease: Lease, inspection: AllocationInspection) -> ReconcileOutcome {
    if lease.allocation_state == AllocationState::Tombstoned {
        ReconcileOutcome::Tombstoned { lease, inspection }
    } else {
        ReconcileOutcome::Released { lease, inspection }
    }
}

/// Convert a needs-attention reconcile result into the fail-closed error type.
#[must_use]
pub fn attention_error(lease: &Lease, inspection: &AllocationInspection) -> Error {
    Error::LeaseAttention(Box::new(LeaseAttentionFailure {
        operation_id: lease.operation_id.clone(),
        allocation_state: lease.allocation_state.as_str().to_owned(),
        classification: inspection.classification.as_str().to_owned(),
        conflicts: inspection.conflicts.clone(),
        path: inspection.derived_path.clone(),
        path_exists: inspection.path_exists,
        branch_commit: inspection.branch_commit.clone(),
        head_commit: inspection.head_commit.clone(),
        worktree_registered: inspection.worktree_registered,
    }))
}

fn optional_git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
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

fn new_operation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{nanos}-{}-{}",
        std::process::id(),
        OPERATION_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    struct RepoHarness {
        _temp: tempfile::TempDir,
        repo: PathBuf,
        store: LeaseStore,
        worktree: PathBuf,
        start: String,
    }

    impl RepoHarness {
        fn new() -> Self {
            let temp = tempdir().unwrap();
            let repo = temp.path().join("repo");
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "user.name", "Test User"]);
            git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
            let start = git(&repo, &["rev-parse", "HEAD"]);
            let store = LeaseStore::open(temp.path().join("leases.db")).unwrap();
            let worktree = temp.path().join("worktrees/acme/sample/job-1");
            Self {
                _temp: temp,
                repo,
                store,
                worktree,
                start,
            }
        }

        fn request(&self) -> AllocateRequest<'_> {
            AllocateRequest {
                repo: &self.repo,
                owner: "acme",
                repo_name: "sample",
                job_id: "job-1",
                branch: "hive/job-1",
                worktree_path: &self.worktree,
                requested_start_point: "refs/heads/main",
                start_commit: &self.start,
                ttl: None,
            }
        }

        fn key(&self) -> JobKey<'_> {
            JobKey {
                owner: "acme",
                repo_name: "sample",
                job_id: "job-1",
            }
        }

        fn inspect_req(&self) -> InspectRequest<'_> {
            InspectRequest {
                repo_root: &self.repo,
                owner: "acme",
                repo_name: "sample",
                job_id: "job-1",
                worktree_path: &self.worktree,
                branch: Some("hive/job-1"),
            }
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn later_commit(repo: &Path) -> String {
        git(repo, &["commit", "--allow-empty", "-m", "later"]);
        git(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn crash_before_mutation_is_retryable_without_reserving_identity() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        assert_eq!(prepared.allocation_state, AllocationState::Prepared);
        assert_eq!(prepared.requested_start_point, "refs/heads/main");
        assert_eq!(prepared.start_commit, harness.start);
        assert_ne!(prepared.requested_start_point, prepared.start_commit);

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "retry");
        assert!(harness.store.find_job(harness.key()).unwrap().is_none());

        let again = harness.store.prepare_allocate(harness.request()).unwrap();
        assert_ne!(again.operation_id, prepared.operation_id);
    }

    #[test]
    fn crash_after_branch_creation_is_fail_closed_without_cleanup() {
        let harness = RepoHarness::new();
        harness.store.prepare_allocate(harness.request()).unwrap();
        harness
            .store
            .mark_mutating(
                &harness
                    .store
                    .find_job(harness.key())
                    .unwrap()
                    .unwrap()
                    .operation_id,
            )
            .unwrap();
        git(&harness.repo, &["branch", "hive/job-1", &harness.start]);

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "needs_attention");
        assert_eq!(
            git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
            harness.start
        );
        assert!(!harness.worktree.exists());
        let lease = harness.store.find_job(harness.key()).unwrap().unwrap();
        assert_eq!(lease.allocation_state, AllocationState::NeedsAttention);
    }

    #[test]
    fn crash_after_filesystem_creation_is_fail_closed_without_cleanup() {
        let harness = RepoHarness::new();
        harness.store.prepare_allocate(harness.request()).unwrap();
        fs::create_dir_all(&harness.worktree).unwrap();
        fs::write(harness.worktree.join("occupied"), "partial\n").unwrap();

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "needs_attention");
        assert_eq!(
            fs::read_to_string(harness.worktree.join("occupied")).unwrap(),
            "partial\n"
        );
    }

    #[test]
    fn crash_after_registration_is_fail_closed_when_head_is_absent() {
        let harness = RepoHarness::new();
        harness.store.prepare_allocate(harness.request()).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "needs_attention");
        assert!(harness.worktree.exists());
        let listing = git(&harness.repo, &["worktree", "list", "--porcelain"]);
        assert!(listing.contains(&harness.worktree.to_string_lossy().to_string()));
    }

    #[test]
    fn matching_interrupted_allocation_is_promoted_with_canonical_commit() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        let ReconcileOutcome::Promoted { lease, inspection } = outcome else {
            panic!("expected promote, got {outcome:?}");
        };
        assert_eq!(lease.start_commit, harness.start);
        assert_eq!(lease.requested_start_point, "refs/heads/main");
        assert_eq!(lease.allocation_state, AllocationState::Active);
        assert_eq!(lease.mode, LeaseMode::WriterLocked);
        assert_eq!(
            inspection.head_commit.as_deref(),
            Some(harness.start.as_str())
        );
        assert_eq!(inspection.classification, EvidenceClass::Matching);
        assert!(!inspection.ttl_expired);
    }

    #[test]
    fn conflicting_head_stays_fail_closed() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        let moved = later_commit(&harness.repo);
        git(
            &harness.repo,
            &["update-ref", "refs/heads/hive/job-1", &moved],
        );

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "needs_attention");
        assert_eq!(
            git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
            moved
        );
        assert!(harness.worktree.exists());
    }

    #[test]
    fn persisted_record_distinguishes_symbolic_resolved_and_operation() {
        let harness = RepoHarness::new();
        let lease = harness.store.prepare_allocate(harness.request()).unwrap();
        let inspection = harness.store.inspect(harness.inspect_req()).unwrap();
        assert_eq!(
            inspection.requested_start_point.as_deref(),
            Some("refs/heads/main")
        );
        assert_eq!(
            inspection.resolved_start_commit.as_deref(),
            Some(harness.start.as_str())
        );
        assert_eq!(
            inspection.operation_id.as_deref(),
            Some(lease.operation_id.as_str())
        );
        assert_ne!(
            inspection.requested_start_point.as_deref(),
            inspection.resolved_start_commit.as_deref()
        );
    }

    #[test]
    fn concurrent_reconcile_cannot_resurrect_tombstoned_lease() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        let other = LeaseStore::open(harness.store.path()).unwrap();
        other.tombstone_by_path(&harness.worktree).unwrap();

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "tombstoned");
        let lease = harness.store.find_job(harness.key()).unwrap().unwrap();
        assert_eq!(lease.allocation_state, AllocationState::Tombstoned);
        assert!(matches!(
            harness.store.prepare_allocate(harness.request()),
            Err(Error::PolicyViolation {
                code: PolicyCode::LeaseTombstoned,
                ..
            })
        ));
    }

    #[test]
    fn concurrent_reconcile_cannot_resurrect_released_lease() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        harness
            .store
            .commit_allocate(&prepared.operation_id)
            .unwrap();
        let other = LeaseStore::open(harness.store.path()).unwrap();
        other.release_by_path(&harness.worktree).unwrap();

        let outcome = harness
            .store
            .reconcile(harness.key(), &harness.repo)
            .unwrap()
            .unwrap();
        assert_eq!(outcome.as_str(), "released");
        assert!(matches!(
            harness.store.prepare_allocate(harness.request()),
            Err(Error::PolicyViolation {
                code: PolicyCode::LeaseReleased,
                ..
            })
        ));
    }

    #[test]
    fn ttl_expiry_and_missing_lease_are_distinct() {
        let harness = RepoHarness::new();
        let mut request = harness.request();
        request.ttl = Some(1);
        let prepared = harness.store.prepare_allocate(request).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        harness
            .store
            .commit_allocate(&prepared.operation_id)
            .unwrap();
        {
            let conn = harness.store.conn.lock().unwrap();
            conn.execute(
                "UPDATE leases SET heartbeat = 1, ttl = 1 WHERE job_id = 'job-1'",
                [],
            )
            .unwrap();
        }
        let orphaned = harness.store.inspect(harness.inspect_req()).unwrap();
        assert_eq!(orphaned.classification, EvidenceClass::OrphanedLease);
        assert!(orphaned.lease_present);
        assert!(orphaned.ttl_expired);
        assert!(orphaned.path_exists);

        let missing_store =
            LeaseStore::open(harness.store.path().with_file_name("other.db")).unwrap();
        let missing = missing_store.inspect(harness.inspect_req()).unwrap();
        assert_eq!(missing.classification, EvidenceClass::MissingLease);
        assert!(!missing.lease_present);
        assert!(missing.path_exists);
        assert!(!missing.ttl_expired);
    }

    #[test]
    fn inspect_does_not_mutate_or_adopt() {
        let harness = RepoHarness::new();
        harness.store.prepare_allocate(harness.request()).unwrap();
        git(&harness.repo, &["branch", "hive/job-1", &harness.start]);
        let before = git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]);
        let _ = harness.store.inspect(harness.inspect_req()).unwrap();
        assert_eq!(
            git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
            before
        );
        assert!(!harness.worktree.exists());
        assert_eq!(
            harness
                .store
                .find_job(harness.key())
                .unwrap()
                .unwrap()
                .allocation_state,
            AllocationState::Prepared
        );
    }

    #[test]
    fn fix_cycles_crash_before_mutation_does_not_count() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        harness
            .store
            .commit_allocate(&prepared.operation_id)
            .unwrap();

        let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
        assert_eq!(token.authorized, 1);
        let recovered = harness
            .store
            .reconcile_fix_cycle(harness.key(), None)
            .unwrap();
        assert_eq!(recovered, FixCycleReconcile::Aborted { fix_cycles: 0 });
        assert_eq!(
            harness
                .store
                .find_job(harness.key())
                .unwrap()
                .unwrap()
                .fix_cycles,
            Some(0)
        );
    }

    #[test]
    fn fix_cycles_crash_after_proven_mutation_is_not_lost() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        harness
            .store
            .commit_allocate(&prepared.operation_id)
            .unwrap();

        let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
        harness
            .store
            .mark_fix_cycle_mutating(&token.operation_id)
            .unwrap();
        let recovered = harness
            .store
            .reconcile_fix_cycle(harness.key(), Some(true))
            .unwrap();
        assert_eq!(recovered, FixCycleReconcile::Committed { fix_cycles: 1 });
        let again = harness
            .store
            .reconcile_fix_cycle(harness.key(), Some(true))
            .unwrap();
        assert_eq!(again, FixCycleReconcile::Idle { fix_cycles: 1 });
    }

    #[test]
    fn fix_cycles_mutate_without_proof_stays_needs_attention() {
        let harness = RepoHarness::new();
        let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
        harness.store.mark_mutating(&prepared.operation_id).unwrap();
        git(
            &harness.repo,
            &[
                "worktree",
                "add",
                "-b",
                "hive/job-1",
                "--",
                harness.worktree.to_str().unwrap(),
                &harness.start,
            ],
        );
        harness
            .store
            .commit_allocate(&prepared.operation_id)
            .unwrap();
        let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
        harness
            .store
            .mark_fix_cycle_mutating(&token.operation_id)
            .unwrap();
        let recovered = harness
            .store
            .reconcile_fix_cycle(harness.key(), None)
            .unwrap();
        assert!(matches!(
            recovered,
            FixCycleReconcile::NeedsAttention { pending: 1, .. }
        ));
        assert_eq!(
            harness
                .store
                .find_job(harness.key())
                .unwrap()
                .unwrap()
                .fix_cycles,
            Some(0)
        );
    }
}
