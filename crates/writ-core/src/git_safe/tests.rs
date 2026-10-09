use super::*;
use crate::error::{Error, PolicyCode};

// ---- git allowlist tests ----

#[test]
fn allowed_subcommand_passes() {
    let cmd = SafeGitCommand::new(&["status".to_owned()]).unwrap();
    assert_eq!(cmd.subcommand(), "status");
}

#[test]
fn push_passes() {
    let cmd = SafeGitCommand::new(&["push".to_owned()]).unwrap();
    assert_eq!(cmd.subcommand(), "push");
}

#[test]
fn unknown_subcommand_rejected() {
    let err = SafeGitCommand::new(&["gc".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn empty_args_rejected() {
    let err = SafeGitCommand::new(&[]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

// ---- merge tests ----

#[test]
fn merge_subcommand_is_allowlisted() {
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "feature".to_owned()]).unwrap();
    assert_eq!(cmd.subcommand(), "merge");
    assert!(cmd.requires_branch_check());
}

#[test]
fn mergetool_subcommand_rejected() {
    let err = SafeGitCommand::new(&["mergetool".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn checkout_branch_named_merge_allowed() {
    // Merge detection is subcommand-only; a branch named "merge" is fine.
    let cmd = SafeGitCommand::new(&["checkout".to_owned(), "merge".to_owned()]).unwrap();
    assert_eq!(cmd.subcommand(), "checkout");
    assert_eq!(cmd.args(), &["checkout", "merge"]);
}

// ---- force push tests ----

#[test]
fn bare_force_rejected() {
    let err = SafeGitCommand::new(&["push".to_owned(), "--force".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn bare_f_flag_rejected() {
    let err = SafeGitCommand::new(&["push".to_owned(), "-f".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn force_with_lease_accepted() {
    let cmd = SafeGitCommand::new(&["push".to_owned(), "--force-with-lease".to_owned()]).unwrap();
    assert_eq!(cmd.subcommand(), "push");
}

#[test]
fn force_with_lease_and_bare_force_rejected() {
    // Bare --force is always rejected, even when --force-with-lease is also present.
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "--force".to_owned(),
        "--force-with-lease".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn force_equals_form_rejected() {
    let err = SafeGitCommand::new(&["push".to_owned(), "--force=true".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn push_mirror_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "--mirror".to_owned(),
        "origin".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn pull_without_rebase_or_ff_only_rejected() {
    let err = SafeGitCommand::new(&["pull".to_owned(), "origin".to_owned(), "main".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn pull_with_rebase_allowed() {
    SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap();
}

#[test]
fn rebase_exec_rejected() {
    let err = SafeGitCommand::new(&[
        "rebase".to_owned(),
        "-x".to_owned(),
        "true".to_owned(),
        "main".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn clone_absolute_dest_rejected() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "https://example.com/r.git".to_owned(),
        "/tmp/outside".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn config_global_rejected() {
    let err = SafeGitCommand::new(&[
        "config".to_owned(),
        "--global".to_owned(),
        "user.name".to_owned(),
        "x".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn push_combined_short_force_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "-fu".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn clone_with_branch_opt_still_rejects_abs_dest() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "-b".to_owned(),
        "main".to_owned(),
        "https://example.com/r.git".to_owned(),
        "/tmp/outside".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn config_attached_short_file_rejected() {
    let err = SafeGitCommand::new(&[
        "config".to_owned(),
        "-f/tmp/outside".to_owned(),
        "user.name".to_owned(),
        "x".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn switch_create_equals_form_target() {
    let args = vec!["switch".to_owned(), "--create=main2".to_owned()];
    assert_eq!(checkout_or_switch_target(&args), Some("main2"));
}

#[test]
fn switch_attached_create_cluster_returns_new_branch() {
    // `switch -cd <point>` creates branch `d`: the start point is not the
    // branch HEAD lands on, so the expected-branch check must compare `d`.
    let args = vec!["switch".to_owned(), "-cd".to_owned(), "feature".to_owned()];
    assert_eq!(checkout_or_switch_target(&args), Some("d"));
    let args = vec!["checkout".to_owned(), "-bfoo".to_owned(), "main".to_owned()];
    assert_eq!(checkout_or_switch_target(&args), Some("foo"));
    // Create letter last: the next argv token is the value.
    let args = vec!["switch".to_owned(), "-tc".to_owned(), "fix".to_owned()];
    assert_eq!(checkout_or_switch_target(&args), Some("fix"));
}

#[test]
fn cluster_create_covers_equals_and_force_create_forms() {
    // `-b`/`-B` (checkout) and `-c`/`-C` (switch) are force-create variants of
    // each other; both take their branch name attached or after `=`.
    for (sub, arg, want) in [
        ("checkout", "-bfoo", "foo"),
        ("checkout", "-b=foo", "foo"),
        ("checkout", "-Bfix", "fix"),
        ("checkout", "-B=fix", "fix"),
        ("switch", "-cfoo", "foo"),
        ("switch", "-c=foo", "foo"),
        ("switch", "-Cfix", "fix"),
        ("switch", "-C=fix", "fix"),
    ] {
        let args = vec![sub.to_owned(), arg.to_owned()];
        assert_eq!(checkout_or_switch_target(&args), Some(want), "{sub} {arg}");
    }
}

#[test]
fn switch_attached_create_cluster_is_pinned_to_new_branch() {
    // `switch -cd <point>` creates branch `d`: the supervisor's branch-pinning
    // check must compare `d` against --expected-branch, not the start point.
    let args = vec!["switch".to_owned(), "-cd".to_owned(), "feature".to_owned()];
    assert!(matches!(
        crate::supervisor::reject_mismatched_checkout(Some("feature"), &args),
        Err(Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        })
    ));
    // Admitted when `d` IS the expected branch.
    crate::supervisor::reject_mismatched_checkout(Some("d"), &args).unwrap();
}

#[test]
fn gh_pr_update_branch_is_remote_policy() {
    SafeGhCommand::new(&[
        "pr".into(),
        "update-branch".into(),
        "1".into(),
        "--rebase".into(),
    ])
    .unwrap();
}

#[test]
fn gh_repo_clone_abs_dest_rejected() {
    let err = SafeGhCommand::new(&[
        "repo".to_owned(),
        "clone".to_owned(),
        "cli/cli".to_owned(),
        "/tmp/outside".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_pr_checkout_rejected() {
    let err =
        SafeGhCommand::new(&["pr".to_owned(), "checkout".to_owned(), "1".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn config_url_scoped_credential_helper_rejected() {
    let err = SafeGitCommand::new(&[
        "config".to_owned(),
        "credential.https://github.com.helper".to_owned(),
        "!true".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn push_prune_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "--prune".to_owned(),
        "origin".to_owned(),
        "refs/heads/*:refs/heads/*".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn clone_c_ssh_command_rejected() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "-c".to_owned(),
        "core.sshCommand=sh -c true".to_owned(),
        "https://example.com/r.git".to_owned(),
        "dst".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn windows_unc_clone_dest_rejected() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "https://example.com/r.git".to_owned(),
        r"\\server\share\out".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn pull_rebase_false_rejected() {
    let err = SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase=false".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn pull_rebase_true_allowed() {
    SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase=true".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap();
}

#[test]
fn pull_rebase_last_option_wins() {
    // `pull --rebase --rebase=false` is a merge pull: git applies the last
    // rebase option, so the earlier --rebase must not admit it.
    let err = SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase".to_owned(),
        "--rebase=false".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));

    // The inverse order is an actual rebase pull.
    SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase=false".to_owned(),
        "--rebase".to_owned(),
    ])
    .unwrap();
}

#[test]
fn pull_no_rebase_last_option_wins() {
    let err = SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase".to_owned(),
        "--no-rebase".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));

    SafeGitCommand::new(&[
        "pull".to_owned(),
        "--no-rebase".to_owned(),
        "--rebase".to_owned(),
    ])
    .unwrap();
}

#[test]
fn rebase_attached_exec_rejected() {
    let err = SafeGitCommand::new(&["rebase".to_owned(), "-xtrue".to_owned(), "main".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn clone_template_opt_still_rejects_abs_dest() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "--template".to_owned(),
        "/tmp/t".to_owned(),
        "https://example.com/r.git".to_owned(),
        "/tmp/outside".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn config_ssh_command_rejected() {
    let err = SafeGitCommand::new(&[
        "config".to_owned(),
        "core.sshCommand".to_owned(),
        "sh -c true".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn push_delete_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "--delete".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn push_delete_refspec_rejected() {
    let err = SafeGitCommand::new(&["push".to_owned(), "origin".to_owned(), ":main".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn push_receive_pack_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "--receive-pack=sh".to_owned(),
        "origin".to_owned(),
        "HEAD".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn checkout_detach_rejected() {
    let err = SafeGitCommand::new(&[
        "checkout".to_owned(),
        "--detach".to_owned(),
        "feature".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }
    ));
}

#[test]
fn checkout_force_in_short_cluster_rejected() {
    // `checkout -fb` smuggles `-f` past the exact-flag check and discards
    // uncommitted work; `-tf` carries the same payload behind a boolean.
    for args in [
        vec!["checkout", "-fb", "feature"],
        vec!["checkout", "-tf", "feature"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let err = SafeGitCommand::new(&args).unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    ..
                }
            ),
            "{args:?}: {err:?}"
        );
    }
}

#[test]
fn switch_force_in_short_cluster_rejected() {
    // `switch -fd` discards worktree changes AND detaches HEAD in one cluster.
    let err = SafeGitCommand::new(&["switch".to_owned(), "-fd".to_owned(), "feature".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn switch_detach_in_short_cluster_rejected() {
    // `-td` hides a detach flag behind a boolean in the same cluster.
    let err = SafeGitCommand::new(&["switch".to_owned(), "-td".to_owned(), "feature".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }
    ));
}

#[test]
fn switch_discard_changes_long_form_rejected() {
    for arg in ["--discard-changes", "--discard-changes=true"] {
        let err = SafeGitCommand::new(&["switch".to_owned(), arg.to_owned(), "feature".to_owned()])
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    ..
                }
            ),
            "{arg}: {err:?}"
        );
    }
}

#[test]
fn switch_create_cluster_letters_are_values_not_detach() {
    // `switch -cd <name>` gives `d` to `-c` as its attached value, so the
    // cluster must not be read as `--detach`.
    SafeGitCommand::new(&["switch".to_owned(), "-cd".to_owned(), "feature".to_owned()]).unwrap();
}

#[test]
fn checkout_pathspec_after_end_of_options_is_not_scanned() {
    // `checkout -- -fd` restores a file literally named `-fd`; args after `--`
    // are pathspecs, never option clusters.
    SafeGitCommand::new(&["checkout".to_owned(), "--".to_owned(), "-fd".to_owned()]).unwrap();
}

#[test]
fn switch_discard_changes_falsy_values_are_noops() {
    // `--discard-changes=<falsy>` disables discarding like a bare boolean
    // negation; only the truthy spellings carry `-f` semantics.
    for arg in ["--discard-changes=false", "--discard-changes=0"] {
        SafeGitCommand::new(&["switch".to_owned(), arg.to_owned(), "feature".to_owned()])
            .unwrap_or_else(|err| panic!("{arg} unexpectedly rejected: {err:?}"));
    }
    let err = SafeGitCommand::new(&[
        "switch".to_owned(),
        "--discard-changes=yes".to_owned(),
        "feature".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

#[test]
fn checkout_branch_cluster_letters_are_values_not_force() {
    // `checkout -bf` gives `f` to `-b` as its attached value, so the cluster
    // must not be read as force; a non-alphabetic cluster tail stops scanning
    // rather than inventing flags.
    SafeGitCommand::new(&[
        "checkout".to_owned(),
        "-bf".to_owned(),
        "feature".to_owned(),
    ])
    .unwrap();
    SafeGitCommand::new(&["checkout".to_owned(), "-1".to_owned(), "feature".to_owned()]).unwrap();
}

#[test]
fn branch_rename_rejected() {
    let err = SafeGitCommand::new(&["branch".to_owned(), "-m".to_owned(), "main".to_owned()])
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }
    ));
}

#[test]
fn gh_repo_selector_extracts_r_flag() {
    let args = vec![
        "pr".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
        "close".to_owned(),
        "1".to_owned(),
    ];
    assert_eq!(gh_repo_selector(&args), Some("other/repo"));
}

#[test]
fn gh_repo_selector_extracts_pflag_spellings() {
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "-R=other/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "-Rother/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "-wR".to_owned(),
            "other/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "-wR=other/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "-wRother/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
    assert_eq!(
        gh_repo_selector(&[
            "pr".to_owned(),
            "--repo=other/repo".to_owned(),
            "view".to_owned()
        ]),
        Some("other/repo")
    );
}

#[test]
fn github_slug_normalize_and_match() {
    assert_eq!(
        normalize_github_repo_slug("https://github.com/Acme/Repo.git").as_deref(),
        Some("acme/repo")
    );
    assert_eq!(
        normalize_github_repo_slug("git@github.com:Acme/Repo.git").as_deref(),
        Some("acme/repo")
    );
    assert!(github_repo_slugs_match("Acme/Repo", "github.com/acme/repo"));
    assert!(!github_repo_slugs_match("Acme/Repo", "other/repo"));
    // Host is part of identity: enterprise origin must not match github.com -R.
    assert!(!github_repo_slugs_match(
        "git@github.enterprise:acme/repo.git",
        "github.com/acme/repo"
    ));
    assert!(github_repo_slugs_match(
        "git@github.enterprise:acme/repo.git",
        "github.enterprise/acme/repo"
    ));
    assert_eq!(github_owner_name("Acme/Repo").as_deref(), Some("acme"));
    assert_eq!(
        github_owner_name("github.com/acme/repo").as_deref(),
        Some("acme")
    );
    assert_eq!(github_owner_name("ACME").as_deref(), Some("acme"));
}

#[test]
fn gh_repo_selector_outside_allowlist_is_rejected_before_run() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(
        &[
            "pr".to_owned(),
            "view".to_owned(),
            "-R".to_owned(),
            "other/repo".to_owned(),
            "1".to_owned(),
        ],
        &allowlist,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_repo_selector_matching_allowlist_is_accepted() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let cmd = SafeGhCommand::with_allowlist(
        &[
            "pr".to_owned(),
            "view".to_owned(),
            "-R".to_owned(),
            "github.com/Acme/Repo".to_owned(),
            "1".to_owned(),
        ],
        &allowlist,
    )
    .unwrap();
    assert_eq!(
        cmd.args(),
        &["pr", "view", "-R", "github.com/Acme/Repo", "1"]
    );
}

#[test]
fn empty_allowlist_denies_gh_repo_selector() {
    let err = SafeGhCommand::with_allowlist(
        &[
            "pr".to_owned(),
            "view".to_owned(),
            "--repo=acme/repo".to_owned(),
            "1".to_owned(),
        ],
        &crate::owners::OwnerAllowlist::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_without_repo_selector_skips_owner_allowlist() {
    let cmd = SafeGhCommand::with_allowlist(
        &["issue".to_owned(), "list".to_owned()],
        &crate::owners::OwnerAllowlist::default(),
    )
    .unwrap();
    assert_eq!(cmd.args(), &["issue", "list"]);
}

#[test]
fn gh_repo_clone_positional_is_enforced_against_allowlist() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(
        &[
            "repo".to_owned(),
            "clone".to_owned(),
            "other/repo".to_owned(),
        ],
        &allowlist,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
    SafeGhCommand::with_allowlist(
        &[
            "repo".to_owned(),
            "clone".to_owned(),
            "acme/repo".to_owned(),
        ],
        &allowlist,
    )
    .unwrap();
}

#[test]
fn gh_repo_delete_positional_is_enforced_against_allowlist() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(
        &[
            "repo".to_owned(),
            "delete".to_owned(),
            "other/project".to_owned(),
            "--yes".to_owned(),
        ],
        &allowlist,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_preverb_values_cannot_hide_command_specific_repository_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec!["pr", "-r", "acme/team", "create", "-f", "-Rother/project"],
        vec!["pr", "-a", "alice", "create", "-f", "-Rother/project"],
        vec!["pr", "--body", "review", "create", "-f", "-Rother/project"],
        vec!["pr", "-r", "review", "create", "-f", "-Rother/project"],
        vec!["pr", "-t", "create", "new", "-f", "-Rother/project"],
        vec!["pr", "--comment=true", "review", "-Rother/project"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            Err("OWNER_NOT_ALLOWED"),
            "{args:?}"
        );
    }
}

#[test]
fn gh_preverb_payloads_and_builtin_aliases_preserve_allowed_targets() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec!["pr", "-r", "other/team", "create", "-Racme/project"],
        vec![
            "pr",
            "-a",
            "https://example.com/a/b",
            "new",
            "-Racme/project",
        ],
        vec!["pr", "--body", "review", "create", "-Racme/project"],
        vec!["pr", "-r", "review", "new", "-Racme/project"],
        vec!["pr", "-t", "create", "new", "-Racme/project"],
        vec!["pr", "--comment=true", "review", "-Racme/project"],
        vec!["pr", "-tRelease", "new", "-Racme/project"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        enforce_gh_repo_targets(&args, &allowlist, None).unwrap();
    }
}

#[test]
fn gh_builtin_aliases_use_canonical_option_arity() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for prefix in [
        vec!["pr", "new", "-f"],
        vec!["issue", "new", "-a", "alice"],
        vec!["gist", "new", "-p"],
        vec!["release", "new", "-p"],
        vec!["repo", "new", "-c"],
        vec!["pr", "ls", "-a", "other/team"],
        vec!["issue", "ls", "-m", "other/project"],
    ] {
        for (target, expected) in [("acme", Ok(())), ("other", Err("OWNER_NOT_ALLOWED"))] {
            let mut args: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
            args.push(format!("-R{target}/project"));
            assert_eq!(
                enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
                expected,
                "{args:?}"
            );
        }
    }
}

#[test]
fn gh_checkout_alias_and_preverb_options_preserve_local_guard() {
    for args in [
        vec!["pr", "co", "1"],
        vec!["pr", "-b", "job", "co", "1"],
        vec!["pr", "-b", "job", "checkout", "1"],
        vec!["pr", "--body", "text", "merge", "1", "--delete-branch"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert!(gh_requires_branch_check(&args), "{args:?}");
        assert_eq!(
            SafeGhCommand::new(&args).unwrap_err().code(),
            "MERGE_BLOCKED"
        );
    }
}

#[test]
fn gh_label_boolean_force_keeps_real_repository_selector() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for verb in ["create", "clone"] {
        let args: Vec<String> = ["label", verb, "name", "-f", "-Rother/project"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            Err("OWNER_NOT_ALLOWED"),
            "{args:?}"
        );
    }
}

#[test]
fn gh_pr_comment_body_urls_are_values_not_repository_targets() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for body in [
        "https://example.com/a/b",
        "https://bad host.example/a/b",
        "other/project",
    ] {
        let args: Vec<String> = ["pr", "comment", "1", "--body", body, "-Racme/project"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        enforce_gh_repo_targets(&args, &allowlist, None).unwrap();
    }
}

#[test]
fn gh_percent_encoded_paths_retain_literal_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (family, target, expected) in [
        (
            "pr",
            "https://github.com/other%2Fproject/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        ("pr", "https://github.com/acme%2fproject/pull/1", Ok(())),
        ("pr", "https://github.com/acme/caf%C3%A9/pull/1", Ok(())),
        (
            "pr",
            "https://github.com/acme%252Fother/project/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "api",
            "https://api.github.com/%72epos/other/project/issues",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "api",
            "https://api.github.com/repos/acme%2Fproject/issues",
            Ok(()),
        ),
        (
            "api",
            "%72epos/other%2fproject/issues",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "api",
            "repos/acme%3Fother/project",
            Err("OWNER_NOT_ALLOWED"),
        ),
        ("api", "repos/acme/project%GG", Err("OWNER_NOT_ALLOWED")),
        (
            "api",
            "https://api.github.com/repos/acme/project?next=%GG#%",
            Ok(()),
        ),
    ] {
        let mut args = vec![family.to_owned()];
        if family == "pr" {
            args.push("merge".into());
        }
        args.push(target.into());
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn gh_url_path_decoding_rejects_malformed_bytes_without_decoding_query() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (target, expected) in [
        (
            "https://github.com/acme/project%/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "https://github.com/acme/project%2/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "https://github.com/acme/project%GG/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            "https://github.com/acme/project%FF/pull/1",
            Err("OWNER_NOT_ALLOWED"),
        ),
        ("https://github.com/acme/project/pull/1?next=%GG#%", Ok(())),
    ] {
        let args = vec!["pr".into(), "view".into(), target.into()];
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn gh_repo_sync_checks_destination_instead_of_source_payload() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for flag in ["-s", "--source"] {
        for (source, destination, expected) in [
            ("acme/source", "other/dest", Err("OWNER_NOT_ALLOWED")),
            ("other/source", "acme/dest", Ok(())),
            ("sync", "other/dest", Err("OWNER_NOT_ALLOWED")),
        ] {
            let args: Vec<String> = ["repo", "sync", flag, source, destination, "--force"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            assert_eq!(
                enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
                expected,
                "{args:?}"
            );
        }
    }
}

#[test]
fn gh_repo_preverb_homepage_keeps_destination_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for verb in ["create", "new", "edit"] {
        for (destination, expected) in [
            ("other/project", Err("OWNER_NOT_ALLOWED")),
            ("acme/project", Ok(())),
        ] {
            let args: Vec<String> = [
                "repo",
                "-h",
                "https://example.com/other/site",
                verb,
                destination,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            assert_eq!(
                enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
                expected,
                "{args:?}"
            );
        }
    }
}

#[test]
fn gh_homepage_arity_preserves_long_forms_and_help_controls() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (args, expected) in [
        (
            vec!["repo", "-hhttps://example.com/site", "new", "other/project"],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec!["repo", "-hR", "create", "other/project"],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec![
                "repo",
                "--homepage",
                "https://example.com/site",
                "create",
                "other/project",
            ],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec![
                "repo",
                "create",
                "-h",
                "https://example.com/site",
                "other/project",
            ],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (vec!["repo", "--help"], Ok(())),
        (vec!["repo", "-h"], Ok(())),
        (vec!["repo", "--help", "create", "acme/project"], Ok(())),
        (
            vec!["pr", "-h", "view", "feature/foo", "-Racme/project"],
            Ok(()),
        ),
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn gh_repo_destinations_use_shared_operands_and_builtin_aliases() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (args, expected) in [
        (
            vec!["repo", "new", "other/project"],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec!["repo", "--description", "create", "new", "other/project"],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec![
                "repo",
                "create",
                "--description",
                "https://example.com/other/project",
                "acme/project",
            ],
            Ok(()),
        ),
        (
            vec![
                "repo",
                "sync",
                "-b",
                "other/branch",
                "-sacme/source",
                "other/dest",
            ],
            Err("OWNER_NOT_ALLOWED"),
        ),
        (
            vec!["repo", "sync", "--source=other/source", "acme/dest"],
            Ok(()),
        ),
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn gh_pr_slash_branch_operands_are_not_repository_selectors() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for verb in [
        "merge",
        "ready",
        "update-branch",
        "view",
        "checks",
        "diff",
        "comment",
        "review",
        "close",
        "reopen",
    ] {
        for branch in ["feature/foo", "feature/foo#123"] {
            let args: Vec<String> = ["pr", verb, branch, "-Racme/project"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            enforce_gh_repo_targets(&args, &allowlist, None).unwrap();
        }
    }
}

#[test]
fn gh_pr_branch_admission_keeps_explicit_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec!["pr", "merge", "feature/foo", "-Rother/project"],
        vec![
            "pr",
            "view",
            "https://github.com/other/project/pull/1",
            "-Racme/project",
        ],
        vec![
            "pr",
            "merge",
            "feature/foo",
            "-Rother/project",
            "-Racme/project",
        ],
        vec!["issue", "transfer", "1", "other/project", "-Racme/source"],
        vec![
            "issue",
            "transfer",
            "https://github.com/other/source/issues/1",
            "acme/target",
            "-Racme/source",
        ],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            Err("OWNER_NOT_ALLOWED"),
            "{args:?}"
        );
    }
}

#[test]
fn gh_issue_transfer_checks_every_real_repository_operand() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (destination, expected) in [
        ("target", Ok(())),
        ("acme/target", Ok(())),
        ("other/target", Err("OWNER_NOT_ALLOWED")),
    ] {
        let args: Vec<String> = ["issue", "transfer", "1", destination, "-Racme/source"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            expected,
            "{args:?}"
        );
    }
}

#[test]
fn gh_repo_selector_respects_command_specific_boolean_flags() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for prefix in [
        vec!["auth", "login", "-c"],
        vec!["auth", "refresh", "-c"],
        vec!["auth", "setup-git", "-f"],
        vec!["auth", "status", "-t"],
        vec!["browse", "-p"],
        vec!["browse", "-c"],
        vec!["gist", "create", "-p"],
        vec!["issue", "develop", "-c"],
        vec!["label", "clone", "-f"],
        vec!["label", "create", "-f"],
        vec!["pr", "checkout", "-f"],
        vec!["pr", "create", "-f"],
        vec!["pr", "review", "-c"],
        vec!["pr", "review", "--comment"],
        vec!["pr", "status", "-c"],
        vec!["pr", "view", "-c"],
        vec!["issue", "view", "-c"],
        vec!["release", "create", "v1", "-p"],
        vec!["release", "create", "v1", "--prerelease"],
        vec!["repo", "create", "-c"],
        vec!["repo", "edit", "--template"],
        vec!["workflow", "run", "--json"],
    ] {
        for (owner, expected) in [("acme", Ok(())), ("other", Err("OWNER_NOT_ALLOWED"))] {
            let mut args: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
            args.push(format!("-R{owner}/project"));
            let result = enforce_gh_repo_targets(&args, &allowlist, None);
            assert_eq!(result.map_err(|error| error.code()), expected, "{args:?}");
        }
    }
}

#[test]
fn gh_audited_required_values_are_never_repository_operands() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for prefix in [
        vec!["pr", "create", "-a"],
        vec!["pr", "create", "-m"],
        vec!["pr", "create", "-r"],
        vec!["pr", "edit", "--add-reviewer"],
        vec!["issue", "create", "--blocked-by"],
        vec!["issue", "create", "-l"],
        vec!["issue", "edit", "--remove-sub-issue"],
        vec!["issue", "edit", "-m"],
        vec!["pr", "list", "-s"],
        vec!["pr", "list", "-B"],
        vec!["issue", "list", "--mention"],
        vec!["issue", "develop", "-n"],
        vec!["pr", "checks", "-i"],
        vec!["pr", "diff", "-e"],
        vec!["pr", "lock", "-r"],
        vec!["issue", "close", "--duplicate-of"],
        vec!["api", "-f"],
        vec!["api", "-p"],
    ] {
        for (value, target, expected) in [
            ("https://example.com/a/b", "-Racme/project", Ok(())),
            ("-Rother/value", "-Racme/project", Ok(())),
            ("--", "-Rother/project", Err("OWNER_NOT_ALLOWED")),
        ] {
            let mut args: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
            args.extend([value.to_owned(), target.to_owned()]);
            assert_eq!(
                enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
                expected,
                "{args:?}"
            );
        }
    }
}

#[test]
fn gh_real_operands_after_end_marker_retain_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec![
            "pr",
            "view",
            "--",
            "HTTPS://GITHUB.COM/other/project/pull/1",
        ],
        vec!["issue", "transfer", "--", "1", "other/project"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None).map_err(|error| error.code()),
            Err("OWNER_NOT_ALLOWED"),
            "{args:?}"
        );
    }
}

#[test]
fn gh_command_arity_preserves_clustered_selectors_and_attached_values() {
    for (family, verb, flag, expected) in [
        ("label", "create", "-fRother/project", Some("other/project")),
        ("pr", "create", "-fRother/project", Some("other/project")),
        ("api", "", "-fRother/project", None),
        ("pr", "create", "-rRother/project", None),
        ("pr", "review", "-rRother/project", Some("other/project")),
        ("pr", "create", "-mRother/project", None),
        ("pr", "merge", "-mRother/project", Some("other/project")),
        ("pr", "list", "-sRother/project", None),
        ("pr", "merge", "-sRother/project", Some("other/project")),
        ("pr", "checks", "-iRother/project", None),
        ("issue", "create", "-lRother/project", None),
        (
            "issue",
            "develop",
            "-lRother/project",
            Some("other/project"),
        ),
        ("pr", "diff", "-eRother/project", None),
        ("pr", "create", "-eRother/project", Some("other/project")),
        ("browse", "", "-cRother/project", Some("other/project")),
        ("browse", "", "-c=Rother/project", None),
    ] {
        let args: Vec<String> = [family, verb, flag]
            .into_iter()
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        assert_eq!(gh_repo_selector(&args), expected, "{args:?}");
    }
}

#[test]
fn gh_audited_booleans_leave_real_end_markers_intact() {
    for prefix in [
        vec!["label", "create", "name", "-f"],
        vec!["pr", "create", "-f"],
        vec!["repo", "edit", "--template"],
        vec!["workflow", "run", "--json"],
        vec!["browse", "-c"],
        vec!["browse", "--commit=Rother/project"],
    ] {
        let mut args: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
        args.extend(["--".into(), "-Rother/project".into()]);
        assert_eq!(gh_repo_selector(&args), None, "{args:?}");
    }
}

#[test]
fn gh_repo_selector_preserves_option_values_and_real_end_markers() {
    for (args, expected) in [
        (vec!["pr", "close", "1", "-c", "-Rother/project"], None),
        (vec!["api", "-p", "--repo=other/project"], None),
        (
            vec!["pr", "create", "-tRelease", "-Racme/project"],
            Some("acme/project"),
        ),
        (
            vec!["pr", "review", "-ctRelease", "-Racme/project"],
            Some("acme/project"),
        ),
        (
            vec!["pr", "review", "-cRother/project"],
            Some("other/project"),
        ),
        (
            vec!["release", "create", "v1", "-pRother/project"],
            Some("other/project"),
        ),
        (
            vec!["pr", "close", "-c", "--", "-Rother/project"],
            Some("other/project"),
        ),
        (
            vec!["api", "-p", "--", "-Rother/project"],
            Some("other/project"),
        ),
        (vec!["pr", "review", "-c", "--", "-Rother/project"], None),
        (
            vec!["release", "create", "v1", "-p", "--", "-Rother/project"],
            None,
        ),
        (
            vec!["pr", "create", "-tRelease", "--", "-Rother/project"],
            None,
        ),
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(gh_repo_selector(&args), expected, "{args:?}");
    }
}

#[test]
fn gh_repo_selector_ignores_repo_letters_in_attached_option_values() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec!["pr", "create", "-tRelease"],
        vec!["pr", "close", "1", "-cRelease"],
        vec!["pr", "review", "-ctRelease"],
        vec!["api", "-pRelease", "repos/acme/project"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(gh_repo_selector(&args), None, "{args:?}");
        enforce_gh_repo_targets(&args, &allowlist, None).unwrap();
    }
}

#[test]
fn gh_pr_url_positional_is_enforced_against_allowlist() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(
        &[
            "pr".to_owned(),
            "view".to_owned(),
            "https://github.com/other/repo/pull/1".to_owned(),
        ],
        &allowlist,
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
}

#[test]
fn filesystem_origin_is_not_a_supported_github_remote() {
    assert!(!is_supported_github_remote("/tmp/acme/repo"));
    assert!(!is_supported_github_remote("file:///tmp/acme/repo.git"));
    assert!(is_supported_github_remote(
        "https://github.com/acme/repo.git"
    ));
    assert!(is_supported_github_remote("git@github.com:acme/repo.git"));
    assert_eq!(
        pin_gh_repo_selector(vec!["pr".into(), "comment".into(), "1".into()], "acme/repo"),
        vec!["pr", "comment", "1", "--repo", "acme/repo"]
    );
    assert_eq!(
        pin_gh_repo_selector(
            vec!["pr".into(), "-R".into(), "acme/repo".into(), "view".into()],
            "acme/repo"
        ),
        vec!["pr", "-R", "acme/repo", "view"]
    );
}

#[test]
fn gh_pflag_repo_spellings_outside_allowlist_are_rejected_before_run() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    for args in [
        vec![
            "pr".to_owned(),
            "view".to_owned(),
            "-R=other/repo".to_owned(),
            "1".to_owned(),
        ],
        vec![
            "pr".to_owned(),
            "view".to_owned(),
            "-Rother/repo".to_owned(),
            "1".to_owned(),
        ],
        vec![
            "pr".to_owned(),
            "view".to_owned(),
            "-wR".to_owned(),
            "other/repo".to_owned(),
            "1".to_owned(),
        ],
    ] {
        let err = SafeGhCommand::with_allowlist(&args, &allowlist).unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::OwnerNotAllowed,
                    ..
                }
            ),
            "expected OwnerNotAllowed for {args:?}, got {err:?}"
        );
    }
}

#[test]
fn gh_repo_env_is_enforced_when_argv_has_no_selector() {
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = enforce_gh_repo_targets(
        &["pr".to_owned(), "view".to_owned(), "1".to_owned()],
        &allowlist,
        Some("other/repo"),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
    enforce_gh_repo_targets(
        &["pr".to_owned(), "view".to_owned(), "1".to_owned()],
        &allowlist,
        Some("github.com/Acme/Repo"),
    )
    .unwrap();
}

#[test]
fn gh_repo_env_is_ignored_for_repository_independent_commands() {
    // GH_REPO only affects commands that operate on a repository. A
    // repository-independent command such as `gh auth status` must not be
    // rejected just because GH_REPO points at some other repo.
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    for args in [
        vec!["auth".to_owned(), "status".to_owned()],
        vec!["ssh-key".to_owned(), "list".to_owned()],
        vec!["gist".to_owned(), "list".to_owned()],
    ] {
        enforce_gh_repo_targets(&args, &allowlist, Some("other/repo"))
            .unwrap_or_else(|e| panic!("expected {args:?} to be allowed, got {e:?}"));
    }
    // A repository-context command with the same GH_REPO is still enforced.
    let err = enforce_gh_repo_targets(
        &["pr".to_owned(), "view".to_owned(), "1".to_owned()],
        &allowlist,
        Some("other/repo"),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::OwnerNotAllowed,
            ..
        }
    ));
}

#[test]
fn implicit_gh_repo_binds_to_origin_for_mutating_pr() {
    // Mutating `gh pr` with no explicit -R but GH_REPO pointing at another
    // repo must be rejected against the verified origin, mirroring -R.
    let args = vec!["pr".to_owned(), "close".to_owned(), "1".to_owned()];
    let err = bind_gh_repo_selector_to_origin(&args, Some("other/repo"), "acme/repo").unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ),
        "expected PathNotAllowed for implicit GH_REPO mismatch, got {err:?}"
    );
    // A matching implicit selector is accepted.
    bind_gh_repo_selector_to_origin(&args, Some("github.com/Acme/Repo"), "acme/repo").unwrap();
    // No selector at all falls back to the working directory (accepted).
    bind_gh_repo_selector_to_origin(&args, None, "acme/repo").unwrap();
}

#[test]
fn explicit_r_takes_precedence_over_gh_repo_env_for_binding() {
    // An explicit -R wins over GH_REPO, and is bound to origin.
    let args = vec![
        "pr".to_owned(),
        "close".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
        "1".to_owned(),
    ];
    assert_eq!(
        effective_gh_repo_selector(&args, Some("acme/repo")).as_deref(),
        Some("other/repo")
    );
    let err = bind_gh_repo_selector_to_origin(&args, Some("acme/repo"), "acme/repo").unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_repo_selector_detects_r_after_dashdash_consumed_as_option_value() {
    // `--template --` makes gh consume `--` as the template value, so gh
    // keeps parsing and `-R other/repo` is still an active selector. The
    // scanner must not treat that `--` as the options terminator.
    let args = vec![
        "pr".to_owned(),
        "view".to_owned(),
        "--template".to_owned(),
        "--".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
    ];
    assert_eq!(gh_repo_selector(&args), Some("other/repo"));

    // And it must be enforced against the allowlist at the SafeGhCommand
    // boundary rather than slipping through.
    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(&args, &allowlist).unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                ..
            }
        ),
        "expected OwnerNotAllowed for `-R other/repo` after `--template --`, got {err:?}"
    );
}

#[test]
fn gh_repo_selector_detects_r_after_dashdash_following_unlisted_value_option() {
    // Regression for the residual bypass: `-c` is the short form of
    // `--comment` (`gh pr close`) and is NOT in GH_VALUE_TAKING_OPTIONS, yet
    // gh consumes the trailing `--` as its value and keeps parsing, so
    // `-R other/repo` stays an active selector. The fail-closed arity logic
    // must still detect and enforce it; keying only off the hand-maintained
    // value-option list (the pre-fix behavior) would miss it entirely.
    let args = vec![
        "pr".to_owned(),
        "close".to_owned(),
        "1".to_owned(),
        "-c".to_owned(),
        "--".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
    ];
    assert_eq!(
        gh_repo_selector(&args),
        Some("other/repo"),
        "`-R other/repo` after `-c --` must still be detected"
    );

    let allowlist = crate::owners::OwnerAllowlist::from_owners(["acme"]);
    let err = SafeGhCommand::with_allowlist(&args, &allowlist).unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::OwnerNotAllowed,
                ..
            }
        ),
        "expected OwnerNotAllowed for `-R other/repo` after `-c --`, got {err:?}"
    );
}

#[test]
fn gh_repo_selector_still_terminates_on_real_dashdash() {
    // A bare `--` that is NOT an option value remains an end-of-options
    // terminator, so a following `-R` is a positional and not a selector.
    let args = vec![
        "pr".to_owned(),
        "view".to_owned(),
        "--".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
    ];
    assert_eq!(gh_repo_selector(&args), None);
}

#[test]
fn gh_repo_selector_terminates_on_dashdash_after_known_boolean_flag() {
    // `--web` is a known boolean flag, so it does NOT consume the following
    // `--`; that `--` is a genuine terminator and the trailing `-R` is a
    // positional operand to gh, not an active selector.
    let args = vec![
        "pr".to_owned(),
        "view".to_owned(),
        "--web".to_owned(),
        "--".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
    ];
    assert_eq!(gh_repo_selector(&args), None);
}

#[test]
fn first_positional_after_skips_dashdash_consumed_as_option_value() {
    // `-t --` consumes `--` as the `-t`/`--template` value; the real
    // sub-subcommand `close` still follows and must be found.
    let args = vec![
        "-t".to_owned(),
        "--".to_owned(),
        "close".to_owned(),
        "1".to_owned(),
    ];
    assert_eq!(first_positional_after(&args), Some("close"));
}

#[test]
fn push_refspec_to_other_branch_rejected() {
    let err = reject_push_outside_expected_branch(
        &[
            "push".to_owned(),
            "origin".to_owned(),
            "HEAD:main".to_owned(),
        ],
        "feature",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }
    ));
}

#[test]
fn push_to_expected_branch_ok() {
    reject_push_outside_expected_branch(
        &[
            "push".to_owned(),
            "origin".to_owned(),
            "HEAD:feature".to_owned(),
        ],
        "feature",
    )
    .unwrap();
}

#[test]
fn clone_u_upload_pack_rejected() {
    let err = SafeGitCommand::new(&[
        "clone".to_owned(),
        "-u".to_owned(),
        "sh -c true".to_owned(),
        "https://example.com/r.git".to_owned(),
        "dst".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn ext_transport_url_rejected() {
    let err = SafeGitCommand::new(&[
        "ls-remote".to_owned(),
        "ext::gh pr merge 1 --merge".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn force_refspec_rejected() {
    let err = SafeGitCommand::new(&[
        "push".to_owned(),
        "origin".to_owned(),
        "+HEAD:main".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            ..
        }
    ));
}

// ---- mutating subcommand detection ----

#[test]
fn push_is_mutating() {
    let cmd = SafeGitCommand::new(&["push".to_owned()]).unwrap();
    assert!(cmd.requires_branch_check());
}

#[test]
fn commit_is_mutating() {
    let cmd =
        SafeGitCommand::new(&["commit".to_owned(), "-m".to_owned(), "msg".to_owned()]).unwrap();
    assert!(cmd.requires_branch_check());
}

#[test]
fn add_is_mutating() {
    let cmd = SafeGitCommand::new(&["add".to_owned(), ".".to_owned()]).unwrap();
    assert!(cmd.requires_branch_check());
}

#[test]
fn clean_is_mutating() {
    let cmd = SafeGitCommand::new(&["clean".to_owned(), "-fd".to_owned()]).unwrap();
    assert!(cmd.requires_branch_check());
}

#[test]
fn status_is_not_mutating() {
    let cmd = SafeGitCommand::new(&["status".to_owned()]).unwrap();
    assert!(!cmd.requires_branch_check());
}

#[test]
fn diff_is_not_mutating() {
    let cmd = SafeGitCommand::new(&["diff".to_owned()]).unwrap();
    assert!(!cmd.requires_branch_check());
}

// ---- gh allowlist tests ----

#[test]
fn gh_pr_create_allowed() {
    let cmd = SafeGhCommand::new(&[
        "pr".to_owned(),
        "create".to_owned(),
        "--title".to_owned(),
        "test".to_owned(),
    ])
    .unwrap();
    assert_eq!(cmd.args(), &["pr", "create", "--title", "test"]);
}

#[test]
fn gh_remote_pr_actions_are_allowed_with_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for args in [
        vec!["pr", "-R", "acme/widgets", "merge", "1", "-m"],
        vec!["pr", "-R=acme/widgets", "merge", "1"],
        vec!["pr", "ready", "1", "--repo", "acme/widgets"],
        vec![
            "pr",
            "update-branch",
            "1",
            "--repo",
            "acme/widgets",
            "--rebase",
        ],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        SafeGhCommand::with_allowlist(&args, &allowlist).unwrap();
        assert!(!gh_requires_branch_check(&args));
    }
    for flag in [
        "--merge",
        "--squash",
        "--rebase",
        "--auto",
        "--admin",
        "--merge-queue",
    ] {
        let args = vec![
            "pr".into(),
            "merge".into(),
            "1".into(),
            "-Racme/widgets".into(),
            flag.into(),
        ];
        SafeGhCommand::with_allowlist(&args, &allowlist).unwrap();
    }
    let args = vec![
        "pr".into(),
        "merge".into(),
        "1".into(),
        "--repo=other/widgets".into(),
    ];
    assert_eq!(
        SafeGhCommand::with_allowlist(&args, &allowlist)
            .unwrap_err()
            .code(),
        "OWNER_NOT_ALLOWED"
    );
}

#[test]
fn gh_checkout_changing_actions_remain_protected() {
    for args in [
        vec!["pr", "checkout", "1"],
        vec!["pr", "merge", "1", "--delete-branch"],
        vec!["pr", "close", "1", "-d"],
        vec!["pr", "merge", "1", "--delete-branch=true"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert!(gh_requires_branch_check(&args));
        assert!(SafeGhCommand::new(&args).is_err());
    }
    for verb in [
        "create",
        "close",
        "reopen",
        "edit",
        "review",
        "ready",
        "merge",
        "update-branch",
    ] {
        assert!(!gh_requires_branch_check(&[
            "pr".into(),
            verb.into(),
            "1".into()
        ]));
    }
}

#[test]
fn gh_merge_rejects_enabled_deletion_in_short_option_clusters() {
    for flags in [
        vec!["-md=true"],
        vec!["-sd=1"],
        vec!["-dm=false"],
        vec!["-dtupdated"],
        vec!["-tupdated", "-d"],
    ] {
        let mut args = vec!["pr".into(), "merge".into(), "1".into()];
        args.extend(flags.into_iter().map(str::to_owned));
        assert!(gh_requires_branch_check(&args), "{args:?}");
        assert_eq!(
            SafeGhCommand::new(&args).unwrap_err().code(),
            "MERGE_BLOCKED"
        );
    }
}

#[test]
fn gh_merge_does_not_treat_attached_values_as_deletion_flags() {
    for (verb, flags) in [
        ("merge", vec!["-tupdated"]),
        ("merge", vec!["-bupdated"]),
        ("merge", vec!["-mtupdated"]),
        ("close", vec!["-cupdated"]),
        ("merge", vec!["-md=false"]),
        ("merge", vec!["-md=0"]),
        ("merge", vec!["-d", "--delete-branch=false"]),
    ] {
        let mut args = vec!["pr".into(), verb.into(), "1".into()];
        args.extend(flags.into_iter().map(str::to_owned));
        assert!(!gh_requires_branch_check(&args), "{args:?}");
        SafeGhCommand::new(&args).unwrap();
    }
}

#[test]
fn gh_api_short_clusters_preserve_literal_repository_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for flags in [
        vec!["-iHAccept:application/vnd.github+json"],
        vec!["-ii"],
        vec!["-iXPUT"],
        vec!["-iiH", "Accept:application/vnd.github+json"],
    ] {
        for owner in ["acme", "other"] {
            let mut args = vec!["api".into()];
            args.extend(flags.iter().map(|flag| (*flag).to_owned()));
            args.push(format!("repos/{owner}/project"));
            let result = SafeGhCommand::with_allowlist(&args, &allowlist);
            if owner == "acme" {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().code(), "OWNER_NOT_ALLOWED", "{args:?}");
            }
        }
    }
}

#[test]
fn gh_api_repo_placeholder_preserves_literal_owner_check() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (endpoint, allowed) in [
        ("repos/acme/{repo}/pulls", true),
        ("repos/other/{repo}/pulls", false),
        ("https://api.github.com/repos/other/{repo}/pulls", false),
    ] {
        let args = vec!["api".into(), endpoint.into()];
        let result = enforce_gh_repo_targets(&args, &allowlist, Some("acme/project"));
        if allowed {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().code(), "OWNER_NOT_ALLOWED");
        }
    }
}

#[test]
fn gh_api_uses_literal_rest_endpoint_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for endpoint in [
        "repos/acme/widgets/pulls/1/merge",
        "https://api.github.com/repos/acme/widgets/pulls/1/merge",
        "graphql",
    ] {
        let args = vec![
            "api".into(),
            endpoint.into(),
            "--method".into(),
            "PUT".into(),
        ];
        SafeGhCommand::with_allowlist(&args, &allowlist).unwrap();
    }
    for endpoint in [
        "repos/other/widgets/pulls/1/merge",
        "/repos/other/widgets/issues",
        "https://api.github.com/repos/other/widgets",
    ] {
        let args = vec![
            "api".into(),
            "--method".into(),
            "PUT".into(),
            endpoint.into(),
        ];
        assert_eq!(
            SafeGhCommand::with_allowlist(&args, &allowlist)
                .unwrap_err()
                .code(),
            "OWNER_NOT_ALLOWED"
        );
    }
}

#[test]
fn gh_api_repository_collections_enforce_literal_owners() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for (endpoint, allowed) in [
        ("orgs/other/repos", false),
        ("users/other/repos", false),
        ("/orgs/other/repos?type=all", false),
        ("https://api.github.com/orgs/other/repos", false),
        (
            "https://api.github.com/users/other/repos?per_page=10",
            false,
        ),
        ("orgs/acme/repos", true),
        ("users/ACME/repos", true),
        ("https://api.github.com/users/acme/repos", true),
    ] {
        let args: Vec<String> = ["api", "-X", "POST", endpoint, "-f", "name=outside"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let result = enforce_gh_repo_targets(&args, &allowlist, Some("acme/project"));
        assert_eq!(result.is_ok(), allowed, "{endpoint}: {result:?}");
        if !allowed {
            assert_eq!(result.unwrap_err().code(), "OWNER_NOT_ALLOWED");
        }
    }
}

#[test]
fn gh_api_collection_flags_keep_explicit_owner_authoritative() {
    for flags in [
        vec!["--method=POST"],
        vec!["-iXPOST"],
        vec!["-iH", "Accept:application/vnd.github+json"],
        vec!["--silent=true"],
    ] {
        for owner in ["acme", "other"] {
            let mut args = vec!["api".into()];
            args.extend(flags.iter().map(|flag| (*flag).to_owned()));
            args.push(format!("orgs/{owner}/repos"));
            let allowlist = crate::owners::OwnerAllowlist::parse("acme");
            // An unrelated local default must neither veto nor authorize this endpoint.
            let result = enforce_gh_repo_targets(&args, &allowlist, Some("other/default"));
            assert_eq!(result.is_ok(), owner == "acme", "{args:?}: {result:?}");
            if owner != "acme" {
                assert_eq!(result.unwrap_err().code(), "OWNER_NOT_ALLOWED");
            }
        }
    }
}

#[test]
fn gh_api_absolute_urls_preserve_owner_checks_across_scheme_and_host_case() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for origin in [
        "https://api.github.com",
        "http://api.github.com",
        "HTTPS://API.GITHUB.COM",
        "hTtP://Api.GitHub.Com",
        "https://api.github.com:443",
        "https://API.GITHUB.COM.",
        "http://api.github.com:80",
        "https://user@api.github.com",
    ] {
        for owner in ["acme", "other"] {
            let args = vec!["api".into(), format!("{origin}/repos/{owner}/project")];
            let result = enforce_gh_repo_targets(&args, &allowlist, None);
            assert_eq!(result.is_ok(), owner == "acme", "{args:?}: {result:?}");
            if owner == "other" {
                assert_eq!(result.unwrap_err().code(), "OWNER_NOT_ALLOWED");
            }
        }
    }
}

#[test]
fn gh_pr_urls_preserve_public_and_enterprise_owner_checks() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for origin in [
        "https://github.com",
        "http://github.com",
        "HTTPS://GITHUB.COM",
        "hTtP://GitHub.Com",
        "https://git.example.com",
        "HTTPS://GIT.EXAMPLE.COM:443",
        "https://user@git.example.com",
        "https://GITHUB.COM.",
    ] {
        for (command, kind) in [("pr", "pull"), ("issue", "issues")] {
            for (owner, expected) in [("acme", Ok(())), ("other", Err("OWNER_NOT_ALLOWED"))] {
                let args = vec![
                    command.into(),
                    "view".into(),
                    format!("{origin}/{owner}/project/{kind}/1"),
                ];
                let result = enforce_gh_repo_targets(&args, &allowlist, None);
                assert_eq!(result.map_err(|error| error.code()), expected, "{args:?}");
            }
        }
    }
}

#[test]
fn gh_api_url_host_confusion_does_not_infer_a_github_owner() {
    let allowlist = crate::owners::OwnerAllowlist::default();
    for endpoint in [
        "https://api.github.com.evil.example/repos/other/project",
        "https://evil-api.github.com/repos/other/project",
        "https://api.github.com@evil.example/repos/other/project",
        "https://api.github.com?next=/repos/other/project",
        "https://api.github.com",
        "ftp://api.github.com/repos/other/project",
    ] {
        let args = vec!["api".into(), endpoint.into()];
        // Unsupported URLs are outside literal inference, not a new transport ban.
        enforce_gh_repo_targets(&args, &allowlist, None).unwrap();
    }
}

#[test]
fn gh_pr_malformed_http_authorities_are_rejected() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for url in [
        "https://git hub.example/acme/project/pull/1",
        "https://github.com:invalid/acme/project/pull/1",
        "https://.github.com/acme/project/pull/1",
        "https://github..com/acme/project/pull/1",
    ] {
        let args = vec!["pr".into(), "view".into(), url.into()];
        assert_eq!(
            enforce_gh_repo_targets(&args, &allowlist, None)
                .unwrap_err()
                .code(),
            "OWNER_NOT_ALLOWED"
        );
    }
}

#[test]
fn gh_api_options_do_not_hide_literal_repository_target() {
    let allowlist = crate::owners::OwnerAllowlist::parse("acme");
    for flags in [
        vec!["--silent"],
        vec!["--silent=true"],
        vec!["--paginate", "--slurp"],
        vec!["-i"],
        vec!["-XPOST"],
        vec!["--method=PUT"],
        vec!["--preview", "example"],
        vec!["-HAccept:application/json"],
    ] {
        let mut args: Vec<String> = vec!["api".into()];
        args.extend(flags.into_iter().map(str::to_owned));
        args.push("repos/other/widgets/pulls/1/merge".into());
        assert_eq!(
            SafeGhCommand::with_allowlist(&args, &allowlist)
                .unwrap_err()
                .code(),
            "OWNER_NOT_ALLOWED"
        );
    }
}

#[test]
fn gh_unknown_subcommand_rejected() {
    let err = SafeGhCommand::new(&["codespace".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::GhSubcommandNotAllowed,
            ..
        }
    ));
}

#[test]
fn gh_issue_list_allowed() {
    let cmd = SafeGhCommand::new(&["issue".to_owned(), "list".to_owned()]).unwrap();
    assert_eq!(cmd.args(), &["issue", "list"]);
}

#[test]
fn gh_run_rerun_and_view_allowed() {
    let rerun =
        SafeGhCommand::new(&["run".to_owned(), "rerun".to_owned(), "12345".to_owned()]).unwrap();
    assert_eq!(rerun.args(), &["run", "rerun", "12345"]);

    let view = SafeGhCommand::new(&[
        "run".to_owned(),
        "view".to_owned(),
        "12345".to_owned(),
        "--log-failed".to_owned(),
    ])
    .unwrap();
    assert_eq!(view.args(), &["run", "view", "12345", "--log-failed"]);

    let repo_flag = SafeGhCommand::with_allowlist(
        &[
            "run".to_owned(),
            "-R".to_owned(),
            "acme/example-org".to_owned(),
            "rerun".to_owned(),
            "9".to_owned(),
        ],
        &crate::owners::OwnerAllowlist::from_owners(["acme"]),
    )
    .unwrap();
    assert_eq!(
        repo_flag.args(),
        &["run", "-R", "acme/example-org", "rerun", "9"]
    );
}

#[test]
fn gh_run_download_relative_dir_allowed() {
    // `SafeGhCommand::new` applies only the cwd-independent string gate to a
    // download `--dir`; the symlink gate is rooted at the worktree in the
    // supervisor path, so this test must not mutate the process-global cwd.

    let relative = SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "123".to_owned(),
        "--dir".to_owned(),
        "artifacts".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        relative.args(),
        &["run", "download", "123", "--dir", "artifacts"]
    );

    SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "-D".to_owned(),
        "artifacts".to_owned(),
    ])
    .unwrap();
    SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "--dir=artifacts".to_owned(),
    ])
    .unwrap();
    SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "-Dartifacts".to_owned(),
    ])
    .unwrap();
    // `--name` / `--pattern` select artifacts; they are not filesystem destinations.
    SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "--name".to_owned(),
        "coverage".to_owned(),
        "--pattern".to_owned(),
        "cov-*".to_owned(),
    ])
    .unwrap();
    SafeGhCommand::new(&[
        "run".to_owned(),
        "download".to_owned(),
        "-n".to_owned(),
        "../selector-not-a-path".to_owned(),
    ])
    .unwrap();
    SafeGhCommand::new(&["run".to_owned(), "download".to_owned(), "123".to_owned()]).unwrap();
}

#[test]
fn gh_run_download_external_dir_rejected() {
    let cases = [
        vec!["run", "download", "--dir", "/tmp/outside"],
        vec!["run", "download", "--dir=../outside"],
        vec!["run", "download", "-D", "/tmp/outside"],
        vec!["run", "download", "-D/tmp/outside"],
        vec!["run", "download", "-D=../outside"],
        vec!["run", "download", "--dir"],
    ];
    for args in cases {
        let owned: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let err = SafeGhCommand::new(&owned).unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::PathNotAllowed,
                    ..
                }
            ),
            "expected PathNotAllowed for {owned:?}, got {err:?}"
        );
    }
}

