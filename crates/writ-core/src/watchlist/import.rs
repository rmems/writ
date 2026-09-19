//! Read-only import from a pr-babysit `watched-prs.json`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::WatchlistError;
use super::ops::AddReport;
use super::schema::{WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo};
use super::stack::detect_stacks;
use super::store::{mutate_watchlist, utc_now_rfc3339};

/// Read-only import from a pr-babysit `watched-prs.json`. The source file is
/// never written. Identity is still `(repo, number)`; existing hive history
/// (`fix_count`) is preserved.
pub fn import_pr_babysit(
    list: &mut Watchlist,
    source_path: &Path,
) -> Result<AddReport, WatchlistError> {
    let data = fs::read_to_string(source_path).map_err(|err| WatchlistError::Io {
        context: "read pr-babysit state",
        path: source_path.to_path_buf(),
        source: err,
    })?;
    let parsed: PrBabysitFile = serde_json::from_str(&data).map_err(|err| WatchlistError::Gh {
        repo: source_path.display().to_string(),
        number: 0,
        message: format!("failed to parse pr-babysit JSON: {err}"),
    })?;
    let now = utc_now_rfc3339();
    let mut report = AddReport {
        added: Vec::new(),
        refreshed: Vec::new(),
        skipped: Vec::new(),
    };
    for incoming in parsed.prs {
        let Some(repo) = incoming.repo.clone() else {
            continue;
        };
        // Reject malformed identities: a repo that is not `owner/name` would
        // break owner filtering and later refreshes, so never persist it.
        if owner_of_repo(&repo).is_none() {
            continue;
        }
        let status = map_imported_status(incoming.last_status.as_deref());
        if let Some(existing) = list.get_mut(&repo, incoming.number) {
            // Refresh source fields from the incoming record but preserve the
            // hive budget (`fix_count`) already accrued locally.
            if let Some(branch) = incoming.branch.clone() {
                existing.branch = branch;
            }
            existing.base = incoming.base.clone().or_else(|| existing.base.clone());
            existing.title = incoming.title.clone().or_else(|| existing.title.clone());
            existing.url = incoming.url.clone().or_else(|| existing.url.clone());
            existing.stack_id = incoming
                .stack_id
                .clone()
                .or_else(|| existing.stack_id.clone());
            existing.stack_type = incoming
                .stack_type
                .clone()
                .or_else(|| existing.stack_type.clone());
            existing.stack_position = incoming.stack_position.or(existing.stack_position);
            if let Some(added_at) = incoming.added_at.clone() {
                existing.added_at = Some(added_at);
            }
            existing.check_count = incoming.check_count.or(existing.check_count);
            existing.status = status;
            report.refreshed.push((repo, incoming.number));
            continue;
        }
        list.prs.push(WatchEntry {
            repo: repo.clone(),
            number: incoming.number,
            branch: incoming.branch.unwrap_or_default(),
            status,
            last_checked: incoming.last_checked.clone().unwrap_or_else(|| now.clone()),
            fix_count: incoming.fix_count.unwrap_or(0),
            residual_blockers: Vec::new(),
            stack_id: incoming.stack_id.clone(),
            stack_type: incoming.stack_type.clone(),
            stack_position: incoming.stack_position,
            base: incoming.base.clone(),
            title: incoming.title.clone(),
            added_at: incoming.added_at.clone().or_else(|| Some(now.clone())),
            check_count: incoming.check_count,
            url: incoming.url.clone(),
            kind: Some(WatchKind::PrBabysit),
            extra: serde_json::Map::new(),
        });
        report.added.push((repo, incoming.number));
    }
    detect_stacks(list);
    Ok(report)
}

/// Persist [`import_pr_babysit`] against `path`.
pub fn import_pr_babysit_at(path: &Path, source_path: &Path) -> Result<AddReport, WatchlistError> {
    mutate_watchlist(path, |list| import_pr_babysit(list, source_path))
}

/// Default pr-babysit path under the user data directory (read-only).
#[must_use]
pub fn default_pr_babysit_path() -> std::path::PathBuf {
    crate::paths::user_data_dir()
        .join("pr-babysit")
        .join("watched-prs.json")
}

#[derive(Debug, Deserialize)]
struct PrBabysitFile {
    #[serde(default)]
    prs: Vec<PrBabysitEntry>,
}

#[derive(Debug, Deserialize)]
struct PrBabysitEntry {
    number: u64,
    repo: Option<String>,
    branch: Option<String>,
    stack_id: Option<String>,
    stack_type: Option<String>,
    stack_position: Option<u32>,
    base: Option<String>,
    title: Option<String>,
    added_at: Option<String>,
    last_checked: Option<String>,
    last_status: Option<String>,
    check_count: Option<u32>,
    fix_count: Option<u32>,
    url: Option<String>,
}

fn map_imported_status(last_status: Option<&str>) -> WatchStatus {
    match last_status.map(str::to_ascii_lowercase).as_deref() {
        Some("healthy" | "success" | "pass") => WatchStatus::Healthy,
        Some("failed" | "fail" | "failure") => WatchStatus::Failed,
        Some("residual") => WatchStatus::Residual,
        Some("conflict") => WatchStatus::Conflict,
        Some("timeout") => WatchStatus::Timeout,
        _ => WatchStatus::Pending,
    }
}
