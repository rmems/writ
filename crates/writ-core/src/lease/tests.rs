
use super::*;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

struct RepoHarness {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    store: LeaseStore,
    worktree: PathBuf,
    start: String,
}

impl RepoHarness {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
        let start = git(&repo, &["rev-parse", "HEAD"]);
        let store = LeaseStore::open(temp.path().join("leases.db")).unwrap();
        let worktree = temp.path().join("worktrees/acme/sample/job-1");
        Self {
            _temp: temp,
            repo,
            store,
            worktree,
            start,
        }
    }

    fn request(&self) -> AllocateRequest<'_> {
        AllocateRequest {
            repo: &self.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "hive/job-1",
            worktree_path: &self.worktree,
            requested_start_point: "refs/heads/main",
            start_commit: &self.start,
            ttl: None,
        }
    }

    fn key(&self) -> JobKey<'_> {
        JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
        }
    }

    fn inspect_req(&self) -> InspectRequest<'_> {
        InspectRequest {
            repo_root: &self.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &self.worktree,
            branch: Some("hive/job-1"),
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn later_commit(repo: &Path) -> String {
    git(repo, &["commit", "--allow-empty", "-m", "later"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[test]
fn crash_before_mutation_is_retryable_without_reserving_identity() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
    assert_eq!(prepared.allocation_state, AllocationState::Prepared);
    assert_eq!(prepared.requested_start_point, "refs/heads/main");
    assert_eq!(prepared.start_commit, harness.start);
    assert_ne!(prepared.requested_start_point, prepared.start_commit);

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
    assert!(
        classify::porcelain_lists_worktree(&listing, &harness.worktree),
        "fail-closed reconcile must keep the registered worktree; listing={listing:?} expected={:?}",
        harness.worktree
    );
}

#[test]
fn matching_interrupted_allocation_is_promoted_with_canonical_commit() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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

    let outcome = harness
        .store
        .reconcile(harness.key(), &harness.repo)
        .unwrap()
        .unwrap();
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
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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
fn persisted_record_distinguishes_symbolic_resolved_and_operation() {
    let harness = RepoHarness::new();
    let lease = harness.store.prepare_allocate(harness.request()).unwrap();
    let inspection = harness.store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(
        inspection.requested_start_point.as_deref(),
        Some("refs/heads/main")
    );
    assert_eq!(
        inspection.resolved_start_commit.as_deref(),
        Some(harness.start.as_str())
    );
    assert_eq!(
        inspection.operation_id.as_deref(),
        Some(lease.operation_id.as_str())
    );
    assert_ne!(
        inspection.requested_start_point.as_deref(),
        inspection.resolved_start_commit.as_deref()
    );
}

#[test]
fn concurrent_reconcile_cannot_resurrect_tombstoned_lease() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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
    assert!(orphaned.lease_present);
    assert!(orphaned.ttl_expired);
    assert!(orphaned.path_exists);

    let missing_store = LeaseStore::open(harness.store.path().with_file_name("other.db")).unwrap();
    let missing = missing_store.inspect(harness.inspect_req()).unwrap();
    assert_eq!(missing.classification, EvidenceClass::MissingLease);
    assert!(!missing.lease_present);
    assert!(missing.path_exists);
    assert!(!missing.ttl_expired);
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
fn fix_cycles_crash_before_mutation_does_not_count() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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

    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    assert_eq!(token.authorized, 1);
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), None)
        .unwrap();
    assert_eq!(recovered, FixCycleReconcile::Aborted { fix_cycles: 0 });
    assert_eq!(
        harness
            .store
            .find_job(harness.key())
            .unwrap()
            .unwrap()
            .fix_cycles,
        Some(0)
    );
}

#[test]
fn fix_cycles_crash_after_proven_mutation_is_not_lost() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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

    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    harness
        .store
        .mark_fix_cycle_mutating(&token.operation_id)
        .unwrap();
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), Some(true))
        .unwrap();
    assert_eq!(recovered, FixCycleReconcile::Committed { fix_cycles: 1 });
    let again = harness
        .store
        .reconcile_fix_cycle(harness.key(), Some(true))
        .unwrap();
    assert_eq!(again, FixCycleReconcile::Idle { fix_cycles: 1 });
}