#[test]
fn gh_run_download_unknown_flag_rejected() {
    for args in [
        vec!["run", "download", "-hD/tmp/outside"],
        vec!["run", "download", "-xD", "/tmp/outside"],
        vec!["run", "download", "--output", "/tmp/outside"],
    ] {
        let owned: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let err = SafeGhCommand::new(&owned).unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::GhFlagNotAllowed,
                    ..
                }
            ),
            "expected GhFlagNotAllowed for {owned:?}, got {err:?}"
        );
    }
}

#[test]
fn gh_run_delete_and_cancel_rejected() {
    for verb in ["delete", "cancel"] {
        let err =
            SafeGhCommand::new(&["run".to_owned(), verb.to_owned(), "1".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                ..
            }
        ));
    }
    let err = SafeGhCommand::new(&["run".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::GhSubcommandNotAllowed,
            ..
        }
    ));
}

// ---- error display tests ----

#[test]
fn policy_code_display() {
    assert_eq!(PolicyCode::BareForcePush.as_str(), "BARE_FORCE_PUSH");
    assert_eq!(PolicyCode::MergeBlocked.as_str(), "MERGE_BLOCKED");
    assert_eq!(
        PolicyCode::SubcommandNotAllowed.as_str(),
        "SUBCOMMAND_NOT_ALLOWED"
    );
    assert_eq!(PolicyCode::BranchMismatch.as_str(), "BRANCH_MISMATCH");
    assert_eq!(
        PolicyCode::GhSubcommandNotAllowed.as_str(),
        "GH_SUBCOMMAND_NOT_ALLOWED"
    );
    assert_eq!(PolicyCode::GhFlagNotAllowed.as_str(), "GH_FLAG_NOT_ALLOWED");
    assert_eq!(PolicyCode::OwnerNotAllowed.as_str(), "OWNER_NOT_ALLOWED");
    assert_eq!(
        PolicyCode::UnauthorizedSource.as_str(),
        "UNAUTHORIZED_SOURCE"
    );
}

