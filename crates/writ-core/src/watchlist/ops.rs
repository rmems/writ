//! Watchlist mutations: add, remove, check, check-all, optional pr-babysit import.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::WatchlistError;
use super::schema::{StackGroup, WatchEntry, WatchKind, WatchStatus, Watchlist, owner_of_repo};
use super::store::{
    load_allowed_owners, load_watchlist, mutate_watchlist, owner_is_allowed, utc_now_rfc3339,
};
use crate::git_safe::SafeGhCommand;

/// Snapshot of a pull request used to insert or refresh a watchlist entry.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PrSnapshot {
    /// `owner/name`.
    pub repo: String,
    /// Pull-request number.
    pub number: u64,
    /// Head branch.
    pub branch: String,
    /// Base branch.
    pub base: String,
    /// Title.
    pub title: String,
    /// HTML URL.
    pub url: String,
    /// GitHub `state`: `OPEN`, `MERGED`, or `CLOSED`.
    pub state: String,
    /// GitHub `mergeable` (`MERGEABLE`, `CONFLICTING`, `UNKNOWN`).
    pub mergeable: Option<String>,
    /// GitHub `reviewDecision`.
    pub review_decision: Option<String>,
    /// Rollup checks.
    pub checks: Vec<CheckSnapshot>,
}

/// One CI/status rollup row.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CheckSnapshot {
    /// Check name or context.
    pub name: String,
    /// Combined state/conclusion (`SUCCESS`, `FAILURE`, `PENDING`, …).
    pub state: String,
}

/// Source of PR metadata. Production uses `gh pr view` via [`GhPrProbe`].
pub trait PrProbe {
    /// Load current GitHub metadata for one pull request.
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError>;
}

/// Probe that runs allowlisted `gh pr view --json …`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhPrProbe;

impl PrProbe for GhPrProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
        let args = [
            "pr".to_owned(),
            "view".to_owned(),
            number.to_string(),
            "--repo".to_owned(),
            repo.to_owned(),
            "--json".to_owned(),
            "number,title,url,state,headRefName,baseRefName,mergeable,reviewDecision,statusCheckRollup"
                .to_owned(),
        ];
        let cmd = SafeGhCommand::new(&args)?;
        let output = cmd.run()?;
        if output.exit_code != 0 {
            let stderr = output.stderr.trim();
            if looks_like_timeout(stderr) {
                return Err(WatchlistError::Timeout {
                    repo: repo.to_owned(),
                    number,
                    message: stderr.to_owned(),
                });
            }
            return Err(WatchlistError::Gh {
                repo: repo.to_owned(),
                number,
                message: if stderr.is_empty() {
                    format!("gh pr view exited {}", output.exit_code)
                } else {
                    stderr.to_owned()
                },
            });
        }
        parse_pr_view(repo, &output.stdout)
    }
}

fn looks_like_timeout(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("timeout") || lower.contains("timed out")
}

#[derive(Debug, Deserialize)]
struct GhPrView {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    mergeable: Option<String>,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<GhCheck>>,
}

#[derive(Debug, Deserialize)]
struct GhCheck {
    name: Option<String>,
    context: Option<String>,
    state: Option<String>,
    conclusion: Option<String>,
    status: Option<String>,
}

fn parse_pr_view(repo: &str, stdout: &str) -> Result<PrSnapshot, WatchlistError> {
    let view: GhPrView = serde_json::from_str(stdout).map_err(|err| WatchlistError::Gh {
        repo: repo.to_owned(),
        number: 0,
        message: format!("failed to parse gh pr view JSON: {err}"),
    })?;
    let checks = view
        .status_check_rollup
        .unwrap_or_default()
        .into_iter()
        .map(|check| {
            let name = check
                .name
                .or(check.context)
                .unwrap_or_else(|| "unnamed".to_owned());
            let state = check
                .conclusion
                .or(check.state)
                .or(check.status)
                .unwrap_or_else(|| "UNKNOWN".to_owned());
            CheckSnapshot { name, state }
        })
        .collect();
    Ok(PrSnapshot {
        repo: repo.to_owned(),
        number: view.number,
        branch: view.head_ref_name,
        base: view.base_ref_name,
        title: view.title,
        url: view.url,
        state: view.state,
        mergeable: view.mergeable,
        review_decision: view.review_decision,
        checks,
    })
}

