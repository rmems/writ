use super::*;
use crate::error::PolicyCode;
use std::fs;

#[test]
fn needs_attention_can_abort_after_residual_state_is_removed() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    fs::create_dir_all(&harness.worktree).unwrap();
    fs::write(harness.worktree.join("occupied"), "partial\n").unwrap();
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );

    fs::remove_dir_all(&harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, "retry");
    assert!(harness.store.find_job(harness.key()).unwrap().is_none());
}

#[test]
fn crash_after_branch_creation_is_fail_closed_without_cleanup() {
    let harness = RepoHarness::new();
    harness.store.prepare_allocate(harness.request()).unwrap();
    harness
        .store
        .mark_mutating(
            &harness
                .store
                .find_job(harness.key())
                .unwrap()
                .unwrap()
                .operation_id,
        )
        .unwrap();
    git(&harness.repo, &["branch", "hive/job-1", &harness.start]);
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
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
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
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
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert!(harness.worktree.exists());
    let listing = git(&harness.repo, &["worktree", "list", "--porcelain", "-z"]);
    assert!(
        classify::porcelain_lists_worktree(listing.as_bytes(), &harness.worktree),
        "fail-closed reconcile must keep the registered worktree; listing={listing:?} expected={:?}",
        harness.worktree
    );
}

#[test]
fn matching_interrupted_allocation_is_promoted_with_canonical_commit() {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let outcome = assert_outcome(&harness.store, harness.key(), &harness.repo, "promoted");
    let ReconcileOutcome::Promoted { lease, inspection } = outcome else {
        panic!("expected promote, got {outcome:?}");
    };
    assert_eq!(lease.start_commit, harness.start);
    assert_eq!(lease.requested_start_point, "refs/heads/main");
    assert_eq!(lease.allocation_state, AllocationState::Active);
    assert_eq!(lease.mode, LeaseMode::WriterLocked);
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
    harness.prepare_and_add_worktree();
    let moved = later_commit(&harness.repo);
    git(
        &harness.repo,
        &["update-ref", "refs/heads/hive/job-1", &moved],
    );
    assert_outcome(
        &harness.store,
        harness.key(),
        &harness.repo,
        "needs_attention",
    );
    assert_eq!(
        git(&harness.repo, &["rev-parse", "refs/heads/hive/job-1"]),
        moved
    );
    assert!(harness.worktree.exists());
}

fn refuse_after_terminal(
    finalize: fn(&LeaseStore, &std::path::Path) -> crate::error::Result<Option<Lease>>,
    expected: &str,
    code: PolicyCode,
) {
    let harness = RepoHarness::new();
    harness.prepare_and_add_worktree();
    let other = LeaseStore::open(harness.store.path()).unwrap();
    finalize(&other, &harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, expected);
    let err = harness
        .store
        .prepare_allocate(harness.request())
        .unwrap_err();
    assert_policy(err, code);
}

#[test]
fn concurrent_reconcile_cannot_resurrect_tombstoned_lease() {
    refuse_after_terminal(
        LeaseStore::tombstone_by_path,
        "tombstoned",
        PolicyCode::LeaseTombstoned,
    );
}

#[test]
fn concurrent_reconcile_cannot_resurrect_released_lease() {
    let harness = RepoHarness::new();
    let prepared = harness.prepare_and_add_worktree();
    harness
        .store
        .commit_allocate(&prepared.operation_id)
        .unwrap();
    let other = LeaseStore::open(harness.store.path()).unwrap();
    other.release_by_path(&harness.worktree).unwrap();
    assert_outcome(&harness.store, harness.key(), &harness.repo, "released");
    let err = harness
        .store
        .prepare_allocate(harness.request())
        .unwrap_err();
    assert_policy(err, PolicyCode::LeaseReleased);
}
