use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use super::*;
use crate::watchlist::import::import_pr_babysit;
use crate::watchlist::probe::{CheckSnapshot, parse_pr_view};
use crate::watchlist::schema::{WatchStatus, Watchlist};
use crate::watchlist::stack::compute_parents;

struct MapProbe {
    snaps: BTreeMap<(String, u64), PrSnapshot>,
}

impl PrProbe for MapProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
        self.snaps
            .iter()
            .find_map(|((have_repo, have_number), snap)| {
                (*have_number == number && repos_match(have_repo, repo)).then(|| snap.clone())
            })
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
        is_draft: false,
        checks: vec![CheckSnapshot {
            name: "ci".to_owned(),
            state: "SUCCESS".to_owned(),
        }],
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

    snap.checks = vec![CheckSnapshot {
        name: "codacy".to_owned(),
        state: "ACTION_REQUIRED".to_owned(),
    }];
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Residual);
    assert_eq!(blockers, vec!["class_b:codacy".to_owned()]);

    snap.checks = vec![CheckSnapshot {
        name: "setup".to_owned(),
        state: "STARTUP_FAILURE".to_owned(),
    }];
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Failed);
    assert_eq!(blockers, vec!["class_a:setup".to_owned()]);

    snap.checks = vec![CheckSnapshot {
        name: "ci".to_owned(),
        state: "STALE".to_owned(),
    }];
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Failed);
    assert_eq!(blockers, vec!["class_a:ci".to_owned()]);
}

#[test]
fn classify_empty_rollup_is_pending() {
    let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
    snap.checks.clear();
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Pending);
    assert!(blockers.is_empty());

    snap.review_decision = Some("REVIEW_REQUIRED".to_owned());
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Residual);
    assert_eq!(blockers, vec!["review:review_required".to_owned()]);
}

#[test]
fn parse_pr_view_empty_conclusion_is_pending() {
    let json = r#"{
        "number": 7,
        "title": "wip",
        "url": "https://example.test/acme/widgets/pull/7",
        "state": "OPEN",
        "headRefName": "feat/a",
        "baseRefName": "main",
        "mergeable": "MERGEABLE",
        "reviewDecision": null,
        "isDraft": false,
        "statusCheckRollup": [
            {"name": "ci", "conclusion": "", "status": "IN_PROGRESS", "state": ""}
        ]
    }"#;
    let snap = parse_pr_view("acme/widgets", json).unwrap();
    assert_eq!(snap.checks[0].state, "IN_PROGRESS");
    let (status, _) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Pending);
}

#[test]
fn classify_mergeable_unknown_is_pending() {
    let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
    snap.mergeable = Some("UNKNOWN".to_owned());
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Pending);
    assert!(blockers.contains(&"pending:mergeable_unknown".to_owned()));
}

#[test]
fn classify_draft_is_pending() {
    let mut snap = open_snap("acme/widgets", 1, "feat/a", "main");
    snap.is_draft = true;
    let (status, blockers) = classify_snapshot(&snap);
    assert_eq!(status, WatchStatus::Pending);
    assert!(blockers.contains(&"draft:true".to_owned()));
}

#[test]
fn probe_timeout_maps_to_watchlist_timeout() {
    // A stalled child must surface as WatchlistError::Timeout. `sleep 5`
    // is not gh, so we exercise the mapping via a tiny probe that reuses
    // the same GhRun::TimedOut -> Timeout logic shape.
    struct SlowProbe;
    impl PrProbe for SlowProbe {
        fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
            use crate::git_safe::{GhRun, SafeGhCommand};
            // `version` is allowlisted and returns fast; force the timeout
            // path with a zero deadline to assert the mapping.
            let cmd = SafeGhCommand::new(&[
                "pr".to_owned(),
                "view".to_owned(),
                number.to_string(),
                "--repo".to_owned(),
                repo.to_owned(),
            ])?;
            match cmd.run_with_timeout(Duration::from_millis(0)) {
                Ok(GhRun::TimedOut { timeout }) => Err(WatchlistError::Timeout {
                    repo: repo.to_owned(),
                    number,
                    message: format!("gh exceeded {}s deadline", timeout.as_secs()),
                }),
                Ok(GhRun::Completed(_)) => Ok(open_snap(repo, number, "feat/a", "main")),
                Err(err) => Err(err.into()),
            }
        }
    }
    let err = SlowProbe.view("acme/widgets", 1).unwrap_err();
    assert!(
        matches!(err, WatchlistError::Timeout { number: 1, .. }),
        "expected timeout, got {err:?}"
    );
}

