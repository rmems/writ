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
fn gh_pr_update_branch_rejected() {
    let err = SafeGhCommand::new(&["pr".to_owned(), "update-branch".to_owned(), "1".to_owned()])
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
fn gh_pr_merge_after_repo_flag_rejected() {
    let err = SafeGhCommand::new(&[
        "pr".to_owned(),
        "-R".to_owned(),
        "acme/widgets".to_owned(),
        "merge".to_owned(),
        "1".to_owned(),
        "-m".to_owned(),
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
fn gh_pr_merge_after_equals_repo_flag_rejected() {
    let err = SafeGhCommand::new(&[
        "pr".to_owned(),
        "-R=acme/widgets".to_owned(),
        "merge".to_owned(),
        "1".to_owned(),
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
fn gh_pr_merge_rejected() {
    let err = SafeGhCommand::new(&["pr".to_owned(), "merge".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn gh_pr_ready_rejected() {
    let err = SafeGhCommand::new(&["pr".to_owned(), "ready".to_owned()]).unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            ..
        }
    ));
}

#[test]
fn gh_merge_flag_rejected() {
    for args in [
        vec!["pr".to_owned(), "create".to_owned(), "--merge".to_owned()],
        vec![
            "pr".to_owned(),
            "view".to_owned(),
            "1".to_owned(),
            "--merge-queue".to_owned(),
        ],
    ] {
        let err = SafeGhCommand::new(&args).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::GhFlagNotAllowed,
                ..
            }
        ));
    }
}

#[test]
fn gh_api_rejected() {
    // api removed from allowlist to block merge via REST/GraphQL.
    let err = SafeGhCommand::new(&[
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        "query=mutation { mergePullRequest }".to_owned(),
    ])
    .unwrap_err();
    assert!(matches!(
        err,
        Error::PolicyViolation {
            code: PolicyCode::GhSubcommandNotAllowed,
            ..
        }
    ));
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
    let _ = std::fs::remove_dir_all(&worker_a);
    let _ = std::fs::remove_dir_all(&worker_b);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn merge_on_default_branch_is_blocked() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "worker-a".to_owned()]).unwrap();
    let err = cmd.run(&repo, None).unwrap_err();
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
    assert!(format!("{err}").contains("default branch"), "{err}");
    let _ = std::fs::remove_dir_all(&worker_a);
    let _ = std::fs::remove_dir_all(&worker_b);
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn merge_refuses_to_lose_uncommitted_wip() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    std::fs::write(worker_b.join("wip.txt"), "keep-me\n").unwrap();
    let cmd = SafeGitCommand::new(&["merge".to_owned(), "worker-a".to_owned()]).unwrap();
    let err = cmd.run(&worker_b, Some("worker-b")).unwrap_err();
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
    assert!(format!("{err}").contains("uncommitted work"), "{err}");
    assert_eq!(
        std::fs::read_to_string(worker_b.join("wip.txt")).unwrap(),
        "keep-me\n"
    );
    assert!(
        !worker_b.join("a.txt").exists(),
        "peer file must not appear after refused merge"
    );
    let _ = std::fs::remove_dir_all(&worker_a);
    let _ = std::fs::remove_dir_all(&worker_b);
    let _ = std::fs::remove_dir_all(&repo);
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
fn pull_on_default_branch_is_blocked() {
    let (repo, worker_a, worker_b) = two_assigned_worktrees();
    let cmd = SafeGitCommand::new(&[
        "pull".to_owned(),
        "--rebase".to_owned(),
        "origin".to_owned(),
        "main".to_owned(),
    ])
    .unwrap();
    let err = cmd.run(&repo, None).unwrap_err();
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
    assert!(format!("{err}").contains("default branch"), "{err}");
    let _ = std::fs::remove_dir_all(&worker_a);
    let _ = std::fs::remove_dir_all(&worker_b);
    let _ = std::fs::remove_dir_all(&repo);
}