/// Result of adding one or more pull requests.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AddReport {
    /// Newly inserted identities.
    pub added: Vec<(String, u64)>,
    /// Existing identities whose metadata was refreshed.
    pub refreshed: Vec<(String, u64)>,
    /// Skipped MERGED/CLOSED identities.
    pub skipped: Vec<(String, u64, String)>,
}

/// Result of one check cycle.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CheckReport {
    /// Entries still on the watchlist after the cycle, in visit order.
    pub checked: Vec<WatchEntry>,
    /// MERGED/CLOSED identities that were pruned.
    pub pruned: Vec<(String, u64, String)>,
}

/// Add `numbers` in `repo`, resolving metadata through `probe`.
///
/// `add` refreshes branch/base/title but preserves `fix_count` and history
/// unless `reset` is true. Stack-mates stay when a related PR is already
/// present; stack fields are inferred from `base` matching another watched
/// branch in the same repo.
pub fn add_prs(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    repo: &str,
    numbers: &[u64],
    kind: WatchKind,
    reset: bool,
    allowed_owners: &[String],
) -> Result<AddReport, WatchlistError> {
    validate_repo(repo)?;
    if let Some(owner) = owner_of_repo(repo)
        && !allowed_owners.is_empty()
        && !owner_is_allowed(owner, allowed_owners)
    {
        return Err(WatchlistError::OwnerNotAllowed {
            owner: owner.to_owned(),
        });
    }
    let now = utc_now_rfc3339();
    let mut report = AddReport {
        added: Vec::new(),
        refreshed: Vec::new(),
        skipped: Vec::new(),
    };
    for &number in numbers {
        let snapshot = probe.view(repo, number)?;
        let github_state = snapshot.state.to_ascii_uppercase();
        if github_state == "MERGED" || github_state == "CLOSED" {
            report
                .skipped
                .push((repo.to_owned(), number, snapshot.state.clone()));
            continue;
        }
        if let Some(existing) = list.get_mut(repo, number) {
            existing.branch = snapshot.branch.clone();
            existing.base = Some(snapshot.base.clone());
            existing.title = Some(snapshot.title.clone());
            existing.url = Some(snapshot.url.clone());
            if reset {
                existing.fix_count = 0;
                existing.check_count = Some(0);
                existing.residual_blockers.clear();
                existing.status = WatchStatus::Pending;
            }
            report.refreshed.push((repo.to_owned(), number));
        } else {
            list.prs.push(WatchEntry {
                repo: repo.to_owned(),
                number,
                branch: snapshot.branch.clone(),
                status: WatchStatus::Pending,
                last_checked: now.clone(),
                fix_count: 0,
                residual_blockers: Vec::new(),
                stack_id: None,
                stack_type: None,
                stack_position: None,
                base: Some(snapshot.base.clone()),
                title: Some(snapshot.title.clone()),
                added_at: Some(now.clone()),
                check_count: Some(0),
                url: Some(snapshot.url.clone()),
                kind: Some(kind),
                extra: serde_json::Map::new(),
            });
            report.added.push((repo.to_owned(), number));
        }
    }
    detect_stacks(list);
    Ok(report)
}

/// Remove one entry. Stack-mates remain; empty groups are dropped.
pub fn remove_pr(
    list: &mut Watchlist,
    repo: &str,
    number: u64,
) -> Result<WatchEntry, WatchlistError> {
    validate_repo(repo)?;
    let Some(index) = list
        .prs
        .iter()
        .position(|entry| entry.is_identity(repo, number))
    else {
        return Err(WatchlistError::NotFound {
            repo: repo.to_owned(),
            number,
        });
    };
    let removed = list.prs.remove(index);
    rebuild_groups(list);
    Ok(removed)
}

