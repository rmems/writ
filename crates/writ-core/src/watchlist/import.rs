//! Read-only import from a pr-babysit `watched-prs.json`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::WatchlistError;
use super::ops::AddReport;
use super::schema::{WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo};
use super::stack::{detect_stacks, rebuild_groups};
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
    let parsed: PrBabysitFile = serde_json::from_str(&data).map_err(|err| {
        WatchlistError::InvalidInput(format!(
            "failed to parse pr-babysit JSON at {}: {err}",
            source_path.display()
        ))
    })?;
    let annotations: Vec<_> = parsed
        .prs
        .iter()
        .filter_map(imported_stack_annotation)
        .collect();
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
    restore_partial_stack_annotations(list, &annotations);
    rebuild_groups(list);
    Ok(report)
}

type StackAnnotation = (String, u64, Option<String>, Option<String>, Option<u32>);

fn imported_stack_annotation(incoming: &PrBabysitEntry) -> Option<StackAnnotation> {
    let repo = incoming.repo.clone()?;
    owner_of_repo(&repo)?;
    Some((
        repo,
        incoming.number,
        incoming.stack_id.clone(),
        incoming.stack_type.clone(),
        incoming.stack_position,
    ))
}

fn restore_partial_stack_annotations(list: &mut Watchlist, annotations: &[StackAnnotation]) {
    for (repo, number, stack_id, stack_type, stack_position) in annotations {
        let Some(entry) = list.get_mut(repo, *number) else {
            continue;
        };
        if entry.stack_id.is_none() && stack_id.is_some() {
            entry.stack_id.clone_from(stack_id);
            entry.stack_type.clone_from(stack_type);
            entry.stack_position = *stack_position;
        }
    }
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
    if paths_resolve_to_same_file(path, source_path) {
        return Err(WatchlistError::InvalidInput(
            "pr-babysit source must differ from the watchlist destination".to_owned(),
        ));
    }
    mutate_watchlist(path, |list| import_pr_babysit(list, source_path))
}

fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right || metadata_identifies_same_file(&left, &right),
        _ => left == right,
    }
}

#[cfg(unix)]
fn metadata_identifies_same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
        _ => false,
    }
}

#[cfg(windows)]
fn metadata_identifies_same_file(left: &Path, right: &Path) -> bool {
    let (Ok(left), Ok(right)) = (fs::File::open(left), fs::File::open(right)) else {
        return false;
    };
    if left.try_lock().is_err() {
        return false;
    }
    let same = match right.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
        Ok(()) => {
            let _ = right.unlock();
            false
        }
    };
    let _ = left.unlock();
    same
}

#[cfg(not(any(unix, windows)))]
fn metadata_identifies_same_file(_left: &Path, _right: &Path) -> bool {
    false
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

    #[test]
    fn malformed_import_is_invalid_input_not_github_failure() {
        let path = scratch_path("malformed");
        fs::write(&path, "not json").unwrap();
        let mut list = Watchlist::default();

        let err = import_pr_babysit(&mut list, &path).unwrap_err();

        assert!(matches!(&err, WatchlistError::InvalidInput(_)));
        assert_eq!(err.code(), "INVALID_INPUT");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn import_rejects_destination_as_source() {
        let path = scratch_path("same-source-destination");
        fs::write(&path, r#"{"version":1,"prs":[],"groups":{}}"#).unwrap();

        let err = import_pr_babysit_at(&path, &path).unwrap_err();

        assert!(matches!(err, WatchlistError::InvalidInput(_)));
        assert!(path.exists());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn import_rejects_hard_link_alias_of_destination() {
        let path = scratch_path("hard-link-destination");
        let alias = path.with_file_name("source-alias.json");
        fs::write(&path, r#"{"version":1,"prs":[],"groups":{}}"#).unwrap();
        fs::hard_link(&path, &alias).unwrap();

        let err = import_pr_babysit_at(&path, &alias).unwrap_err();

        assert!(matches!(err, WatchlistError::InvalidInput(_)));
        assert!(path.exists());
        let _ = fs::remove_file(alias);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn partial_import_preserves_source_stack_annotations() {
        let path = scratch_path("partial-stack");
        fs::write(
            &path,
            r#"{"prs":[{"number":2,"repo":"acme/widgets","branch":"feat/top","base":"feat/base","stack_id":"stack-a","stack_type":"feature","stack_position":1}]}"#,
        )
        .unwrap();
        let mut list = Watchlist::default();

        import_pr_babysit(&mut list, &path).unwrap();

        let entry = list.get("acme/widgets", 2).unwrap();
        assert_eq!(entry.stack_id.as_deref(), Some("stack-a"));
        assert_eq!(entry.stack_type.as_deref(), Some("feature"));
        assert_eq!(entry.stack_position, Some(1));
        assert_eq!(list.groups["stack-a"].numbers, vec![2]);
        let _ = fs::remove_file(path);
    }
}
