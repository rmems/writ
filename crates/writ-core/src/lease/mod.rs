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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::error::{Error, LeaseAttentionFailure, PolicyCode, Result};

mod classify;
mod fix_cycle;
mod occupant;
mod query;
mod recover;
mod schema;
mod types;

pub use types::*;

use query::{LIVE_PATH_LOOKUP, list_agents_on, list_leases_on, query_lease_locked, query_lease_tx};

static OPERATION_SEQ: AtomicU64 = AtomicU64::new(0);

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

    /// Grant or refresh a writer lock for an already-created checkout.
    ///
    /// Completes in one SQLite transaction as `ACTIVE`. Crash-consistency for
    /// interrupted registration uses [`Self::prepare_allocate`] instead. An
    /// active lease held by a different checkout path is never seized. A
    /// different job cannot take a checkout path that already has a live
    /// (unreleased, untombstoned) lease. Either conflict fails with
    /// [`PolicyCode::LeaseConflict`].
    pub fn grant(&self, grant: LeaseGrant<'_>) -> Result<Lease> {
        let now = now_secs();
        let repo = path_text(grant.repo);
        let worktree_path = path_text(grant.worktree_path);
        let branch_ref = format!("refs/heads/{}", grant.branch);
        let operation_id = new_operation_id();
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin grant", e))?;
        if let Some(existing) = query_lease_tx(
            &tx,
            "WHERE owner = ?1 AND repo_name = ?2 AND job_id = ?3",
            params![grant.owner, grant.repo_name, grant.job_id],
            "lookup lease before grant",
        )? && existing.allocation_state == AllocationState::Tombstoned
        {
            return Err(terminal_error(&existing));
        }
        occupant::reject_live_path_occupant(
            &tx,
            occupant::LivePathClaim {
                worktree_path: &worktree_path,
                owner: grant.owner,
                repo_name: grant.repo_name,
                job_id: grant.job_id,
            },
            "grant lease",
        )?;
        let changed = tx
            .execute(
                schema::GRANT_UPSERT,
                params![
                    repo,
                    grant.owner,
                    grant.repo_name,
                    grant.job_id,
                    grant.branch,
                    branch_ref,
                    worktree_path,
                    grant.start_commit,
                    operation_id,
                    AllocationState::Active.as_str(),
                    LeaseMode::WriterLocked.as_str(),
                    now,
                ],
            )
            .map_err(|e| occupant::map_live_path_constraint(e, "grant lease"))?;
        if changed == 0 {
            return Err(Error::PolicyViolation {
                code: PolicyCode::LeaseConflict,
                message: format!(
                    "job `{}` already holds an active lease for a different worktree path",
                    grant.job_id
                ),
            });
        }
        tx.commit().map_err(|e| lease_err("commit grant", e))?;
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
        let names = schema::table_column_names(&conn)?;
        Ok(["max_files", "max_churn", "max_fix_cycles", "fix_cycles"]
            .into_iter()
            .all(|col| names.iter().any(|name| name == col)))
    }

    /// Persist requested + resolved identity before the ownership mutation.
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
        let worktree_path = path_text(request.worktree_path);
        let mut conn = self.lock()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| lease_err("begin allocate prepare", e))?;
        occupant::reject_live_path_occupant(
            &tx,
            occupant::LivePathClaim {
                worktree_path: &worktree_path,
                owner: request.owner,
                repo_name: request.repo_name,
                job_id: request.job_id,
            },
            "prepare allocate",
        )?;
        insert_prepared_row(
            &tx,
            PreparedInsert {
                request,
                branch_ref: &branch_ref,
                worktree_path: &worktree_path,
                operation_id: &operation_id,
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
        schema::update_op_phase(
            &tx,
            schema::OpPhase {
                operation_id,
                phase,
                status,
                now,
            },
        )?;
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
        let existing =
            query_lease_tx(&tx, LIVE_PATH_LOOKUP, params![path], "lookup lease by path")?;
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
            WHERE id = ?6 AND tombstoned_at IS NULL
            ",
            params![
                state.as_str(),
                LeaseMode::Unassigned.as_str(),
                released_at,
                tombstoned_at,
                now,
                lease.row_id,
            ],
        )
        .map_err(|e| lease_err("finalize lease", e))?;
        schema::insert_op(
            &tx,
            schema::OpRecord {
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
        Ok(classify::inspect_now(&lease, request))
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
        let inspection = classify::inspect_now(&Some(lease.clone()), request);
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

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::LeaseStore {
            context: "lease store",
            message: "lease store mutex poisoned".to_owned(),
        })
    }
}

struct PreparedInsert<'a> {
    request: AllocateRequest<'a>,
    branch_ref: &'a str,
    worktree_path: &'a str,
    operation_id: &'a str,
    now: i64,
}

fn insert_prepared_row(tx: &rusqlite::Transaction<'_>, row: PreparedInsert<'_>) -> Result<()> {
    let request = row.request;
    tx.execute(
        schema::PREPARE_INSERT,
        params![
            path_text(request.repo),
            request.owner,
            request.repo_name,
            request.job_id,
            request.branch,
            row.branch_ref,
            row.worktree_path,
            request.requested_start_point,
            request.start_commit,
            row.operation_id,
            AllocationState::Prepared.as_str(),
            LeaseMode::Unassigned.as_str(),
            request.ttl,
            row.now,
        ],
    )
    .map_err(|e| occupant::map_live_path_constraint(e, "prepare allocate"))?;
    schema::insert_op(
        tx,
        schema::OpRecord {
            operation_id: row.operation_id,
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
            kind: "ALLOCATE",
            phase: "PREPARE",
            requested_start_point: Some(request.requested_start_point),
            resolved_start_commit: Some(request.start_commit),
            status: "IN_PROGRESS",
            now: row.now,
        },
    )
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

pub(super) fn terminal_error(lease: &Lease) -> Error {
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

pub(super) fn terminal_outcome(lease: Lease, inspection: AllocationInspection) -> ReconcileOutcome {
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

pub(super) fn lease_err(context: &'static str, err: rusqlite::Error) -> Error {
    Error::LeaseStore {
        context,
        message: err.to_string(),
    }
}

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(super) fn new_operation_id() -> String {
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

pub(super) fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests;