/// Run a check cycle over matching entries: refresh from GitHub, update
/// `last_checked` / `status` / `residual_blockers` / `check_count`, prune
/// MERGED/CLOSED. Does not increment `fix_count` (babysit owns that budget).
///
/// Visit order is stack-bottom first, then remaining PRs by repo and number.
pub fn check_prs(
    list: &mut Watchlist,
    probe: &impl PrProbe,
    owner: Option<&str>,
    repo: Option<&str>,
    numbers: Option<&[u64]>,
    allowed_owners: &[String],
) -> Result<CheckReport, WatchlistError> {
    let now = utc_now_rfc3339();
    let mut targets: Vec<(String, u64)> = ordered_identities(list)
        .into_iter()
        .filter(|(target_repo, number)| {
            repo.is_none_or(|want| target_repo == want)
                && owner.is_none_or(|want| {
                    owner_of_repo(target_repo).is_some_and(|have| have.eq_ignore_ascii_case(want))
                })
                && numbers.is_none_or(|want| want.contains(number))
        })
        .collect();

    if repo.is_none() && owner.is_none() && numbers.is_none() {
        // Multi-owner check-all requires an allowlist.
        if allowed_owners.is_empty() {
            return Err(WatchlistError::AllowlistRequired);
        }
        targets.retain(|(target_repo, _)| {
            owner_of_repo(target_repo).is_some_and(|have| owner_is_allowed(have, allowed_owners))
        });
    } else if !allowed_owners.is_empty() {
        targets.retain(|(target_repo, _)| {
            owner_of_repo(target_repo).is_some_and(|have| owner_is_allowed(have, allowed_owners))
        });
    }

    let mut pruned = Vec::new();
    let mut checked = Vec::new();
    for (target_repo, number) in targets {
        match probe.view(&target_repo, number) {
            Ok(snapshot) => {
                let github_state = snapshot.state.to_ascii_uppercase();
                if github_state == "MERGED" || github_state == "CLOSED" {
                    let _ = remove_pr(list, &target_repo, number);
                    pruned.push((target_repo, number, snapshot.state));
                    continue;
                }
                let (status, blockers) = classify_snapshot(&snapshot);
                if let Some(entry) = list.get_mut(&target_repo, number) {
                    entry.branch = snapshot.branch;
                    entry.base = Some(snapshot.base);
                    entry.title = Some(snapshot.title);
                    entry.url = Some(snapshot.url);
                    entry.status = status;
                    entry.residual_blockers = blockers;
                    entry.last_checked = now.clone();
                    entry.check_count = Some(entry.check_count.unwrap_or(0).saturating_add(1));
                    checked.push(entry.clone());
                }
            }
            Err(WatchlistError::Timeout { .. }) => {
                if let Some(entry) = list.get_mut(&target_repo, number) {
                    entry.status = WatchStatus::Timeout;
                    entry.residual_blockers = vec!["timeout:gh".to_owned()];
                    entry.last_checked = now.clone();
                    entry.check_count = Some(entry.check_count.unwrap_or(0).saturating_add(1));
                    checked.push(entry.clone());
                }
            }
            Err(err) => return Err(err),
        }
    }
    detect_stacks(list);
    Ok(CheckReport { checked, pruned })
}

/// Persist `add_prs` against `path`.
pub fn add_prs_at(
    path: &Path,
    probe: &impl PrProbe,
    repo: &str,
    numbers: &[u64],
    kind: WatchKind,
    reset: bool,
    allowed_owners: Option<&[String]>,
) -> Result<AddReport, WatchlistError> {
    let owners = allowed_owners
        .map(ToOwned::to_owned)
        .unwrap_or_else(load_allowed_owners);
    mutate_watchlist(path, |list| {
        add_prs(list, probe, repo, numbers, kind, reset, &owners)
    })
}

/// Persist `remove_pr` against `path`.
pub fn remove_pr_at(path: &Path, repo: &str, number: u64) -> Result<WatchEntry, WatchlistError> {
    mutate_watchlist(path, |list| remove_pr(list, repo, number))
}