#[test]
fn import_skips_malformed_repo() {
    let dir = std::env::temp_dir().join(format!(
        "watchlist-import-bad-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("watched-prs.json");
    fs::write(
        &source,
        r#"{"prs": [{"number": 1, "repo": "not-a-slug", "branch": "x"}]}"#,
    )
    .unwrap();
    let mut list = Watchlist::default();
    let report = import_pr_babysit(&mut list, &source).unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(report.added.is_empty());
    assert!(list.prs.is_empty());
}

#[test]
fn import_refreshes_existing_and_preserves_fix_count() {
    let dir = std::env::temp_dir().join(format!(
        "watchlist-import-refresh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("watched-prs.json");
    fs::write(
        &source,
        r#"{"prs": [{"number": 5, "repo": "acme/widgets", "branch": "feat/new",
            "title": "Fresh title", "url": "https://example.test/x",
            "check_count": 9, "last_status": "healthy"}]}"#,
    )
    .unwrap();
    let mut list = Watchlist::default();
    list.prs.push(WatchEntry {
        repo: "acme/widgets".to_owned(),
        number: 5,
        branch: "feat/old".to_owned(),
        status: WatchStatus::Pending,
        last_checked: "2026-01-01T00:00:00Z".to_owned(),
        fix_count: 3,
        residual_blockers: Vec::new(),
        stack_id: None,
        stack_type: None,
        stack_position: None,
        base: Some("main".to_owned()),
        title: Some("Old title".to_owned()),
        added_at: Some("2026-01-01T00:00:00Z".to_owned()),
        check_count: Some(1),
        url: None,
        kind: Some(WatchKind::PrBabysit),
        extra: serde_json::Map::new(),
    });
    let report = import_pr_babysit(&mut list, &source).unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(report.refreshed, vec![("acme/widgets".to_owned(), 5)]);
    let entry = list.get("acme/widgets", 5).unwrap();
    assert_eq!(entry.branch, "feat/new");
    assert_eq!(entry.title.as_deref(), Some("Fresh title"));
    assert_eq!(entry.url.as_deref(), Some("https://example.test/x"));
    assert_eq!(entry.check_count, Some(9));
    assert_eq!(entry.status, WatchStatus::Healthy);
    // Hive budget preserved.
    assert_eq!(entry.fix_count, 3);
}

#[test]
fn three_deep_stack_is_stable_regardless_of_add_order() {
    let mut probe = MapProbe {
        snaps: BTreeMap::new(),
    };
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 3),
        open_snap("acme/widgets", 3, "feat/top", "feat/mid"),
    );
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 2),
        open_snap("acme/widgets", 2, "feat/mid", "feat/base"),
    );
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 1),
        open_snap("acme/widgets", 1, "feat/base", "main"),
    );
    let mut list = Watchlist::default();
    add_prs(
        &mut list,
        &probe,
        "acme/widgets",
        &[3, 2, 1],
        WatchKind::PrBabysit,
        false,
        &[],
    )
    .unwrap();
    let bottom = list.get("acme/widgets", 1).unwrap();
    let mid = list.get("acme/widgets", 2).unwrap();
    let top = list.get("acme/widgets", 3).unwrap();
    assert_eq!(bottom.stack_id, mid.stack_id);
    assert_eq!(mid.stack_id, top.stack_id);
    assert_eq!(bottom.stack_position, Some(0));
    assert_eq!(mid.stack_position, Some(1));
    assert_eq!(top.stack_position, Some(2));
    let group = list.groups.get(bottom.stack_id.as_ref().unwrap()).unwrap();
    assert_eq!(group.numbers, vec![1, 2, 3]);
}

#[test]
fn stack_parent_selection_prefers_lowest_number_on_branch_collision() {
    // Two PRs (5 and 9) share the head branch `feat/base`; a child PR 7
    // bases on `feat/base`. The inferred parent must deterministically be
    // the lowest number (5), independent of insertion order.
    let mut probe = MapProbe {
        snaps: BTreeMap::new(),
    };
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 9),
        open_snap("acme/widgets", 9, "feat/base", "main"),
    );
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 7),
        open_snap("acme/widgets", 7, "feat/child", "feat/base"),
    );
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 5),
        open_snap("acme/widgets", 5, "feat/base", "main"),
    );
    let mut list = Watchlist::default();
    add_prs(
        &mut list,
        &probe,
        "acme/widgets",
        &[9, 7, 5],
        WatchKind::PrBabysit,
        false,
        &[],
    )
    .unwrap();
    let parent = compute_parents(&list);
    let child_idx = list
        .prs
        .iter()
        .position(|e| e.number == 7)
        .expect("child present");
    let root_idx = parent[child_idx].expect("child has a parent");
    assert_eq!(list.prs[root_idx].number, 5);
}