#[test]
fn error_display_includes_code_and_message() {
    let err = Error::PolicyViolation {
        code: PolicyCode::BareForcePush,
        message: "test message".to_owned(),
    };
    let display = format!("{err}");
    assert!(display.contains("BARE_FORCE_PUSH"));
    assert!(display.contains("test message"));
}

/// Isolated git repo with a named branch for execution tests.
///
/// Avoids depending on the workspace checkout (tarpaulin / detached HEAD).
fn temp_repo_with_branch(branch: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "writ-core-git-safe-{}-{}-{}",
        std::process::id(),
        seq,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).expect("create temp repo dir");

    let null_dev = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_dev)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    // Portable across Git versions / Windows template races.
    git(&["init"]);
    git(&["checkout", "-b", branch]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "writ-core-test"]);
    // Keep blob contents byte-identical on Windows runners (autocrlf rewrites \n).
    git(&["config", "core.autocrlf", "false"]);
    std::fs::write(
        dir.join("README"),
        "init
",
    )
    .expect("write README");
    git(&["add", "README"]);
    git(&["commit", "-m", "init"]);
    dir
}

#[test]
fn run_status_in_repo_executes() {
    let repo = temp_repo_with_branch("main");
    let cmd =
        SafeGitCommand::new(&["rev-parse".to_owned(), "--is-inside-work-tree".to_owned()]).unwrap();
    let out = cmd.run(&repo, None).expect("git should run in temp repo");
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert_eq!(out.stdout.trim(), "true");
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn run_verifies_expected_branch_for_mutating() {
    let repo = temp_repo_with_branch("job-branch");
    let current = resolve_current_branch(&repo).expect("resolve branch");
    assert_eq!(current, "job-branch");

    // status is not mutating — expected_branch is ignored.
    let cmd = SafeGitCommand::new(&["status".to_owned(), "--porcelain".to_owned()]).unwrap();
    let out = cmd
        .run(&repo, Some("definitely-not-this-branch"))
        .expect("status should run");
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);

    // Mutating with wrong branch is rejected before spawn.
    let push = SafeGitCommand::new(&["push".to_owned(), "--dry-run".to_owned()]).unwrap();
    let err = push
        .run(&repo, Some("definitely-not-this-branch"))
        .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }
    ));

    // Matching branch passes branch verification (push may still fail without a remote).
    let push_ok = SafeGitCommand::new(&["push".to_owned(), "--dry-run".to_owned()]).unwrap();
    match push_ok.run(&repo, Some("job-branch")) {
        Ok(_) => {}
        Err(Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            ..
        }) => panic!("matching branch must not fail branch verification"),
        Err(_) => {} // e.g. no remote configured
    }

    let _ = std::fs::remove_dir_all(&repo);
}