/// Load and filter without writing.
pub fn list_prs_at(
    path: &Path,
    owner: Option<&str>,
    repo: Option<&str>,
) -> Result<Vec<WatchEntry>, WatchlistError> {
    let list = load_watchlist(path)?;
    Ok(list.filtered(owner, repo).into_iter().cloned().collect())
}

/// Persist `check_prs` against `path`.
pub fn check_prs_at(
    path: &Path,
    probe: &impl PrProbe,
    owner: Option<&str>,
    repo: Option<&str>,
    numbers: Option<&[u64]>,
    allowed_owners: Option<&[String]>,
) -> Result<CheckReport, WatchlistError> {
    let owners = allowed_owners
        .map(ToOwned::to_owned)
        .unwrap_or_else(load_allowed_owners);
    mutate_watchlist(path, |list| {
        check_prs(list, probe, owner, repo, numbers, &owners)
    })
}

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
        if list.get(&repo, incoming.number).is_some() {
            report.refreshed.push((repo, incoming.number));
            continue;
        }
        let status = map_imported_status(incoming.last_status.as_deref());
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

pub(crate) fn classify_snapshot(snapshot: &PrSnapshot) -> (WatchStatus, Vec<String>) {
    let mut blockers = Vec::new();
    let mergeable = snapshot
        .mergeable
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    if mergeable == "CONFLICTING" {
        blockers.push("conflict:mergeable".to_owned());
        return (WatchStatus::Conflict, blockers);
    }

    let mut failed = false;
    let mut pending = false;
    for check in &snapshot.checks {
        let state = check.state.to_ascii_uppercase();
        if matches!(
            state.as_str(),
            "FAILURE" | "FAIL" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "CANCELLED"
        ) {
            failed = true;
            blockers.push(format!("class_a:{}", sanitize_token(&check.name)));
        } else if matches!(
            state.as_str(),
            "PENDING" | "IN_PROGRESS" | "QUEUED" | "EXPECTED" | "UNKNOWN"
        ) {
            pending = true;
            blockers.push(format!("class_c:{}", sanitize_token(&check.name)));
        }
    }

    if let Some(decision) = snapshot.review_decision.as_deref() {
        let decision = decision.to_ascii_uppercase();
        if decision == "REVIEW_REQUIRED" || decision == "CHANGES_REQUESTED" {
            blockers.push(format!("review:{}", sanitize_token(&decision)));
        }
    }

    if failed {
        (WatchStatus::Failed, blockers)
    } else if pending {
        (WatchStatus::Pending, blockers)
    } else if !blockers.is_empty() {
        (WatchStatus::Residual, blockers)
    } else {
        (WatchStatus::Healthy, blockers)
    }
}

fn sanitize_token(name: &str) -> String {
    let token: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        "unnamed".to_owned()
    } else {
        token
    }
}

fn validate_repo(repo: &str) -> Result<(), WatchlistError> {
    if owner_of_repo(repo).is_none() {
        return Err(WatchlistError::InvalidInput(format!(
            "repo must be owner/name, got `{repo}`"
        )));
    }
    Ok(())
}

fn ordered_identities(list: &Watchlist) -> Vec<(String, u64)> {
    let mut stacked: Vec<(u32, String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_some())
        .map(|entry| {
            (
                entry.stack_position.unwrap_or(0),
                entry.repo.clone(),
                entry.number,
            )
        })
        .collect();
    stacked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut rest: Vec<(String, u64)> = list
        .prs
        .iter()
        .filter(|entry| entry.stack_id.is_none())
        .map(|entry| (entry.repo.clone(), entry.number))
        .collect();
    rest.sort();

    let mut out: Vec<(String, u64)> = stacked
        .into_iter()
        .map(|(_, repo, number)| (repo, number))
        .collect();
    out.extend(rest);
    out
}

