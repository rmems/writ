//! Tests for the collaboration watchlist view assembler.

use super::*;
use crate::lease::{LeaseGrant, LeaseStore};
use crate::owners::OwnerAllowlist;
use crate::watchlist::github::{CheckSnapshot, PrRef, ProbeError};
use rusqlite::Connection;
use tempfile::tempdir;

const ACME: &str = "acme";

fn allowlist() -> OwnerAllowlist {
    OwnerAllowlist::parse(ACME)
}

fn view(
    store: &LeaseStore,
    query: &WatchQuery,
    probe: Option<&dyn GithubProbe>,
    allowlist: &OwnerAllowlist,
) -> WatchlistData {
    load_view(ViewLoad {
        store,
        query,
        probe,
        allowlist,
    })
    .unwrap()
}

struct NoGithub;

impl GithubProbe for NoGithub {
    fn view(&self, target: PrRef<'_>) -> std::result::Result<PrSnapshot, ProbeError> {
        Err(ProbeError::Gh {
            repo: target.repo.to_owned(),
            message: "unused".to_owned(),
        })
    }

    fn list_prs(&self, repo: &str) -> std::result::Result<Vec<PrSnapshot>, ProbeError> {
        Err(ProbeError::OwnerNotAllowed {
            repo: repo.to_owned(),
        })
    }
}

struct FakePr;

impl GithubProbe for FakePr {
    fn view(&self, _target: PrRef<'_>) -> std::result::Result<PrSnapshot, ProbeError> {
        unimplemented!()
    }

    fn list_prs(&self, repo: &str) -> std::result::Result<Vec<PrSnapshot>, ProbeError> {
        Ok(vec![PrSnapshot {
            repo: repo.to_owned(),
            number: 41,
            branch: "hive/job-1".to_owned(),
            head_owner: Some("acme".to_owned()),
            base: "main".to_owned(),
            title: "fix".to_owned(),
            url: "https://example.test/41".to_owned(),
            state: "OPEN".to_owned(),
            mergeable: Some("CONFLICTING".to_owned()),
            review_decision: None,
            is_draft: false,
            checks: vec![CheckSnapshot {
                name: "ci".to_owned(),
                state: "SUCCESS".to_owned(),
            }],
        }])
    }
}

struct NoPr;

impl GithubProbe for NoPr {
    fn view(&self, _target: PrRef<'_>) -> std::result::Result<PrSnapshot, ProbeError> {
        unimplemented!()
    }

    fn list_prs(&self, _repo: &str) -> std::result::Result<Vec<PrSnapshot>, ProbeError> {
        Ok(Vec::new())
    }
}

struct SeededStore {
    _tmp: tempfile::TempDir,
    store: LeaseStore,
}

fn seed_job() -> SeededStore {
    let tmp = tempdir().unwrap();
    let db = tmp.path().join("leases.db");
    let store = LeaseStore::open(&db).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("checkout");
    std::fs::create_dir_all(&wt).unwrap();
    let grant = LeaseGrant {
        repo: &repo,
        owner: "acme",
        repo_name: "sample",
        job_id: "job-1",
        branch: "hive/job-1",
        worktree_path: &wt,
        start_commit: "abc123",
    };
    store.grant(grant).unwrap();
    SeededStore { _tmp: tmp, store }
}

fn exec_sql(store: &LeaseStore, sql: &str) {
    Connection::open(store.path())
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

fn assert_interrupted_collab(store: &LeaseStore, state: &str, expected: CollabStatus) {
    exec_sql(
        store,
        &format!(
            "UPDATE leases SET allocation_state = '{state}', \
             released_at = NULL, tombstoned_at = NULL"
        ),
    );
    let data = view(store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(data.entries.len(), 1, "{state}");
    assert_eq!(
        data.entries[0].recovery_status,
        RecoveryStatus::NeedsReconcile,
        "{state}"
    );
    assert_eq!(data.entries[0].collab_status, expected, "{state}");
}

#[test]
fn list_reads_leases_without_coord_or_github() {
    let seeded = seed_job();
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert!(data.coord_available);
    assert!(!data.github_probed);
    assert_eq!(data.entries.len(), 1);
    assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
    assert_eq!(data.entries[0].recovery_status, RecoveryStatus::Live);
    assert!(data.entries[0].github.is_none());
    assert!(data.entries[0].coord.agent_id.is_none());
}

#[test]
fn paused_claim_and_help_message_shape_waiting_and_paused() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        "
            INSERT INTO coord_claims (
                owner, repo_name, job_id, branch, worktree_path, agent_id,
                session_id, intent, declared_paths, owner_generation, paused_at,
                created_at, updated_at
            ) VALUES (
                'acme','sample','job-1','hive/job-1','checkout','agent-a',
                NULL,NULL,'[]',1,10,1,1
            );
        ",
    );
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert!(data.coord_available);
    assert_eq!(data.entries[0].collab_status, CollabStatus::Paused);
    assert!(data.entries[0].coord.paused);
}