fn git_in(dir: &std::path::Path, args: &[&str]) {
    let null_dev = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_dev)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Two assigned feature-branch worktrees that share one repository.
fn two_assigned_worktrees() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let repo = temp_repo_with_branch("main");
    let worker_a = repo.with_file_name(format!(
        "{}-worker-a",
        repo.file_name().unwrap().to_string_lossy()
    ));
    let worker_b = repo.with_file_name(format!(
        "{}-worker-b",
        repo.file_name().unwrap().to_string_lossy()
    ));
    git_in(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "worker-a",
            worker_a.to_str().expect("utf8 worktree path"),
        ],
    );
    git_in(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "worker-b",
            worker_b.to_str().expect("utf8 worktree path"),
        ],
    );
    std::fs::write(worker_a.join("a.txt"), "from-a\n").unwrap();
    git_in(&worker_a, &["add", "a.txt"]);
    git_in(&worker_a, &["commit", "-m", "worker-a change"]);
    std::fs::write(worker_b.join("b.txt"), "from-b\n").unwrap();
    git_in(&worker_b, &["add", "b.txt"]);
    git_in(&worker_b, &["commit", "-m", "worker-b change"]);
    (repo, worker_a, worker_b)
}

fn remove_all(repo: &std::path::Path, worker_a: &std::path::Path, worker_b: &std::path::Path) {
    let _ = std::fs::remove_dir_all(worker_a);
    let _ = std::fs::remove_dir_all(worker_b);
    let _ = std::fs::remove_dir_all(repo);
}