#[test]
fn fix_cycles_mutate_without_proof_stays_needs_attention() {
    let harness = RepoHarness::new();
    let prepared = harness.store.prepare_allocate(harness.request()).unwrap();
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
    let token = harness.store.prepare_fix_cycle(harness.key()).unwrap();
    harness
        .store
        .mark_fix_cycle_mutating(&token.operation_id)
        .unwrap();
    let recovered = harness
        .store
        .reconcile_fix_cycle(harness.key(), None)
        .unwrap();
    assert!(matches!(
        recovered,
        FixCycleReconcile::NeedsAttention { pending: 1, .. }
    ));
    assert_eq!(
        harness
            .store
            .find_job(harness.key())
            .unwrap()
            .unwrap()
            .fix_cycles,
        Some(0)
    );
}

// Exercises the `same_existing_path` tolerance inside `inspect_git`'s
// `worktree_registered` check (the path-equality fix from this PR). git
// reports the OS-canonical real worktree path, while the stored lease path
// reaches the same directory through a symlinked prefix (the macOS
// `/var -> /private/var` situation). Under the old exact-string comparison
// these differ and the worktree would be reported unregistered, wrongly
// classifying a clean allocation as `NeedsAttention`. This asserts the
// integration-level behaviour, not just the helper.
#[cfg(unix)]
#[test]
fn worktree_registered_tolerates_symlinked_stored_path() {
    let harness = RepoHarness::new();

    // Create and register the worktree at its real (non-symlinked) path.
    fs::create_dir_all(harness.worktree.parent().unwrap()).unwrap();
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

    // Build an alias that reaches the same directory via a symlink, so the
    // stored path differs from git's reported path only by canonicalization.
    let real_parent = harness.worktree.parent().unwrap();
    let alias_parent = harness._temp.path().join("aliased-worktrees");
    std::os::unix::fs::symlink(real_parent, &alias_parent).unwrap();
    let aliased_worktree = alias_parent.join("job-1");
    assert_ne!(aliased_worktree, harness.worktree);

    let evidence = classify::inspect_git(&harness.repo, &aliased_worktree, "hive/job-1");
    assert!(
        evidence.worktree_registered,
        "symlinked stored path should still match git's canonical worktree path"
    );

    // And the full inspection should classify the allocation as Matching,
    // proving the tolerance flows through to the reconcile decision.
    harness.store.prepare_allocate(harness.request()).unwrap();
    let inspection = harness
        .store
        .inspect(InspectRequest {
            repo_root: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            worktree_path: &aliased_worktree,
            branch: Some("hive/job-1"),
        })
        .unwrap();
    assert!(inspection.worktree_registered);
    assert_eq!(inspection.classification, EvidenceClass::Matching);
}

#[test]
fn grant_release_preserves_identity_and_budget_columns() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    assert!(store.has_budget_columns().unwrap());

    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("worktrees/acme/sample/gh-42");
    let grant = LeaseGrant {
        repo: &repo,
        owner: "acme",
        repo_name: "sample",
        job_id: "gh-42",
        branch: "hive/gh-42",
        worktree_path: &wt,
        start_commit: "abc123",
    };
    let held = store.grant(grant).unwrap();
    assert_eq!(held.mode, LeaseMode::WriterLocked);
    assert_eq!(held.allocation_state, AllocationState::Active);
    assert_eq!(held.max_files, None);
    assert_eq!(held.fix_cycles, Some(0));
    assert!(held.released_at.is_none());
    assert!(held.row_id > 0);
    assert!(held.created_at > 0);

    let released = store.release_by_path(&wt).unwrap().unwrap();
    assert_eq!(released.mode, LeaseMode::Unassigned);
    assert!(released.released_at.is_some());

    let active = store.list_active().unwrap();
    assert!(active.is_empty(), "released lease must not list as active");
    let all = store.list_all().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].mode, LeaseMode::Unassigned);
    let all = store.list_all().unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].released_at.is_some());

    let resume = store
        .find_resume(ResumeKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "gh-42",
            branch: "hive/gh-42",
        })
        .unwrap()
        .unwrap();
    assert_eq!(resume.start_commit, "abc123");
    assert_eq!(resume.branch_ref, "refs/heads/hive/gh-42");
}

