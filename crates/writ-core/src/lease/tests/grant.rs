use super::*;
use crate::error::{Error, PolicyCode};
use rusqlite::params;

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
    assert!(
        store.list_live().unwrap().is_empty(),
        "released lease must not list as live"
    );
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
    assert_eq!(resume.requested_start_point, "refs/heads/hive/gh-42");
}

#[test]
fn released_lease_can_be_tombstoned_and_cannot_be_granted_again() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
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

    store.grant(grant).unwrap();
    let released = store.release_by_path(&wt).unwrap().unwrap();
    assert_eq!(released.allocation_state, AllocationState::Released);

    let tombstoned = store.tombstone_by_path(&wt).unwrap().unwrap();
    assert_eq!(tombstoned.allocation_state, AllocationState::Tombstoned);
    assert!(tombstoned.released_at.is_some());
    assert!(tombstoned.tombstoned_at.is_some());

    assert_policy(store.grant(grant).unwrap_err(), PolicyCode::LeaseTombstoned);
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

#[test]
fn detached_prepare_stores_head_not_a_heads_ref() {
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
    assert_eq!(prepared.branch, "(detached)");
    assert_eq!(prepared.branch_ref, "HEAD");
    assert_ne!(prepared.branch_ref, "refs/heads/(detached)");
}

#[test]
fn list_live_includes_interrupted_and_excludes_released() {
    let tmp = tempdir().unwrap();
    let store = LeaseStore::open(tmp.path().join("leases.db")).unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("worktrees/acme/sample/gh-42");
    store
        .grant(LeaseGrant {
            repo: &repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "gh-42",
            branch: "hive/gh-42",
            worktree_path: &wt,
            start_commit: "abc123",
        })
        .unwrap();
    rusqlite::Connection::open(store.path())
        .unwrap()
        .execute("UPDATE leases SET allocation_state = 'PREPARED'", [])
        .unwrap();
    assert!(store.list_active().unwrap().is_empty());
    assert_eq!(store.list_live().unwrap().len(), 1);
}
