use super::*;
use crate::error::{Error, PolicyCode};

fn assert_policy_code(err: Error, expected: PolicyCode) {
    assert!(matches!(err, Error::PolicyViolation { code, .. } if code == expected));
}

#[test]
fn normalize_strips_path_and_exe() {
    for (input, want) in [
        ("/usr/bin/git", "git"),
        (r"C:\Program Files\git.exe", "git"),
        ("./gh", "gh"),
        ("GH.EXE", "gh"),
    ] {
        assert_eq!(normalize_program_name(input), want);
    }
}

#[test]
fn policy_retains_direct_git_and_checkout_guards() {
    for (program, args, expected) in [
        (
            "/usr/bin/git",
            vec!["push", "--force"],
            PolicyCode::BareForcePush,
        ),
        ("gh", vec!["pr", "checkout", "1"], PolicyCode::MergeBlocked),
        ("git", vec!["commit", "-m", "x"], PolicyCode::BranchMismatch),
    ] {
        assert_policy_code(
            check_command_policy(program, &args, &RunOptions::default()).unwrap_err(),
            expected,
        );
    }
}

#[test]
fn remote_gh_actions_need_no_checkout_or_expected_branch() {
    let options = RunOptions {
        allowlist: Some(OwnerAllowlist::parse("acme")),
        ..RunOptions::default()
    };
    for verb in [
        "create",
        "edit",
        "close",
        "reopen",
        "review",
        "merge",
        "ready",
        "update-branch",
    ] {
        check_command_policy("gh", &["pr", verb, "1", "--repo", "acme/project"], &options).unwrap();
    }
    assert_policy_code(
        check_command_policy("gh", &["pr", "merge", "1", "-Rother/project"], &options).unwrap_err(),
        PolicyCode::OwnerNotAllowed,
    );
}

#[cfg(unix)]
#[test]
fn remote_gh_pins_only_explicit_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    let replacement = root.path().join("replacement");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    let link = root.path().join("selected");
    std::os::unix::fs::symlink(&original, &link).unwrap();
    let options = RunOptions {
        repo: Some(link.clone()),
        allowlist: Some(OwnerAllowlist::parse("acme")),
        ..RunOptions::default()
    };
    let prepared = prepare_supervised_command(&CommandRequest {
        program: "gh",
        args: &["pr", "view", "1", "--repo", "acme/project"],
        options: &options,
    })
    .unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&replacement, &link).unwrap();
    assert_eq!(
        prepared.cwd.unwrap().canonicalize().unwrap(),
        original.canonicalize().unwrap()
    );
    assert!(prepared.branch_check.is_none());

    let no_directory = RunOptions {
        repo: None,
        ..options
    };
    let prepared = prepare_supervised_command(&CommandRequest {
        program: "gh",
        args: &["pr", "view", "1", "--repo", "acme/project"],
        options: &no_directory,
    })
    .unwrap();
    assert!(prepared.cwd.is_none());
    assert!(prepared.branch_check.is_none());
}

#[test]
fn supervisor_admits_host_scripts_and_interpreters() {
    for (program, args) in [
        ("sh", vec!["-c", "echo host-script"]),
        ("python3.11", vec!["-c", "print(1)"]),
        ("env", vec!["git", "status"]),
        ("./tools/run", vec![]),
        ("/opt/tools/run", vec![]),
    ] {
        check_command_policy(program, &args, &RunOptions::default()).unwrap();
    }
}

fn test_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
        vec!["commit", "--allow-empty", "-m", "base"],
        vec!["branch", "peer"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    dir
}

#[tokio::test]
async fn supervised_git_uses_harness_selected_checkout_and_assigned_branch() {
    let repo = test_repo();
    let options = RunOptions {
        repo: Some(repo.path().to_path_buf()),
        expected_branch: Some("main".into()),
        ..RunOptions::default()
    };
    let output = Supervisor::new(1)
        .run(
            "git",
            &["commit", "--allow-empty", "-m", "assigned"],
            None,
            &options,
        )
        .await
        .unwrap();
    assert!(output.succeeded(), "{output:?}");
    let wrong_branch = RunOptions {
        expected_branch: Some("other".into()),
        ..options
    };
    let err = Supervisor::new(1)
        .run(
            "git",
            &["commit", "--allow-empty", "-m", "must not commit"],
            None,
            &wrong_branch,
        )
        .await
        .unwrap_err();
    assert_policy_code(err, PolicyCode::BranchMismatch);
}

#[tokio::test]
async fn supervised_local_merge_preserves_dirty_wip() {
    let repo = test_repo();
    std::fs::write(repo.path().join("wip.txt"), "keep me").unwrap();
    let options = RunOptions {
        repo: Some(repo.path().to_path_buf()),
        expected_branch: Some("main".into()),
        ..RunOptions::default()
    };
    let err = Supervisor::new(1)
        .run("git", &["merge", "peer"], None, &options)
        .await
        .unwrap_err();
    assert_policy_code(err, PolicyCode::MergeBlocked);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("wip.txt")).unwrap(),
        "keep me"
    );
}

#[tokio::test]
async fn supervised_git_accepts_canonical_parent_components() {
    let repo = test_repo();
    let options = RunOptions {
        repo: Some(repo.path().join(".git").join("..")),
        ..RunOptions::default()
    };
    let output = Supervisor::new(1)
        .run("git", &["status", "--porcelain"], None, &options)
        .await
        .unwrap();
    assert!(output.succeeded(), "{output:?}");
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn supervised_host_shell_captures_output() {
    let (program, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let output = Supervisor::new(1)
        .run(
            program,
            &[flag, "echo host-script"],
            None,
            &RunOptions::default(),
        )
        .await
        .unwrap();
    assert!(output.succeeded(), "{output:?}");
    assert_eq!(output.stdout.trim(), "host-script");
}

#[cfg(unix)]
#[tokio::test]
async fn supervised_host_shell_remains_timeout_contained() {
    let output = Supervisor::new(1)
        .run(
            "sh",
            &["-c", "sleep 30"],
            Some(Duration::from_millis(100)),
            &RunOptions::default(),
        )
        .await
        .unwrap();
    assert!(output.timed_out && output.killed, "{output:?}");
}

#[tokio::test]
async fn recovery_still_rejects_checkout_and_bare_force_push() {
    let supervisor = Supervisor::new(1);
    let policy = TimeoutPolicy::default();
    for (program, args, code) in [
        ("gh", vec!["pr", "checkout", "1"], PolicyCode::MergeBlocked),
        ("git", vec!["push", "--force"], PolicyCode::BareForcePush),
    ] {
        assert_policy_code(
            supervisor
                .run_with_policy(program, &args, &policy, &RunOptions::default())
                .await
                .unwrap_err(),
            code,
        );
    }
}
