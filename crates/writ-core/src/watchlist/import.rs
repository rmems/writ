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
        apply_babysit_row(list, &incoming, &now, &mut report);
    }
    detect_stacks(list);
    Ok(report)
}

fn apply_babysit_row(
    list: &mut Watchlist,
    incoming: &PrBabysitEntry,
    now: &str,
    report: &mut AddReport,
) {
    let Some(repo) = incoming.repo.clone() else {
        return;
    };
    if owner_of_repo(&repo).is_none() {
        return;
    }
    let status = map_imported_status(incoming.last_status.as_deref());
    if let Some(existing) = list.get_mut(&repo, incoming.number) {
        refresh_existing_from_babysit(existing, incoming, status);
        report.refreshed.push((repo, incoming.number));
        return;
    }
    list.prs
        .push(new_entry_from_babysit(&repo, incoming, status, now));
    report.added.push((repo, incoming.number));
}

fn refresh_existing_from_babysit(
    existing: &mut WatchEntry,
    incoming: &PrBabysitEntry,
    status: WatchStatus,
) {
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
}

fn new_entry_from_babysit(
    repo: &str,
    incoming: &PrBabysitEntry,
    status: WatchStatus,
    now: &str,
) -> WatchEntry {
    WatchEntry {
        repo: repo.to_owned(),
        number: incoming.number,
        branch: incoming.branch.clone().unwrap_or_default(),
        status,
        last_checked: incoming
            .last_checked
            .clone()
            .unwrap_or_else(|| now.to_owned()),
        fix_count: incoming.fix_count.unwrap_or(0),
        residual_blockers: Vec::new(),
        stack_id: incoming.stack_id.clone(),
        stack_type: incoming.stack_type.clone(),
        stack_position: incoming.stack_position,
        base: incoming.base.clone(),
        title: incoming.title.clone(),
        added_at: incoming.added_at.clone().or_else(|| Some(now.to_owned())),
        check_count: incoming.check_count,
        url: incoming.url.clone(),
        kind: Some(WatchKind::PrBabysit),
        extra: serde_json::Map::new(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("writ-import-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("source.json")
    }

    #[test]
    fn import_skips_bad_repo_and_refreshes_existing() {
        let path = scratch_path("rows");
        fs::write(
            &path,
            r#"{"prs":[
              {"number":1,"repo":"notslug"},
              {"number":2,"repo":"acme/widgets","branch":"b","last_status":"failed","fix_count":3}
            ]}"#,
        )
        .unwrap();
        let mut list = Watchlist::default();
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 2,
            branch: "old".to_owned(),
            status: WatchStatus::Healthy,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 9,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: None,
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        let report = import_pr_babysit(&mut list, &path).unwrap();
        assert!(report.added.is_empty());
        assert_eq!(report.refreshed, vec![("acme/widgets".to_owned(), 2)]);
        let entry = list.get("acme/widgets", 2).unwrap();
        assert_eq!(entry.branch, "b");
        assert_eq!(entry.status, WatchStatus::Failed);
        assert_eq!(entry.fix_count, 9);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn import_missing_file_is_io_error() {
        let mut list = Watchlist::default();
        let err =
            import_pr_babysit(&mut list, std::path::Path::new("/no/such/file.json")).unwrap_err();
        assert!(matches!(err, WatchlistError::Io { .. }));
    }
}
