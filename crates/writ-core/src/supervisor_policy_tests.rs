use std::path::PathBuf;

use crate::error::{Error, PolicyCode};

use super::*;

fn platform_pair(windows: &'static str, unix: &'static str) -> &'static str {
    let pair = (windows, unix);
    if cfg!(windows) { pair.0 } else { pair.1 }
}

fn assert_policy_code(err: Error, expected: PolicyCode) {
    assert!(matches!(
        err,
        Error::PolicyViolation { code, .. } if code == expected
    ));
}

fn assert_default_policy(program: &str, args: &[&str], expected: PolicyCode) {
    assert_policy_code(
        check_command_policy(program, args, &RunOptions::default()).unwrap_err(),
        expected,
    );
}

fn normalize_cases() -> &'static [(&'static str, &'static str)] {
    &[
        ("/usr/bin/git", "git"),
        (r"C:\Program Files\git.exe", "git"),
        ("./gh", "gh"),
        ("GH.EXE", "gh"),
    ]
}

#[test]
fn normalize_strips_path_and_exe() {
    for (input, want) in normalize_cases() {
        assert_eq!(normalize_program_name(input), *want);
    }
}

fn git_gh_unsafe_policy_cases() -> &'static [(&'static str, &'static [&'static str], PolicyCode)] {
    &[
        (
            "/usr/bin/git",
            &["push", "--force"],
            PolicyCode::BareForcePush,
        ),
        ("gh", &["pr", "merge"], PolicyCode::MergeBlocked),
        ("git", &["commit", "-m", "x"], PolicyCode::BranchMismatch),
        ("./tools/run", &[], PolicyCode::SubcommandNotAllowed),
    ]
}

fn wrapper_unsafe_policy_cases() -> &'static [(&'static str, &'static [&'static str], PolicyCode)] {
    &[
        (
            "sh",
            &["-c", "gh pr merge 1"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "setsid",
            &["gh", "pr", "merge"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "env",
            &["gh", "pr", "merge"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "python3.11",
            &["-c", "print(1)"],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "python3",
            &[
                "-c",
                "import subprocess; subprocess.run(['gh','pr','merge','1'])",
            ],
            PolicyCode::SubcommandNotAllowed,
        ),
        (
            "curl",
            &[
                "-X",
                "PUT",
                "https://api.github.com/repos/o/r/pulls/1/merge",
            ],
            PolicyCode::SubcommandNotAllowed,
        ),
    ]
}

#[test]
fn policy_blocks_known_unsafe_invocations() {
    for cases in [git_gh_unsafe_policy_cases(), wrapper_unsafe_policy_cases()] {
        for (program, args, expected) in cases {
            assert_default_policy(program, args, *expected);
        }
    }
}

/// Fixed path outside the default worktree sandbox (not `temp_dir`, which Codacy
/// flags for security-sensitive policy tests).
fn repo_outside_worktree_base_fixture() -> PathBuf {
    platform_pair(r"C:\Windows\Temp", "/tmp").into()
}

#[test]
fn mutating_git_rejects_repo_outside_default_worktree_base() {
    let repo = repo_outside_worktree_base_fixture();
    assert_policy_code(
        check_command_policy(
            "git",
            &["commit", "-m", "x"],
            &RunOptions {
                expected_branch: Some("feature".to_owned()),
                repo: Some(repo),
                ..RunOptions::default()
            },
        )
        .unwrap_err(),
        PolicyCode::PathNotAllowed,
    );
}

#[tokio::test]
async fn recovery_still_rejects_merge_and_bare_force_push() {
    let supervisor = Supervisor::new(1);
    let policy = TimeoutPolicy {
        idle: Some(Duration::from_millis(50)),
        grace: Duration::from_millis(20),
        ..TimeoutPolicy::default()
    };
    let merge = supervisor
        .run_with_policy("gh", &["pr", "merge"], &policy, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(merge, PolicyCode::MergeBlocked);
    let force = supervisor
        .run_with_policy("git", &["push", "--force"], &policy, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(force, PolicyCode::BareForcePush);
}

#[tokio::test]
async fn run_rejects_merge_before_spawn() {
    let err = Supervisor::new(1)
        .run("gh", &["pr", "merge"], None, &RunOptions::default())
        .await
        .unwrap_err();
    assert_policy_code(err, PolicyCode::MergeBlocked);
}