/// `cmd` must be refused with MERGE_BLOCKED whose message contains `needle`.
fn expect_merge_blocked(
    cmd: &SafeGitCommand,
    dir: &std::path::Path,
    expected_branch: Option<&str>,
    needle: &str,
) {
    let err = cmd.run(dir, expected_branch).unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ),
        "{err}"
    );
    assert!(format!("{err}").contains(needle), "{err}");
}

#[test]
fn two_worktree_local_merge_integrates_peer_branch() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    let cmd = SafeGitCommand::new(&[
        "merge".to_owned(),
        "--no-edit".to_owned(),
        "worker-a".to_owned(),
    ])
    .unwrap();
    let out = cmd
        .run(&worker_b, Some("worker-b"))
        .expect("peer merge should be admitted");
    assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(worker_b.join("a.txt")).unwrap(),
        "from-a\n"
    );
    assert_eq!(
        std::fs::read_to_string(worker_b.join("b.txt")).unwrap(),
        "from-b\n"
    );
    remove_all(&repo, &worker_a, &worker_b);
}

#[test]
fn merge_on_explicitly_assigned_main_is_allowed() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "worker-a".to_owned()]).unwrap();
    let out = cmd.run(&repo, Some("main")).unwrap();
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    remove_all(&repo, &worker_a, &worker_b);
}