#[test]
fn grant_refuses_active_lease_held_by_different_path() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt_a = tmp.path().join("checkouts/a");
    let wt_b = tmp.path().join("checkouts/b");
    fn grant_for<'a>(repo: &'a Path, wt: &'a Path) -> LeaseGrant<'a> {
        LeaseGrant {
            repo,
            owner: "local",
            repo_name: "repo",
            job_id: "job-1",
            branch: "hive/job-1",
            worktree_path: wt,
            start_commit: "abc123",
        }
    }

    store.grant(grant_for(&repo, &wt_a)).unwrap();

    let err = store.grant(grant_for(&repo, &wt_b)).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::LeaseConflict,
            ..
        }
    ));

    let refreshed = store.grant(grant_for(&repo, &wt_a)).unwrap();
    assert!(refreshed.released_at.is_none());
    assert_eq!(refreshed.worktree_path, path_text(&wt_a));

    store.release_by_path(&wt_a).unwrap();
    let moved = store.grant(grant_for(&repo, &wt_b)).unwrap();
    assert_eq!(moved.worktree_path, path_text(&wt_b));
    assert_eq!(moved.allocation_state, AllocationState::Active);
}

#[test]
fn grant_refuses_active_path_held_by_different_job() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("checkouts/shared");
    let grant_a = LeaseGrant {
        repo: &repo,
        owner: "local",
        repo_name: "repo",
        job_id: "job-a",
        branch: "hive/job-a",
        worktree_path: &wt,
        start_commit: "abc123",
    };
    let grant_b = LeaseGrant {
        job_id: "job-b",
        branch: "hive/job-b",
        ..grant_a
    };

    store.grant(grant_a).unwrap();
    let err = store.grant(grant_b).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::LeaseConflict,
            ..
        }
    ));
    let active = store.list_active().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].job_id, "job-a");

    store.release_by_path(&wt).unwrap();
    let moved = store.grant(grant_b).unwrap();
    assert_eq!(moved.job_id, "job-b");
    assert!(moved.released_at.is_none());

    // Sequential reuse leaves a released row and an active row on the same
    // path. Lookup must return the live holder, not fail closed on
    // query_row's multiple-row error.
    let found = store.find_by_path(&wt).unwrap().unwrap();
    assert_eq!(found.job_id, "job-b");
    assert!(found.released_at.is_none());

    let released = store.release_by_path(&wt).unwrap().unwrap();
    assert_eq!(released.job_id, "job-b");
    assert!(released.released_at.is_some());
}

#[test]
fn agent_upsert_and_retire() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    store
        .upsert_agent(AgentIdentity {
            agent_id: "agent-1",
            agent_type: "Explore",
            session_id: Some("session-1"),
        })
        .unwrap();
    store
        .upsert_agent(AgentIdentity {
            agent_id: "agent-2",
            agent_type: "Worker",
            session_id: None,
        })
        .unwrap();
    store.retire_agent("agent-1").unwrap();
    let agents = store.list_agents().unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].agent_id, "agent-2");
    assert!(agents[0].stopped_at.is_none());
    assert_eq!(agents[1].agent_id, "agent-1");
    assert!(agents[1].stopped_at.is_some());
    let conn = store.conn.lock().unwrap();
    let stopped: Option<i64> = conn
        .query_row(
            "SELECT stopped_at FROM agents WHERE agent_id = ?1",
            params!["agent-1"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stopped.is_some());
}

#[test]
fn standalone_clone_counts_as_registered_without_parent_worktree_list() {
    let temp = tempdir().unwrap();
    let origin = temp.path().join("origin");
    fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-b", "main"]);
    git(&origin, &["config", "user.email", "test@example.com"]);
    git(&origin, &["config", "user.name", "Test User"]);
    git(&origin, &["commit", "--allow-empty", "-m", "initial"]);
    let start = git(&origin, &["rev-parse", "HEAD"]);
    let clone = temp.path().join("standalone");
    git(
        temp.path(),
        &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
    );

    let evidence = classify::inspect_git(&origin, &clone, "main");
    assert!(
        evidence.worktree_registered,
        "a standalone clone is a live checkout even when absent from origin's worktree list"
    );
    assert_eq!(evidence.head_commit.as_deref(), Some(start.as_str()));
}

#[test]
fn unrecognized_mode_is_unknown_not_unassigned() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("checkouts/a");
    store
        .grant(LeaseGrant {
            repo: &repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/a",
            worktree_path: &wt,
            start_commit: "abc123",
        })
        .unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE leases SET mode = 'NOT_A_MODE'", [])
            .unwrap();
    }
    let lease = store
        .find_job(JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
        })
        .unwrap()
        .unwrap();
    assert_eq!(lease.mode, LeaseMode::Unknown);
    assert_eq!(lease.mode_raw, "NOT_A_MODE");
    assert_ne!(lease.mode, LeaseMode::Unassigned);
}