fn detect_stacks(list: &mut Watchlist) {
    for index in 0..list.prs.len() {
        let Some(base) = list.prs[index].base.clone() else {
            continue;
        };
        let repo = list.prs[index].repo.clone();
        let parent = list
            .prs
            .iter()
            .enumerate()
            .find(|(other, entry)| *other != index && entry.repo == repo && entry.branch == base)
            .map(|(_, entry)| {
                (
                    entry.stack_id.clone(),
                    entry.stack_position.unwrap_or(0),
                    entry.number,
                )
            });
        let Some((parent_stack, parent_pos, parent_number)) = parent else {
            continue;
        };
        let stack_id = parent_stack.unwrap_or_else(|| format!("{repo}#{parent_number}"));
        if list.prs[index]
            .stack_id
            .as_ref()
            .is_none_or(|id| id.is_empty())
        {
            list.prs[index].stack_id = Some(stack_id.clone());
            list.prs[index].stack_position = Some(parent_pos.saturating_add(1));
        }
        if let Some(parent_entry) = list.prs.iter_mut().find(|entry| {
            entry.repo == repo && entry.number == parent_number && entry.stack_id.is_none()
        }) {
            parent_entry.stack_id = Some(stack_id);
            parent_entry.stack_position = Some(parent_pos);
        }
    }
    rebuild_groups(list);
}