#[test]
fn local_merge_without_branch_pin_accepts_clean_branch_names() {
    for branch in ["main", "master", "job-branch"] {
        let repo = temp_repo_with_branch(branch);
        git_in(&repo, &["checkout", "-b", "peer"]);
        std::fs::write(repo.join("peer.txt"), "peer change\n").unwrap();
        git_in(&repo, &["add", "peer.txt"]);
        git_in(&repo, &["commit", "-m", "peer change"]);
        git_in(&repo, &["checkout", branch]);
        let cmd = SafeGitCommand::new(&["merge".to_owned(), "peer".to_owned()]).unwrap();

        let err = cmd.run(&repo, Some("other-branch")).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }
        ));
        assert!(!repo.join("peer.txt").exists());

        let output = cmd.run(&repo, None).expect("direct branch pin is optional");
        assert_eq!(output.exit_code, 0, "{branch}: {}", output.stderr);
        assert_eq!(
            std::fs::read_to_string(repo.join("peer.txt")).unwrap(),
            "peer change\n"
        );
        assert_eq!(resolve_current_branch(&repo).unwrap(), branch);
        let _ = std::fs::remove_dir_all(repo);
    }
}

#[test]
fn merge_refuses_to_lose_uncommitted_wip() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    std::fs::write(worker_b.join("wip.txt"), "keep-me\n").unwrap();
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "worker-a".to_owned()]).unwrap();
    expect_merge_blocked(&cmd, &worker_b, Some("worker-b"), "uncommitted work");
    assert_eq!(
        std::fs::read_to_string(worker_b.join("wip.txt")).unwrap(),
        "keep-me\n"
    );
    assert!(
        !worker_b.join("a.txt").exists(),
        "peer file must not appear after refused merge"
    );
    remove_all(&repo, &worker_a, &worker_b);
}

