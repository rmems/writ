//! Handoff lifecycle: declared-vs-actual scope at offer time, single live
//! owner after ACK, restart durability, and concurrent ACK races (#111/#112).

use super::*;

fn declared(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| String::from(*p)).collect()
}

fn write_commit(worktree: &Path, rel: &str, body: &str, message: &str) {
    let file = worktree.join(rel);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, body).unwrap();
    git(worktree, &["add", "--", rel]);
    git(worktree, &["commit", "-q", "-m", message]);
}

fn write_only(worktree: &Path, rel: &str, body: &str) {
    let file = worktree.join(rel);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, body).unwrap();
}

#[test]
fn scope_drift_separates_undeclared_changes_from_untouched_declarations() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(
        &harness.store,
        "job-a",
        "agent-a",
        &declared(&["src/a", "docs"]),
    );
    write_commit(&worktree, "src/a/x.rs", "fn a() {}", "declared change");
    write_commit(&worktree, "src/b/y.rs", "fn b() {}", "undeclared change");
    write_only(&worktree, "notes.txt", "untracked scratch");
    write_only(&worktree, "src/a/x.rs", "fn a() { /* edited */ }");

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(drift.start_commit, harness.start);
    assert_eq!(drift.declared_paths, declared(&["src/a", "docs"]));
    assert_eq!(
        drift.changed_paths,
        declared(&["notes.txt", "src/a/x.rs", "src/b/y.rs"])
    );
    assert_eq!(
        drift.undeclared_paths,
        declared(&["notes.txt", "src/b/y.rs"])
    );
    assert_eq!(drift.untouched_declared_paths, declared(&["docs"]));
    assert_eq!(
        (drift.agent_id.as_str(), drift.branch.as_str()),
        ("agent-a", "hive/job-a")
    );
}

#[test]
fn scope_drift_is_clean_when_changes_stay_inside_the_declaration() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));
    write_commit(&worktree, "src/lib.rs", "pub fn x() {}", "in scope");

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(drift.changed_paths, declared(&["src/lib.rs"]));
    assert!(drift.undeclared_paths.is_empty());
    assert!(drift.untouched_declared_paths.is_empty());
}

#[test]
fn scope_drift_reports_a_rename_as_both_paths() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(
        &worktree,
        "src/b/x.rs",
        "fn x() {}",
        "lands outside the declaration",
    );
    fs::create_dir_all(worktree.join("src/a")).unwrap();
    git(&worktree, &["mv", "src/b/x.rs", "src/a/x.rs"]);
    git(&worktree, &["commit", "-q", "-m", "move it inside"]);

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(drift.changed_paths, declared(&["src/a/x.rs", "src/b/x.rs"]));
    assert_eq!(drift.undeclared_paths, declared(&["src/b/x.rs"]));
}

#[test]
fn scope_drift_ignores_upstream_commits_merged_into_the_job_branch() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(&worktree, "src/a/x.rs", "fn a() {}", "job work");
    write_commit(
        &harness.repo,
        "src/upstream/u.rs",
        "fn u() {}",
        "peer work on main",
    );
    git(&worktree, &["merge", "--no-edit", "main"]);
    assert!(worktree.join("src/upstream/u.rs").exists());

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(drift.changed_paths, declared(&["src/a/x.rs"]));
    assert!(drift.undeclared_paths.is_empty());
}

#[test]
fn scope_drift_ignores_upstream_commits_after_a_rebase_onto_a_fetched_base() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(&worktree, "src/a/x.rs", "fn a() {}", "job work");
    write_commit(
        &harness.repo,
        "src/upstream/u.rs",
        "fn u() {}",
        "peer work on main",
    );
    let remote = harness._temp.path().join("origin.git");
    git(
        &harness.repo,
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        &harness.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&harness.repo, &["push", "-q", "origin", "main"]);
    git(&worktree, &["push", "-q", "origin", "hive/job-a"]);
    git(&worktree, &["rebase", "-q", "main"]);
    assert!(worktree.join("src/upstream/u.rs").exists());

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    // The upstream commit is on first-parent history now, but it is reachable
    // from origin/main; the job's own pushed commit on origin/hive/job-a is
    // not treated as upstream.
    assert_eq!(drift.changed_paths, declared(&["src/a/x.rs"]));
    assert!(drift.undeclared_paths.is_empty());
}

#[test]
fn scope_drift_keeps_job_commits_when_a_stacked_child_branch_is_pushed() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(&worktree, "src/b/x.rs", "fn b() {}", "undeclared job work");
    let remote = harness._temp.path().join("origin.git");
    git(
        &harness.repo,
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        &harness.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&worktree, &["push", "-q", "origin", "hive/job-a"]);
    // A stacked child pushed under another name contains job-a's commits.
    git(&worktree, &["branch", "hive/job-b", "hive/job-a"]);
    git(&worktree, &["push", "-q", "origin", "hive/job-b"]);
    // A stale copy of the branch pushed under another name does too.
    git(
        &worktree,
        &["push", "-q", "origin", "hive/job-a:refs/heads/backup/job-a"],
    );

    let drift = harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(drift.changed_paths, declared(&["src/b/x.rs"]));
    assert_eq!(drift.undeclared_paths, declared(&["src/b/x.rs"]));
}

