use std::fs;

use super::{RepoHarness, add_job_worktree, git, later_commit, prepare_and_mutate};
use crate::error::{Error, PolicyCode};
use crate::lease::{AllocationState, EvidenceClass, LeaseMode, LeaseStore, ReconcileOutcome};

#[test]
fn prepare_records_symbolic_and_canonical_identity() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_eq!(prepared.allocation_state, AllocationState::Prepared);
    assert_eq!(prepared.requested_start_point, "refs/heads/main");
    assert_eq!(prepared.start_commit, harness.start);
    assert_ne!(prepared.requested_start_point, prepared.start_commit);
}

#[test]
fn crash_before_mutation_is_retryable_without_reserving_identity() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "retry");
    assert!(harness.store.find_job(harness.key()).unwrap().is_none());

    let again = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_ne!(again.operation_id, prepared.operation_id);
}

#[test]
fn crash_after_branch_creation_is_fail_closed_without_cleanup() {
    let harness = RepoHarness::new();
    prepare_and_mutate(&harness);
    git(&harness.repo, &["branch", "hive/job-1", &harness.start]);

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "needs_attention");
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        harness.start
    );
    assert!(!harness.worktree.exists());
    let lease = harness.store.find_job(harness.key()).unwrap().unwrap();
    assert_eq!(lease.allocation_state, AllocationState::NeedsAttention);
}

#[test]
fn crash_after_filesystem_creation_is_fail_closed_without_cleanup() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    fs::create_dir_all(&harness.worktree).unwrap();
    fs::write(harness.worktree.join("occupied"), "partial\n").unwrap();

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "needs_attention");
    assert_eq!(
        fs::read_to_string(harness.worktree.join("occupied")).unwrap(),
        "partial\n"
    );
}

#[test]
fn crash_after_registration_is_fail_closed_when_head_is_absent() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    git(
        &harness.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--",
            harness.worktree.to_str().unwrap(),
            &harness.start,
        ],
    );

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "needs_attention");
    assert!(harness.worktree.exists());
    let listing = git(&harness.repo, &["worktree", "list", "--porcelain"]);
    assert!(listing.contains(&harness.worktree.to_string_lossy().to_string()));
}

fn promoted(harness: &RepoHarness) -> ReconcileOutcome {
    prepare_and_mutate(harness);
    add_job_worktree(harness);
    harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap()
}

#[test]
fn matching_interrupted_allocation_is_promoted_with_canonical_commit() {
    let harness = RepoHarness::new();
    let ReconcileOutcome::Promoted { lease, .. } = promoted(&harness) else {
        panic!("expected promote");
    };
    assert_eq!(lease.start_commit, harness.start);
    assert_eq!(lease.requested_start_point, "refs/heads/main");
    assert_eq!(lease.allocation_state, AllocationState::Active);
    assert_eq!(lease.mode, LeaseMode::WriterLocked);
}

#[test]
fn matching_promoted_inspection_uses_canonical_commit() {
    let harness = RepoHarness::new();
    let ReconcileOutcome::Promoted { inspection, .. } = promoted(&harness) else {
        panic!("expected promote");
    };
    assert_eq!(
        inspection.head_commit.as_deref(),
        Some(harness.start.as_str())
    );
    assert_eq!(inspection.classification, EvidenceClass::Matching);
    assert!(!inspection.ttl_expired);
}

#[test]
fn conflicting_head_stays_fail_closed() {
    let harness = RepoHarness::new();
    prepare_and_mutate(&harness);
    add_job_worktree(&harness);
    let moved = later_commit(&harness.repo);
    git(
        &harness.repo,
        &["update-ref", "refs/heads/hive/job-1", &moved],
    );

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "needs_attention");
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        moved
    );
    assert!(harness.worktree.exists());
}

#[test]
fn concurrent_reconcile_cannot_resurrect_tombstoned_lease() {
    let harness = RepoHarness::new();
    prepare_and_mutate(&harness);
    add_job_worktree(&harness);
    let other = LeaseStore::open(harness.store.path()).unwrap();
    other.tombstone_by_path(&harness.worktree).unwrap();

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "tombstoned");
    let lease = harness.store.find_job(harness.key()).unwrap().unwrap();
    assert_eq!(lease.allocation_state, AllocationState::Tombstoned);
    assert!(matches!(
        harness.store.prepare_allocate(harness.request()),
        Err(Error::PolicyViolation {
            code: PolicyCode::LeaseTombstoned,
            ..
        })
    ));
}

#[test]
fn concurrent_reconcile_cannot_resurrect_released_lease() {
    let harness = RepoHarness::new();
    let operation_id = prepare_and_mutate(&harness);
    add_job_worktree(&harness);
    harness.store.commit_allocate(&operation_id).unwrap();
    let other = LeaseStore::open(harness.store.path()).unwrap();
    other.release_by_path(&harness.worktree).unwrap();

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.as_str(), "released");
    assert!(matches!(
        harness.store.prepare_allocate(harness.request()),
        Err(Error::PolicyViolation {
            code: PolicyCode::LeaseReleased,
            ..
        })
    ));
}