#[test]
fn merge_abort_is_allowed_with_dirty_tree() {
    let repo = temp_repo_with_branch("worker-a");
    std::fs::write(repo.join("wip.txt"), "keep-me\n").unwrap();
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "--abort".to_owned()]).unwrap();
    let out = cmd
        .run(&repo, Some("worker-a"))
        .expect("merge --abort is recovery, not a new integration");
    // Git may exit non-zero if there is no merge in progress; policy must still admit it.
    assert!(
        out.exit_code == 0 || out.stderr.contains("MERGE_HEAD"),
        "stderr={}",
        out.stderr
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("wip.txt")).unwrap(),
        "keep-me\n"
    );
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn pull_on_explicitly_assigned_main_passes_local_admission() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    let cmd = SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap();
    cmd.admit_local_merge(&repo).unwrap();
    cmd.verify_branch(&repo, "main").unwrap();
    remove_all(&repo, &worker_a, &worker_b);
}

// ---- FEAT-002 / PR #183 finding #2: gh run verbs are not local-checkout changes ----

#[test]
fn gh_run_verbs_skip_local_checkout_check() {
    // `gh run` verbs act on remote Actions runs (GitHub authorizes) or, for
    // `download`, are bounded by their own destination gates. None switch or
    // delete the local checkout, so the local-checkout gate does not apply.
    for args in [
        vec!["run", "rerun", "123"],
        vec!["run", "rerun", "--failed", "123"],
        vec!["run", "download", "123"],
        vec!["run", "download", "--dir", "sub", "123"],
        vec!["run", "view", "123"],
        vec!["run", "view", "--log-failed", "123"],
        vec!["run", "list"],
        vec!["run", "watch", "123"],
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            !gh_requires_branch_check(&owned),
            "expected {args:?} to skip the local-checkout check"
        );
    }
}