#[test]
fn filtered_check_rejects_disallowed_owner() {
    let probe = MapProbe {
        snaps: BTreeMap::new(),
    };
    let mut list = Watchlist::default();
    list.prs.push(WatchEntry {
        repo: "evil/repo".to_owned(),
        number: 1,
        branch: "feat/x".to_owned(),
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
    let err = check_prs(
        &mut list,
        &probe,
        None,
        Some("evil/repo"),
        None,
        &["acme".to_owned()],
    )
    .unwrap_err();
    assert!(matches!(err, WatchlistError::OwnerNotAllowed { .. }));
}

#[test]
fn check_missing_identity_is_not_found() {
    let probe = MapProbe {
        snaps: BTreeMap::new(),
    };
    let mut list = Watchlist::default();
    let err = check_prs(
        &mut list,
        &probe,
        None,
        Some("acme/widgets"),
        Some(&[41]),
        &[],
    )
    .unwrap_err();
    assert!(matches!(err, WatchlistError::NotFound { number: 41, .. }));

    let err = check_prs(
        &mut list,
        &probe,
        None,
        Some("not-a-slug"),
        Some(&[41]),
        &[],
    )
    .unwrap_err();
    assert!(matches!(err, WatchlistError::InvalidInput(_)));
}

#[test]
fn add_and_check_treat_repo_slug_case_as_identity() {
    let mut probe = MapProbe {
        snaps: BTreeMap::new(),
    };
    probe.snaps.insert(
        ("acme/widgets".to_owned(), 41),
        open_snap("acme/widgets", 41, "feat/a", "main"),
    );
    let mut list = Watchlist::default();
    add_prs(
        &mut list,
        &probe,
        "acme/widgets",
        &[41],
        WatchKind::PrBabysit,
        false,
        &[],
    )
    .unwrap();
    let second = add_prs(
        &mut list,
        &probe,
        "ACME/widgets",
        &[41],
        WatchKind::PrBabysit,
        false,
        &[],
    )
    .unwrap();
    assert_eq!(second.refreshed, vec![("ACME/widgets".to_owned(), 41)]);
    assert!(second.added.is_empty());
    assert_eq!(list.prs.len(), 1);

    let report = check_prs(
        &mut list,
        &probe,
        None,
        Some("ACME/Widgets"),
        Some(&[41]),
        &[],
    )
    .unwrap();
    assert_eq!(report.checked.len(), 1);
    assert_eq!(report.checked[0].repo, "acme/widgets");
    remove_pr(&mut list, "Acme/widgets", 41).unwrap();
    assert!(list.prs.is_empty());
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

struct FailSecondProbe {
    first: PrSnapshot,
}

impl PrProbe for FailSecondProbe {
    fn view(&self, repo: &str, number: u64) -> Result<PrSnapshot, WatchlistError> {
        if number == 1 {
            Ok(self.first.clone())
        } else {
            Err(WatchlistError::Gh {
                repo: repo.to_owned(),
                number,
                message: "boom".to_owned(),
            })
        }
    }
}

#[test]
fn check_persists_completed_entries_before_gh_failure() {
    let dir = std::env::temp_dir().join(format!(
        "watchlist-partial-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("watchlist.json");
    let mut list = Watchlist::default();
    list.prs.push(WatchEntry {
        repo: "acme/widgets".to_owned(),
        number: 1,
        branch: "feat/a".to_owned(),
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
    list.prs.push(WatchEntry {
        repo: "acme/widgets".to_owned(),
        number: 2,
        branch: "feat/b".to_owned(),
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
    crate::watchlist::save_watchlist(&path, &list).unwrap();
    let probe = FailSecondProbe {
        first: open_snap("acme/widgets", 1, "feat/a", "main"),
    };
    let err = check_prs_at(&path, &probe, None, Some("acme/widgets"), None, Some(&[])).unwrap_err();
    assert!(matches!(err, WatchlistError::Gh { number: 2, .. }));
    let loaded = crate::watchlist::load_watchlist(&path).unwrap();
    let first = loaded.get("acme/widgets", 1).unwrap();
    assert_eq!(first.status, WatchStatus::Healthy);
    assert_eq!(first.check_count, Some(1));
    assert_eq!(
        loaded.get("acme/widgets", 2).unwrap().status,
        WatchStatus::Pending
    );
    let _ = fs::remove_dir_all(&dir);
}