#[test]
fn github_conflict_marks_conflicted_without_merge_gate() {
    let seeded = seed_job();
    let query = WatchQuery {
        probe_github: true,
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, Some(&FakePr), &allowlist());
    assert!(data.github_probed);
    assert_eq!(data.entries[0].collab_status, CollabStatus::Conflicted);
    let github = data.entries[0].github.as_ref().unwrap();
    assert_eq!(github.check_status, "conflict");
    assert_eq!(github.number, 41);
}

#[test]
fn github_probe_failure_stays_on_lease_view() {
    let seeded = seed_job();
    let query = WatchQuery {
        probe_github: true,
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, Some(&NoGithub), &allowlist());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
    assert!(
        data.entries[0]
            .residual_blockers
            .iter()
            .any(|b| b.contains("owner_not_allowed"))
    );
}

#[test]
fn unacked_help_without_pause_is_waiting() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        "
            INSERT INTO coord_claims (
                owner, repo_name, job_id, branch, worktree_path, agent_id,
                session_id, intent, declared_paths, owner_generation, paused_at,
                created_at, updated_at
            ) VALUES (
                'acme','sample','job-1','hive/job-1','checkout','agent-a',
                NULL,NULL,'[]',1,NULL,1,1
            );
            INSERT INTO coord_messages (
                created_at, kind, from_agent_id, from_owner, from_repo_name, from_job_id,
                to_agent_id, to_owner, to_repo_name, to_job_id, owner_generation,
                body, paths, ack_of, acked_at
            ) VALUES (
                1,'handoff','agent-b','acme','sample','job-2',
                NULL,'acme','sample','job-1',1,
                'take over','[]',NULL,NULL
            );
        ",
    );
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Waiting);
    assert_eq!(
        data.entries[0].coord.waiting_on.as_deref(),
        Some("handoff:agent-b:job-2")
    );
}