#[test]
fn scope_drift_counts_a_fast_forward_until_the_checkout_is_reregistered() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(
        &harness.repo,
        "src/upstream/u.rs",
        "fn u() {}",
        "peer work on main",
    );
    git(&worktree, &["merge", "--ff-only", "main"]);

    // Without a remote-tracking ref for main, a fast-forward (like a rebase
    // onto local-only commits) moves the branch base and the replayed
    // upstream commit is counted.
    let drift = harness.store.scope_drift(job_key_a()).unwrap();
    assert_eq!(drift.undeclared_paths, declared(&["src/upstream/u.rs"]));

    // Re-registering the checkout refreshes the lease start commit to HEAD.
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    harness
        .store
        .grant(crate::lease::LeaseGrant {
            repo: &harness.repo,
            owner: "acme",
            repo_name: "sample",
            job_id: "job-a",
            branch: "hive/job-a",
            worktree_path: &worktree,
            start_commit: &head,
        })
        .unwrap();
    let drift = harness.store.scope_drift(job_key_a()).unwrap();
    assert_eq!(drift.start_commit, head);
    assert!(drift.changed_paths.is_empty());
}

#[test]
fn handoff_compares_against_the_declaration_locked_with_the_offer() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_only(&worktree, "src/b/y.rs", "fn b() {}");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));

    let offer = sample_handoff(&harness.store, "declaration widened first", Some("job-a"));

    assert!(offer.paths.is_empty(), "got {:?}", offer.paths);
}

#[test]
fn scope_drift_inspection_is_read_only() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    write_only(&worktree, "wip.txt", "keep me");
    let head_before = git(&worktree, &["rev-parse", "HEAD"]);

    harness.store.scope_drift(job_key_a()).unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        fs::read_to_string(worktree.join("wip.txt")).unwrap(),
        "keep me"
    );
    assert!(git(&worktree, &["diff", "--cached", "--name-only"]).is_empty());
}

#[test]
fn scope_drift_fails_loudly_without_an_inspectable_checkout() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));

    let err = harness.store.scope_drift(job_key_a()).unwrap_err();
    assert!(
        err.to_string().contains("inspect changed paths"),
        "got {err}"
    );

    let missing = harness
        .store
        .scope_drift(JobKey {
            owner: "acme",
            repo_name: "sample",
            job_id: "job-none",
        })
        .unwrap_err();
    assert!(
        missing.to_string().contains("no coordination claim"),
        "got {missing}"
    );
}

#[test]
fn handoff_offer_carries_changed_paths_outside_the_declaration() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src/a"]));
    write_commit(&worktree, "src/a/x.rs", "fn a() {}", "declared");
    write_only(&worktree, "src/b/y.rs", "fn b() {}");

    let offer = sample_handoff(&harness.store, "taking a break", Some("job-a"));

    assert_eq!(offer.kind, MessageKind::Handoff);
    assert_eq!(offer.paths, declared(&["src/b/y.rs"]));
    let inbox = harness.store.inbox(job_key_a()).unwrap();
    let stored = inbox.iter().find(|m| m.id == offer.id).unwrap();
    assert_eq!(stored.paths, declared(&["src/b/y.rs"]));
}

#[test]
fn handoff_offer_still_posts_when_the_checkout_cannot_be_inspected() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));

    let offer = sample_handoff(&harness.store, "plain directory", Some("job-a"));

    assert!(offer.paths.is_empty());
    let peer = LeaseStore::open(&harness.path).unwrap();
    let (_, transferred) = sample_ack(&peer, offer.id).unwrap();
    assert_eq!(transferred.unwrap().agent_id, "agent-b");
}

fn transfer_job_a(harness: &Harness) -> (CoordClaim, CoordClaim) {
    let before = harness.store.find_claim(job_key_a()).unwrap().unwrap();
    let offer = sample_handoff(&harness.store, "transfer", Some("job-a"));
    let peer = LeaseStore::open(&harness.path).unwrap();
    let (_, transferred) = sample_ack(&peer, offer.id).unwrap();
    (
        before,
        transferred.expect("handoff ACK transfers the claim"),
    )
}

#[test]
fn accepted_transfer_leaves_exactly_one_live_owner_and_keeps_checkout_identity() {
    let harness = Harness::new();
    let worktree = harness.seed_checkout_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));
    write_only(&worktree, "wip.txt", "uncommitted work");
    let head = git(&worktree, &["rev-parse", "HEAD"]);

    let (before, after) = transfer_job_a(&harness);

    let live = harness
        .store
        .list_claims()
        .unwrap()
        .into_iter()
        .filter(|claim| claim.job_id == "job-a")
        .collect::<Vec<_>>();
    assert_eq!(live.len(), 1, "one live owner after transfer");
    assert_eq!(
        (live[0].agent_id.as_str(), live[0].owner_generation),
        ("agent-b", before.owner_generation + 1)
    );
    assert_eq!(after.branch, before.branch);
    assert_eq!(after.worktree_path, before.worktree_path);
    assert_eq!(after.declared_paths, before.declared_paths);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(worktree.join("wip.txt")).unwrap(),
        "uncommitted work"
    );
}

