use super::*;

#[test]
fn regrant_clears_stale_coord_claim() {
    let harness = Harness::new();
    let worktree = harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    harness.store.release_by_path(&worktree).unwrap();
    harness
        .store
        .grant(crate::lease::LeaseGrant {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &worktree,
            start_commit: &harness.start,
        })
        .unwrap();
    let claimed = announce(&harness.store, "job-a", "agent-b", &[]);
    assert_eq!(claimed.claim.agent_id, "agent-b");
}

#[test]
fn active_reregister_refreshes_coord_claim_checkout_identity() {
    let harness = Harness::new();
    let worktree = harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);

    harness
        .store
        .grant(crate::lease::LeaseGrant {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/job-renamed",
            worktree_path: &worktree,
            start_commit: &harness.start,
        })
        .unwrap();

    let claim = harness.store.find_claim(job_key_a()).unwrap().unwrap();
    assert_eq!(claim.branch, "hive/job-renamed");
    assert_eq!(claim.worktree_path, worktree.to_string_lossy());
}
