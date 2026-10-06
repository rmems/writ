use super::*;
use std::fs;

#[test]
fn interrupted_allocation_on_a_different_branch_stays_fail_closed() {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    git(&harness.worktree, &["switch", "-c", "hive/job-2"]);
    assert_eq!(
        git(&harness.worktree, &["rev-parse", "HEAD"]),
        harness.start
    );

    let outcome = assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    let ReconcileOutcome::NeedsAttention { inspection, .. } = outcome else {
        panic!("expected needs attention, got {outcome:?}");
    };
    assert!(
        inspection
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("worker branch"))
    );
}

#[test]
fn interrupted_detached_allocation_attached_to_branch_stays_fail_closed() {
    let harness = RepoHarness::new();
    let prepared = harness
        .store
        .prepare_allocate(AllocateRequest {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "(detached)",
            worktree_path: &harness.worktree,
            requested_start_point: "HEAD",
            start_commit: &harness.start,
            ttl: None,
        })
        .unwrap();
    harness.store.mark_mutating(&prepared.operation_id).unwrap();
    git(
        &harness.repo,
        &[
            "worktree",
            "add",
            "-b",
            "hive/attached",
            "--",
            harness.worktree.to_str().unwrap(),
            &harness.start,
        ],
    );

    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
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
    assert!(orphaned.lease_present && orphaned.ttl_expired && orphaned.path_exists);
    let missing_store = LeaseStore::open(harness.store.path().with_file_name("other.db")).unwrap();
    let missing = missing_store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(missing.classification, EvidenceClass::MissingLease);
    assert!(!missing.lease_present && missing.path_exists && !missing.ttl_expired);
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
fn inspect_rejects_a_foreign_repository_with_the_same_branch() {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let foreign = harness._temp.path().join("foreign");
    git(
        harness._temp.path(),
        &[
            "clone",
            "--quiet",
            harness.repo.to_str().unwrap(),
            foreign.to_str().unwrap(),
        ],
    );
    git(&foreign, &["branch", "hive/job-1", &harness.start]);
    let inspection = harness
        .store
        .inspect(InspectRequest {
            repo_root: &foreign,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &harness.worktree,
            branch: Some("hive/job-1"),
        })
        .unwrap();
    assert_eq!(inspection.classification, EvidenceClass::NeedsAttention);
    assert!(
        inspection
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("git identity"))
    );
}

#[test]
fn inspect_does_not_retry_prepared_lease_against_a_foreign_repo() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_eq!(prepared.allocation_state, AllocationState::Prepared);
    let foreign = harness._temp.path().join("missing-foreign-repo");
    let inspection = harness
        .store
        .inspect(InspectRequest {
            repo_root: &foreign,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &harness.worktree,
            branch: Some("hive/job-1"),
        })
        .unwrap();
    assert_eq!(inspection.classification, EvidenceClass::NeedsAttention);
    assert!(
        inspection
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("git identity")),
        "{:?}",
        inspection.conflicts
    );
    let outcome = assert_outcome(&harness.store, harness.key(), &foreign, "needs_attention");
    assert!(matches!(outcome, ReconcileOutcome::NeedsAttention { .. }));
    assert!(harness.store.find_job(harness.key()).unwrap().is_some());
}

#[test]
fn unborn_head_matches_empty_start_commit() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("unborn");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    let store = LeaseStore::open(temp.path().join("leases.db")).unwrap();
    store
        .prepare_allocate(AllocateRequest {
            repo: &repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "main",
            worktree_path: &repo,
            requested_start_point: "refs/heads/main",
            start_commit: "",
            ttl: None,
        })
        .unwrap();
    let inspection = store
        .inspect(InspectRequest {
            repo_root: &repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &repo,
            branch: Some("main"),
        })
        .unwrap();
    assert!(
        !inspection
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("HEAD is absent")),
        "{:?}",
        inspection.conflicts
    );
}
