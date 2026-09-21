//! Tests for the collaboration watchlist view assembler.

use super::*;
use crate::lease::{LeaseGrant, LeaseStore};
use crate::watchlist::github::{CheckSnapshot, ProbeError};
use rusqlite::Connection;
use tempfile::tempdir;

struct NoGithub;

impl GithubProbe for NoGithub {
    fn view(&self, repo: &str, _number: u64) -> std::result::Result<PrSnapshot, ProbeError> {
        Err(ProbeError::Gh {
            repo: repo.to_owned(),
            message: "unused".to_owned(),
        })
    }

    fn find_by_branch(
        &self,
        repo: &str,
        _branch: &str,
    ) -> std::result::Result<Option<PrSnapshot>, ProbeError> {
        Err(ProbeError::OwnerNotAllowed {
            repo: repo.to_owned(),
        })
    }
}

struct FakePr;

impl GithubProbe for FakePr {
    fn view(&self, _repo: &str, _number: u64) -> std::result::Result<PrSnapshot, ProbeError> {
        unimplemented!()
    }

    fn find_by_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> std::result::Result<Option<PrSnapshot>, ProbeError> {
        Ok(Some(PrSnapshot {
            repo: repo.to_owned(),
            number: 41,
            branch: branch.to_owned(),
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
        }))
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

const COORD_TABLES: &str = "
            CREATE TABLE coord_claims (
                owner TEXT, repo_name TEXT, job_id TEXT, agent_id TEXT,
                session_id TEXT, intent TEXT, declared_paths TEXT,
                owner_generation INTEGER, paused_at INTEGER
            );
            CREATE TABLE coord_messages (
                kind TEXT, from_agent_id TEXT, from_job_id TEXT,
                to_owner TEXT, to_repo_name TEXT, to_job_id TEXT,
                body TEXT, acked_at INTEGER
            );
";

#[test]
fn list_reads_leases_without_coord_or_github() {
    let seeded = seed_job();
    let data = load_view(&seeded.store, &WatchQuery::default(), None).unwrap();
    assert!(!data.coord_available);
    assert!(!data.github_probed);
    assert_eq!(data.entries.len(), 1);
    assert_eq!(data.entries[0].collab_status, CollabStatus::Running);
    assert_eq!(data.entries[0].recovery_status, RecoveryStatus::Live);
    assert!(data.entries[0].github.is_none());
}

#[test]
fn paused_claim_and_help_message_shape_waiting_and_paused() {
    let seeded = seed_job();
    exec_sql(
        &seeded.store,
        &format!(
            "{COORD_TABLES}
            INSERT INTO coord_claims VALUES (
                'acme','sample','job-1','agent-a',NULL,NULL,'[]',1,10
            );"
        ),
    );
    let data = load_view(&seeded.store, &WatchQuery::default(), None).unwrap();
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
    let data = load_view(&seeded.store, &query, Some(&FakePr)).unwrap();
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
    let data = load_view(&seeded.store, &query, Some(&NoGithub)).unwrap();
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
        &format!(
            "{COORD_TABLES}
            INSERT INTO coord_claims VALUES (
                'acme','sample','job-1','agent-a',NULL,NULL,'[]',1,NULL
            );
            INSERT INTO coord_messages VALUES (
                'handoff','agent-b','job-2','acme','sample','job-1','take over', NULL
            );"
        ),
    );
    let data = load_view(&seeded.store, &WatchQuery::default(), None).unwrap();
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
    let data = load_view(&seeded.store, &query, None).unwrap();
    assert_eq!(data.entries.len(), 1);
    let query = WatchQuery {
        owner: Some("other".to_owned()),
        ..WatchQuery::default()
    };
    let data = load_view(&seeded.store, &query, None).unwrap();
    assert!(data.entries.is_empty());
}

#[test]
fn merge_ready_lease_is_ready_for_integration() {
    let seeded = seed_job();
    exec_sql(&seeded.store, "UPDATE leases SET mode = 'MERGE_READY'");
    let data = load_view(&seeded.store, &WatchQuery::default(), None).unwrap();
    assert_eq!(
        data.entries[0].collab_status,
        CollabStatus::ReadyForIntegration
    );
}