#[test]
fn gh_issue_develop_checkout_flagged_as_local_checkout_change() {
    // `gh issue develop --checkout` creates the development branch AND switches
    // the worktree onto it; the bare form only creates the branch remotely.
    for args in [
        vec!["issue", "develop", "123", "--checkout"],
        vec!["issue", "develop", "123", "-c"],
        vec!["issue", "develop", "--checkout", "123"],
        vec!["issue", "develop", "-cl", "123"],
        vec!["issue", "develop", "123", "-c=false", "--checkout"],
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            gh_requires_branch_check(&owned),
            "expected {args:?} to be flagged as a local checkout change"
        );
    }
    for args in [
        vec!["issue", "develop", "123"],
        vec!["issue", "develop", "123", "-c=false"],
        vec!["issue", "develop", "123", "--checkout=false"],
        vec!["issue", "develop", "123", "--name", "feat-x"],
        vec!["issue", "develop", "123", "-n", "-c-looking-name"],
        vec!["issue", "view", "123", "-c"],
        vec!["issue", "create", "-c"],
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            !gh_requires_branch_check(&owned),
            "expected {args:?} to skip the local-checkout check"
        );
    }
}

#[test]
fn gh_pr_local_checkout_check_behaviour() {
    // Only pr forms that switch or delete the local checkout are flagged;
    // remote mutations and reads are GitHub's authorization decision.
    for args in [
        vec!["pr", "checkout", "1"],
        vec!["pr", "merge", "1", "--delete-branch"],
        vec!["pr", "close", "1", "--delete-branch"],
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            gh_requires_branch_check(&owned),
            "expected {args:?} to be flagged as a local checkout change"
        );
    }
    for args in [
        vec!["pr", "create"],
        vec!["pr", "close", "1"],
        vec!["pr", "merge", "1"],
        vec!["pr", "view", "1"],
        vec!["pr", "list"],
        vec!["pr", "diff", "1"],
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(
            !gh_requires_branch_check(&owned),
            "expected {args:?} to skip the local-checkout check"
        );
    }
}

#[test]
fn gh_run_rerun_mismatched_selector_rejected_by_origin_bind() {
    // A `gh run rerun` targeting a different (still allowed-owner) repo via -R
    // must be rejected when bound to the verified origin, exactly like pr
    // mutations. This is the gate that extending gh_requires_branch_check wires
    // in through supervisor_cmd::prepare_gh_command.
    let args = vec![
        "run".to_owned(),
        "rerun".to_owned(),
        "-R".to_owned(),
        "other/repo".to_owned(),
        "123".to_owned(),
    ];
    let err = bind_gh_repo_selector_to_origin(&args, None, "acme/repo").unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ),
        "expected PathNotAllowed for mismatched run rerun selector, got {err:?}"
    );
    // A matching selector is accepted.
    bind_gh_repo_selector_to_origin(&args, None, "other/repo").unwrap();
    // No selector falls back to the working directory (accepted); the pin step
    // in the supervisor then injects the verified origin.
    let bare = vec!["run".to_owned(), "rerun".to_owned(), "123".to_owned()];
    bind_gh_repo_selector_to_origin(&bare, None, "acme/repo").unwrap();
    let pinned = pin_gh_repo_selector(bare, "acme/repo");
    assert_eq!(gh_repo_selector(&pinned), Some("acme/repo"));
}

// ---- FEAT-002 / PR #183 finding #7: symlinked download destination escape ----

/// A `gh run download --dir <symlink>` whose destination escapes the worktree
/// via a symlink is rejected by the worktree-root-aware validator, and crucially
/// the validator uses the passed `worktree_root` — NOT the process cwd — so the
/// escape is caught even when the supervisor process runs in a different
/// directory (the normal supervised case). This is a pure function of
/// `(args, root)`: no `set_current_dir`, hence no `CWD_GUARD`.
///
/// If the validator were reverted to reading `std::env::current_dir()` instead
/// of `worktree_root`, the `evil` symlink lives only in `worktree_root` (not in
/// the process cwd, which stays elsewhere), so the walk would step up to an
/// existing ancestor of the process cwd and WRONGLY accept — this test would
/// fail. That makes it a meaningful regression guard for finding #1.
#[cfg(unix)]
#[test]
fn gh_run_download_symlink_escape_uses_worktree_root_not_cwd() {
    use std::os::unix::fs::symlink;

    // External directory the symlink escapes to.
    let outside = tempfile::tempdir().unwrap();
    // Worktree root the child `gh` process would run in.
    let worktree = tempfile::tempdir().unwrap();
    let worktree_root = worktree.path().canonicalize().unwrap();

    // Keep the PROCESS cwd somewhere else entirely so a cwd-based check could
    // not see the worktree's `evil` symlink. We do not chdir to it; the
    // validator takes the root explicitly.
    let process_cwd = tempfile::tempdir().unwrap();
    let _process_cwd_root = process_cwd.path().canonicalize().unwrap();

    // `evil` is a relative name that passes the string check but points outside
    // the worktree.
    symlink(outside.path(), worktree_root.join("evil")).unwrap();

    // Direct symlink destination is rejected.
    let err = reject_external_gh_download_destination_in(
        &[
            "run".to_owned(),
            "download".to_owned(),
            "--dir".to_owned(),
            "evil".to_owned(),
            "123".to_owned(),
        ],
        &worktree_root,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ),
        "expected PathNotAllowed for symlinked download dir, got {err:?}"
    );

    // A symlink used as a leading component of a deeper destination is also
    // rejected (the extraction would still land outside).
    let err = reject_external_gh_download_destination_in(
        &[
            "run".to_owned(),
            "download".to_owned(),
            "--dir".to_owned(),
            "evil/artifacts".to_owned(),
            "123".to_owned(),
        ],
        &worktree_root,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ),
        "expected PathNotAllowed for symlink-prefixed download dir, got {err:?}"
    );
}

/// A plain in-worktree relative destination is accepted by the worktree-root-aware
/// validator while the process cwd stays elsewhere (proving the root, not the
/// cwd, is what the gate resolves against). Pure function of `(args, root)`:
/// no `set_current_dir`, no `CWD_GUARD`.
#[cfg(unix)]
#[test]
fn gh_run_download_plain_relative_dir_accepted_against_worktree_root() {
    // Process cwd stays wherever the test harness launched; we never chdir.
    let worktree = tempfile::tempdir().unwrap();
    let worktree_root = worktree.path().canonicalize().unwrap();

    // A plain in-worktree relative destination (nonexistent nested dir) is fine.
    reject_external_gh_download_destination_in(
        &[
            "run".to_owned(),
            "download".to_owned(),
            "--dir".to_owned(),
            "sub/artifacts".to_owned(),
            "123".to_owned(),
        ],
        &worktree_root,
    )
    .unwrap();

    // An existing real (non-symlink) in-worktree directory is also accepted.
    std::fs::create_dir_all(worktree_root.join("real")).unwrap();
    reject_external_gh_download_destination_in(
        &[
            "run".to_owned(),
            "download".to_owned(),
            "--dir".to_owned(),
            "real".to_owned(),
            "123".to_owned(),
        ],
        &worktree_root,
    )
    .unwrap();

    // The pure-string gate still rejects absolute / `..` escapes here too (both
    // gates run in the worktree-root-aware entry point).
    for bad in ["/tmp/outside", "../outside"] {
        let err = reject_external_gh_download_destination_in(
            &[
                "run".to_owned(),
                "download".to_owned(),
                "--dir".to_owned(),
                bad.to_owned(),
                "123".to_owned(),
            ],
            &worktree_root,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                Error::PolicyViolation {
                    code: PolicyCode::PathNotAllowed,
                    ..
                }
            ),
            "expected PathNotAllowed for {bad}, got {err:?}"
        );
    }

    // A non-download command is a no-op even with an odd `--dir` elsewhere.
    reject_external_gh_download_destination_in(
        &["run".to_owned(), "view".to_owned(), "123".to_owned()],
        &worktree_root,
    )
    .unwrap();
}
