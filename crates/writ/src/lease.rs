//! `writ lease` CLI: inspect and reconcile harness-owned checkout leases.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;

use crate::write_json_line;

#[derive(Debug, Subcommand)]
pub(crate) enum LeaseAction {
    /// Report git and lease identity without mutating or adopting anything.
    Inspect {
        /// Repository root used to inspect branch, HEAD, and registration.
        #[arg(long)]
        repo: PathBuf,
        /// GitHub-style owner segment.
        owner: String,
        /// Repository name segment.
        repo_name: String,
        /// Job id segment (e.g. gh-42).
        job_id: String,
        /// Branch name used when no lease row exists.
        #[arg(long)]
        branch: Option<String>,
        /// Checkout path when no lease row exists (harness-owned location).
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Reconcile an interrupted registration without destructive cleanup.
    Reconcile {
        /// Repository root used to inspect branch, HEAD, and registration.
        #[arg(long)]
        repo: PathBuf,
        /// GitHub-style owner segment.
        owner: String,
        /// Repository name segment.
        repo_name: String,
        /// Job id segment (e.g. gh-42).
        job_id: String,
    },
}

pub(crate) struct LeaseCli<'a> {
    pub allowlist: &'a writ_core::owners::OwnerAllowlist,
    pub json: bool,
    pub stdout: &'a mut dyn Write,
}

pub(crate) fn run(action: LeaseAction, ctx: LeaseCli<'_>) -> writ_core::error::Result<ExitCode> {
    let response = lease_response(action, ctx.allowlist)?;
    if ctx.json {
        write_json_line(ctx.stdout, &response)?;
    } else {
        writeln!(
            ctx.stdout,
            "ok={} command={}",
            response.ok, response.command
        )?;
        writeln!(
            ctx.stdout,
            "{}",
            serde_json::to_string_pretty(&response.data).map_err(std::io::Error::other)?
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn lease_response(
    action: LeaseAction,
    allowlist: &writ_core::owners::OwnerAllowlist,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    match action {
        LeaseAction::Inspect {
            repo,
            owner,
            repo_name,
            job_id,
            branch,
            path,
        } => inspect_lease(InspectLease {
            allowlist,
            repo: &repo,
            owner: &owner,
            repo_name: &repo_name,
            job_id: &job_id,
            branch: branch.as_deref(),
            path: path.as_deref(),
        }),
        LeaseAction::Reconcile {
            repo,
            owner,
            repo_name,
            job_id,
        } => reconcile_lease(ReconcileLease {
            allowlist,
            repo: &repo,
            owner: &owner,
            repo_name: &repo_name,
            job_id: &job_id,
        }),
    }
}

struct InspectLease<'a> {
    allowlist: &'a writ_core::owners::OwnerAllowlist,
    repo: &'a Path,
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
    branch: Option<&'a str>,
    path: Option<&'a Path>,
}

fn inspect_lease(
    request: InspectLease<'_>,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::contract::Response;
    use writ_core::lease::InspectRequest;

    request.allowlist.enforce_owner(request.owner)?;
    let store = crate::store::open_existing_read_only_store()?;
    let worktree_path = inspect_worktree_path(InspectPath {
        store: store.as_ref(),
        owner: request.owner,
        repo_name: request.repo_name,
        job_id: request.job_id,
        path: request.path,
    })?;
    let inspection_req = InspectRequest {
        repo_root: request.repo,
        owner: request.owner,
        repo_name: request.repo_name,
        job_id: request.job_id,
        worktree_path: &worktree_path,
        branch: request.branch,
    };
    let inspection = match store.as_ref() {
        Some(store) => store.inspect(inspection_req)?,
        None => writ_core::lease::LeaseStore::inspect_without_store(inspection_req),
    };
    Ok(Response::success(
        "lease.inspect",
        serde_json::to_value(&inspection).map_err(std::io::Error::other)?,
    ))
}

struct ReconcileLease<'a> {
    allowlist: &'a writ_core::owners::OwnerAllowlist,
    repo: &'a Path,
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
}

fn reconcile_lease(
    request: ReconcileLease<'_>,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::contract::Response;
    use writ_core::lease::{JobKey, LeaseStore};
    use writ_core::paths::lease_store_path;

    request.allowlist.enforce_owner(request.owner)?;
    let store = LeaseStore::open(lease_store_path())?;
    let key = JobKey {
        owner: request.owner,
        repo_name: request.repo_name,
        job_id: request.job_id,
    };
    match store.reconcile(key, request.repo)? {
        None => Ok(Response::success(
            "lease.reconcile",
            serde_json::json!({ "outcome": "absent" }),
        )),
        Some(outcome) => {
            if let writ_core::lease::ReconcileOutcome::NeedsAttention { lease, inspection } =
                &outcome
            {
                return Err(writ_core::lease::attention_error(lease, inspection));
            }
            Ok(Response::success(
                "lease.reconcile",
                serde_json::json!({ "outcome": outcome.as_str() }),
            ))
        }
    }
}

struct InspectPath<'a> {
    store: Option<&'a writ_core::lease::LeaseStore>,
    owner: &'a str,
    repo_name: &'a str,
    job_id: &'a str,
    path: Option<&'a Path>,
}

fn inspect_worktree_path(request: InspectPath<'_>) -> writ_core::error::Result<std::path::PathBuf> {
    use writ_core::lease::JobKey;
    use writ_core::paths::{derive_worktree_path, worktree_base_path};

    if let Some(store) = request.store
        && let Some(lease) = store.find_job(JobKey {
            owner: request.owner,
            repo_name: request.repo_name,
            job_id: request.job_id,
        })?
    {
        return Ok(std::path::PathBuf::from(lease.worktree_path));
    }
    if let Some(path) = request.path {
        return Ok(path.to_path_buf());
    }
    derive_worktree_path(
        &worktree_base_path()?,
        request.owner,
        request.repo_name,
        request.job_id,
    )
}