#[test]
fn previous_owner_cannot_announce_offer_or_send_after_transfer() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));
    let (before, _) = transfer_job_a(&harness);

    assert_held(harness.store.announce(AnnounceRequest {
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
        agent_id: "agent-a",
        session_id: Some("session-a"),
        agent_type: "worker",
        intent: Some("resume"),
        paths: &[],
    }));
    let stale_offer = harness.store.propose_handoff(HandoffRequest {
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
        from_agent_id: "agent-a",
        to_agent_id: "agent-c",
        to_job_id: Some("job-a"),
        expected_generation: Some(before.owner_generation),
        body: "stale source",
    });
    match stale_offer {
        Err(Error::PolicyViolation {
            code: PolicyCode::CoordClaimHeld,
            ..
        }) => {}
        other => panic!("previous owner must not re-offer, got {other:?}"),
    }
    let send = harness.store.send_message(SendRequest {
        owner: "acme",
        repo_name: "sample",
        job_id: "job-a",
        agent_id: "agent-a",
        kind: MessageKind::Help,
        body: "still here?",
        to_agent_id: None,
        to_owner: None,
        to_repo_name: None,
        to_job_id: None,
        paths: &[],
        ack_of: None,
    });
    assert!(send.is_err(), "previous owner must not send as owner");
}

#[test]
fn pending_and_accepted_handoff_state_survive_store_reopen() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &declared(&["src"]));
    let offer = sample_handoff(&harness.store, "offer before restart", Some("job-a"));
    let path = harness.path.clone();
    drop(harness.store);

    // Pending offer is visible and still actionable after reopen.
    let reopened = LeaseStore::open(&path).unwrap();
    let pending = reopened
        .inbox(job_key_a())
        .unwrap()
        .into_iter()
        .find(|m| m.id == offer.id)
        .expect("pending handoff survives reopen");
    assert!(pending.acked_at.is_none());
    let claim = reopened.find_claim(job_key_a()).unwrap().unwrap();
    assert_eq!(
        (claim.agent_id.as_str(), claim.owner_generation),
        ("agent-a", 1)
    );
    let (_, transferred) = sample_ack(&reopened, offer.id).unwrap();
    assert_eq!(transferred.unwrap().agent_id, "agent-b");
    drop(reopened);

    // Accepted state is durable and the consumed offer cannot be replayed.
    let again = LeaseStore::open(&path).unwrap();
    let claim = again.find_claim(job_key_a()).unwrap().unwrap();
    assert_eq!(
        (
            claim.agent_id.as_str(),
            claim.session_id.as_deref(),
            claim.owner_generation,
            claim.paused_at
        ),
        ("agent-b", Some("session-b"), 2, None)
    );
    let inbox = again.inbox(job_key_a()).unwrap();
    assert!(
        inbox
            .iter()
            .any(|m| m.id == offer.id && m.acked_at.is_some())
    );
    assert!(
        inbox
            .iter()
            .any(|m| m.kind == MessageKind::Ack && m.ack_of == Some(offer.id))
    );
    let replay = sample_ack(&again, offer.id).unwrap_err();
    assert!(
        replay.to_string().contains("already acknowledged"),
        "got {replay}"
    );
}

#[test]
fn concurrent_acks_of_one_handoff_transfer_exactly_once() {
    let harness = Harness::new();
    harness.seed_job("job-a", "hive/job-a");
    announce(&harness.store, "job-a", "agent-a", &[]);
    let offer = sample_handoff(&harness.store, "race me", Some("job-a"));

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let racers = (0..2)
        .map(|_| {
            let path = harness.path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                let peer = LeaseStore::open(&path).unwrap();
                barrier.wait();
                sample_ack(&peer, offer.id).map(|(_, claim)| claim)
            })
        })
        .collect::<Vec<_>>();
    let outcomes = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect::<Vec<_>>();

    let wins = outcomes.iter().filter(|o| o.is_ok()).count();
    assert_eq!(wins, 1, "exactly one ACK may transfer, got {outcomes:?}");
    let claim = harness.store.find_claim(job_key_a()).unwrap().unwrap();
    assert_eq!(
        (claim.agent_id.as_str(), claim.owner_generation),
        ("agent-b", 2)
    );
    let acks = harness
        .store
        .inbox(job_key_a())
        .unwrap()
        .into_iter()
        .filter(|m| m.kind == MessageKind::Ack && m.ack_of == Some(offer.id))
        .count();
    assert_eq!(acks, 1);
}