#[test]
fn owner_filter_is_case_insensitive() {
    let seeded = seed_job();
    let query = WatchQuery {
        owner: Some("ACME".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert_eq!(data.entries.len(), 1);
    let query = WatchQuery {
        owner: Some("other".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert!(data.entries.is_empty());
}

#[test]
fn merge_ready_lease_is_ready_for_integration() {
    let seeded = seed_job();
    exec_sql(&seeded.store, "UPDATE leases SET mode = 'MERGE_READY'");
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(
        data.entries[0].collab_status,
        CollabStatus::ReadyForIntegration
    );
}

#[test]
fn repo_and_job_filters() {
    let seeded = seed_job();
    let query = WatchQuery {
        repo: Some("acme/sample".to_owned()),
        job_id: Some("job-1".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert_eq!(data.entries.len(), 1);
    let query = WatchQuery {
        repo: Some("sample".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert_eq!(data.entries.len(), 1);
    let query = WatchQuery {
        repo: Some("other/repo".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert!(data.entries.is_empty());
    let query = WatchQuery {
        job_id: Some("missing".to_owned()),
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert!(data.entries.is_empty());
}

#[test]
fn include_released_lists_unassigned_rows() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        "UPDATE leases SET mode = 'UNASSIGNED', released_at = 99",
    );
    let hidden = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert!(hidden.entries.is_empty());
    let query = WatchQuery {
        include_released: true,
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, None, &allowlist());
    assert_eq!(data.entries.len(), 1);
    assert_eq!(data.entries[0].recovery_status, RecoveryStatus::Released);
    assert_eq!(data.entries[0].collab_status, CollabStatus::Released);
}

#[test]
fn missing_checkout_is_recovery_blocker() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        "UPDATE leases SET worktree_path = '/tmp/writ-missing-checkout-does-not-exist'",
    );
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(
        data.entries[0].recovery_status,
        RecoveryStatus::MissingCheckout
    );
    assert!(
        data.entries[0]
            .residual_blockers
            .iter()
            .any(|b| b == "recovery:missing_checkout")
    );
}

#[test]
fn stale_heartbeat_is_recovery_blocker() {
    let seeded = seed_job();
    exec_sql(&seeded.store, "UPDATE leases SET ttl = 1, heartbeat = 1");
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(
        data.entries[0].recovery_status,
        RecoveryStatus::StaleHeartbeat
    );
}

#[test]
fn blocked_and_review_only_are_waiting() {
    let seeded = seed_job();
    exec_sql(&seeded.store, "UPDATE leases SET mode = 'BLOCKED'");
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Waiting);
    exec_sql(&seeded.store, "UPDATE leases SET mode = 'REVIEW_ONLY'");
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Waiting);
}

#[test]
fn needs_human_is_conflicted() {
    let seeded = seed_job();
    exec_sql(&seeded.store, "UPDATE leases SET mode = 'NEEDS_HUMAN'");
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Conflicted);
}

#[test]
fn github_none_leaves_github_overlay_empty() {
    let seeded = seed_job();
    let query = WatchQuery {
        probe_github: true,
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, Some(&NoPr), &allowlist());
    assert!(data.entries[0].github.is_none());
    assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
}

#[test]
fn disallowed_owner_is_filtered_from_entries() {
    let seeded = seed_job();
    let deny = OwnerAllowlist::parse("other");
    let data = view(&seeded.store, &WatchQuery::default(), None, &deny);
    assert!(data.entries.is_empty());
    let empty_allowlist = OwnerAllowlist::default();
    let data = view(
        &seeded.store,
        &WatchQuery::default(),
        None,
        &empty_allowlist,
    );
    assert!(data.entries.is_empty());
}

#[test]
fn coord_read_failure_surfaces_residual() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        "DROP TABLE coord_claims; CREATE TABLE coord_claims (broken INTEGER);",
    );
    let data = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert!(
        data.entries[0]
            .residual_blockers
            .iter()
            .any(|b| b.starts_with("coord:read_failed:"))
    );
}

#[test]
fn stale_closed_pr_is_skipped_for_open_match() {
    struct ReusedBranch;
    impl GithubProbe for ReusedBranch {
        fn view(&self, _target: PrRef<'_>) -> std::result::Result<PrSnapshot, ProbeError> {
            unimplemented!()
        }
        fn list_prs(&self, repo: &str) -> std::result::Result<Vec<PrSnapshot>, ProbeError> {
            let snap = |number: u64, state: &str, head_owner: &str| PrSnapshot {
                repo: repo.to_owned(),
                number,
                branch: "hive/job-1".to_owned(),
                head_owner: Some(head_owner.to_owned()),
                base: "main".to_owned(),
                title: String::new(),
                url: String::new(),
                state: state.to_owned(),
                mergeable: None,
                review_decision: None,
                is_draft: false,
                checks: Vec::new(),
            };
            Ok(vec![
                snap(30, "CLOSED", "acme"),
                snap(31, "OPEN", "acme"),
                snap(32, "OPEN", "fork-owner"),
            ])
        }
    }
    let seeded = seed_job();
    let query = WatchQuery {
        probe_github: true,
        ..WatchQuery::default()
    };
    let data = view(&seeded.store, &query, Some(&ReusedBranch), &allowlist());
    let github = data.entries[0].github.as_ref().unwrap();
    assert_eq!(github.number, 31);
}

#[test]
fn interrupted_leases_appear_on_the_default_watchlist() {
    let seeded = seed_job();
    let waiting = ["PREPARED", "MUTATING", "ABORTED"];
    let conflicted = ["NEEDS_ATTENTION", "UNKNOWN"];
    for state in waiting {
        assert_interrupted_collab(&seeded.store, state, CollabStatus::Waiting);
    }
    for state in conflicted {
        assert_interrupted_collab(&seeded.store, state, CollabStatus::Conflicted);
    }
    exec_sql(
        &seeded.store,
        "UPDATE leases SET allocation_state = 'RELEASED', released_at = 99",
    );
    let hidden = view(&seeded.store, &WatchQuery::default(), None, &allowlist());
    assert!(hidden.entries.is_empty());
}