fn rebuild_groups(list: &mut Watchlist) {
    let mut groups: BTreeMap<String, StackGroup> = BTreeMap::new();
    for entry in &list.prs {
        let Some(stack_id) = entry.stack_id.as_ref() else {
            continue;
        };
        let group = groups
            .entry(stack_id.clone())
            .or_insert_with(|| StackGroup {
                repo: entry.repo.clone(),
                numbers: Vec::new(),
            });
        if !group.numbers.contains(&entry.number) {
            group.numbers.push(entry.number);
        }
    }
    for group in groups.values_mut() {
        group.numbers.sort_by_key(|number| {
            list.prs
                .iter()
                .find(|entry| entry.number == *number && entry.repo == group.repo)
                .and_then(|entry| entry.stack_position)
                .unwrap_or(0)
        });
    }
    list.groups = groups;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchlist::schema::Watchlist;

    struct MapProbe {
        snaps: BTreeMap<(String, u64), PrSnapshot>,
    }

    impl PrProbe for MapProbe {
        fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
            self.snaps
                .get(&(repo.to_owned(), number))
                .cloned()
                .ok_or_else(|| WatchlistError::NotFound {
                    repo: repo.to_owned(),
                    number,
                })
        }
    }

    fn open_snap(repo: &str, number: u64, branch: &str, base: &str) -> PrSnapshot {
        PrSnapshot {
            repo: repo.to_owned(),
            number,
            branch: branch.to_owned(),
            base: base.to_owned(),
            title: format!("PR {number}"),
            url: format!("https://example.test/{repo}/pull/{number}"),
            state: "OPEN".to_owned(),
            mergeable: Some("MERGEABLE".to_owned()),
            review_decision: None,
            checks: Vec::new(),
        }
    }

    #[test]
    fn add_skips_merged_and_dedupes() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a", "main"),
        );
        probe.snaps.insert(("acme/widgets".to_owned(), 2), {
            let mut snap = open_snap("acme/widgets", 2, "feat/b", "main");
            snap.state = "MERGED".to_owned();
            snap
        });
        let mut list = Watchlist::default();
        let first = add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1, 2],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(first.added, vec![("acme/widgets".to_owned(), 1)]);
        assert_eq!(
            first.skipped,
            vec![("acme/widgets".to_owned(), 2, "MERGED".to_owned())]
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a-renamed", "main"),
        );
        list.get_mut("acme/widgets", 1).unwrap().fix_count = 2;
        let second = add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(second.refreshed, vec![("acme/widgets".to_owned(), 1)]);
        assert_eq!(
            list.get("acme/widgets", 1).unwrap().branch,
            "feat/a-renamed"
        );
        assert_eq!(list.get("acme/widgets", 1).unwrap().fix_count, 2);
    }

    #[test]
    fn add_rejects_disallowed_owner_when_allowlist_set() {
        let probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        let mut list = Watchlist::default();
        let err = add_prs(
            &mut list,
            &probe,
            "evil/repo",
            &[1],
            WatchKind::PrBabysit,
            false,
            &["acme".to_owned()],
        )
        .unwrap_err();
        assert!(matches!(err, WatchlistError::OwnerNotAllowed { .. }));
    }

    #[test]
    fn remove_keeps_stack_mates() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/base", "main"),
        );
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 2),
            open_snap("acme/widgets", 2, "feat/child", "feat/base"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1, 2],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        assert!(list.get("acme/widgets", 2).unwrap().stack_id.is_some());
        remove_pr(&mut list, "acme/widgets", 2).unwrap();
        assert!(list.get("acme/widgets", 1).is_some());
        assert!(list.get("acme/widgets", 2).is_none());
    }

    #[test]
    fn check_all_requires_allowlist_and_prunes_merged() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "feat/a", "main"),
        );
        probe.snaps.insert(("acme/widgets".to_owned(), 2), {
            let mut snap = open_snap("acme/widgets", 2, "feat/b", "main");
            snap.state = "CLOSED".to_owned();
            snap
        });
        probe.snaps.insert(
            ("other/repo".to_owned(), 9),
            open_snap("other/repo", 9, "feat/c", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        list.prs.push(WatchEntry {
            repo: "acme/widgets".to_owned(),
            number: 2,
            branch: "feat/b".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 1,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });
        list.prs.push(WatchEntry {
            repo: "other/repo".to_owned(),
            number: 9,
            branch: "feat/c".to_owned(),
            status: WatchStatus::Pending,
            last_checked: "2026-01-01T00:00:00Z".to_owned(),
            fix_count: 0,
            residual_blockers: Vec::new(),
            stack_id: None,
            stack_type: None,
            stack_position: None,
            base: None,
            title: None,
            added_at: None,
            check_count: Some(0),
            url: None,
            kind: None,
            extra: serde_json::Map::new(),
        });

        let err = check_prs(&mut list, &probe, None, None, None, &[]).unwrap_err();
        assert!(matches!(err, WatchlistError::AllowlistRequired));

        let report = check_prs(&mut list, &probe, None, None, None, &["acme".to_owned()]).unwrap();
        assert_eq!(
            report.pruned,
            vec![("acme/widgets".to_owned(), 2, "CLOSED".to_owned())]
        );
        assert_eq!(report.checked.len(), 1);
        assert_eq!(report.checked[0].status, WatchStatus::Healthy);
        assert!(list.get("other/repo", 9).is_some());
        assert!(list.get("acme/widgets", 2).is_none());
        assert_eq!(list.get("acme/widgets", 1).unwrap().fix_count, 0);
        assert_eq!(list.get("acme/widgets", 1).unwrap().check_count, Some(1));
    }

    #[test]
    fn classify_maps_fail_conflict_review() {
        let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
        snap.mergeable = Some("CONFLICTING".to_owned());
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Conflict);
        assert_eq!(blockers, vec!["conflict:mergeable".to_owned()]);

        snap.mergeable = Some("MERGEABLE".to_owned());
        snap.checks.push(CheckSnapshot {
            name: "ci / test".to_owned(),
            state: "FAILURE".to_owned(),
        });
        let (status, blockers) = classify_snapshot(&snap);
        assert_eq!(status, WatchStatus::Failed);
        assert_eq!(blockers, vec!["class_a:ci___test".to_owned()]);
    }

    #[test]
    fn multi_owner_entries_coexist() {
        let mut probe = MapProbe {
            snaps: BTreeMap::new(),
        };
        probe.snaps.insert(
            ("acme/widgets".to_owned(), 1),
            open_snap("acme/widgets", 1, "a", "main"),
        );
        probe.snaps.insert(
            ("example-org/core".to_owned(), 1),
            open_snap("example-org/core", 1, "b", "main"),
        );
        let mut list = Watchlist::default();
        add_prs(
            &mut list,
            &probe,
            "acme/widgets",
            &[1],
            WatchKind::PrBabysit,
            false,
            &[],
        )
        .unwrap();
        add_prs(
            &mut list,
            &probe,
            "example-org/core",
            &[1],
            WatchKind::IssueToPr,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(list.prs.len(), 2);
        assert_eq!(list.filtered(Some("acme"), None).len(), 1);
        assert_eq!(list.filtered(None, Some("example-org/core")).len(), 1);
    }
}
