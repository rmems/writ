//! Hook-boundary contract tests for GitHub #81 / Linear RM-348.
//!
//! These tests feed Claude Code fixture payloads to `writ hook` on stdin.
//! They do not require a live Claude Code session.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use writ_core::hook::{ComposedPreToolUse, HookOutcome, compose_pre_tool_use};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("writ-hook-cli-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn init_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "trunk"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

fn writ_hook(root: &Path, payload: &serde_json::Value) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_writ"))
        .arg("hook")
        .env("WRIT_LEASE_PATH", root.join("leases.sqlite"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_BIN", root.join("writ-bin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn outcome(output: &Output) -> HookOutcome {
    HookOutcome {
        exit_code: output.status.code().unwrap_or(255) as u8,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn bash(command: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command}
    })
}

/// Run `writ hook` for a WorktreeCreate `payload` expected to be rejected, and
/// assert the shared invariant shared by every rejection test: the hook exits
/// nonzero and no lease row is granted (zero rows if the db was created at all).
/// The `Output` is returned so each test keeps its own unique assertions
/// (stderr substring, escape path, worktree dir).
fn assert_worktree_create_rejected_without_lease(
    root: &Path,
    payload: &serde_json::Value,
) -> Output {
    let output = writ_hook(root, payload);
    assert_ne!(output.status.code(), Some(0));
    let lease_path = root.join("leases.sqlite");
    if lease_path.exists() {
        let conn = rusqlite::Connection::open(&lease_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM leases", [], |row| row.get(0))
            .unwrap_or(0);
        assert_eq!(count, 0);
    }
    output
}

#[test]
fn pretooluse_exit_2_blocks_with_stderr_reason() {
    let root = TestDir::new();
    let output = writ_hook(&root.0, &bash("git merge feature"));
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("MERGE_BLOCKED"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn competing_allow_hook_does_not_override_exit_2() {
    let root = TestDir::new();
    let blocked = outcome(&writ_hook(&root.0, &bash("git push --force")));
    let competitor = HookOutcome {
        exit_code: 0,
        stdout:
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}"#
                .into(),
        stderr: String::new(),
    };
    match compose_pre_tool_use(&[blocked, competitor]) {
        ComposedPreToolUse::Block { reason } => {
            assert!(reason.contains("BARE_FORCE_PUSH"));
        }
        ComposedPreToolUse::Continue => panic!("JSON allow must not override exit 2"),
    }
}

#[test]
fn worktree_create_grants_a_lease_row_queryable_in_sqlite() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "success",
        "owner": "acme",
        "repo": "sample",
        "job_id": "success",
        "branch": "feature/success",
        "start_point": start,
    });
    let output = writ_hook(&root.0, &payload);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(Path::new(&path).is_dir(), "stdout path {path}");

    let conn = rusqlite::Connection::open(root.0.join("leases.sqlite")).unwrap();
    let (branch, mode): (String, String) = conn
        .query_row(
            "SELECT branch, mode FROM leases WHERE worktree_path = ?1",
            [path.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(branch, "feature/success");
    assert_eq!(mode, "WRITER_LOCKED");
}

#[test]
fn worktree_create_refuses_a_lease_on_hook_config() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "settings.json",
        "owner": "acme",
        "repo": ".claude",
        "job_id": "settings.json",
        "branch": "feature/protected",
        "start_point": start,
    });
    let output = assert_worktree_create_rejected_without_lease(&root.0, &payload);
    assert!(String::from_utf8_lossy(&output.stderr).contains("PROTECTED_PATH"));
}

#[test]
fn newline_separated_git_merge_is_blocked() {
    let root = TestDir::new();
    let output = writ_hook(&root.0, &bash("git status\ngit merge feature"));
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn worktree_create_rejects_path_escape() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "../escape",
        "owner": "acme",
        "repo": "sample",
        "job_id": "../escape",
        "branch": "feature/escape",
        "start_point": start,
    });
    let _output = assert_worktree_create_rejected_without_lease(&root.0, &payload);
    assert!(!root.0.join("worktrees").join("..").join("escape").exists());
}

#[test]
fn worktree_create_nonzero_aborts_without_a_lease_row() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "invalid",
        "owner": "acme",
        "repo": "sample",
        "job_id": "invalid",
        "branch": "feature/invalid",
        "start_point": "does-not-exist",
    });
    let _output = assert_worktree_create_rejected_without_lease(&root.0, &payload);
    assert!(!root.0.join("worktrees/acme/sample/invalid").exists());
}

#[test]
fn worktree_remove_releases_the_lease_row() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let create = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "remove-me",
        "owner": "acme",
        "repo": "sample",
        "job_id": "remove-me",
        "branch": "feature/remove-me",
        "start_point": start,
    });
    let created = writ_hook(&root.0, &create);
    assert_eq!(
        created.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let path = String::from_utf8_lossy(&created.stdout).trim().to_owned();

    let remove = serde_json::json!({
        "hook_event_name": "WorktreeRemove",
        "worktree_path": path,
    });
    let removed = writ_hook(&root.0, &remove);
    assert_eq!(
        removed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );

    let conn = rusqlite::Connection::open(root.0.join("leases.sqlite")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM leases WHERE worktree_path = ?1",
            [path.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn malformed_hook_json_fails_closed() {
    let root = TestDir::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_writ"))
        .arg("hook")
        .env("WRIT_LEASE_PATH", root.0.join("leases.sqlite"))
        .env("WRIT_WORKTREE_BASE", root.0.join("worktrees"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"{").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("malformed hook JSON")
            || String::from_utf8_lossy(&output.stderr).contains("failed closed")
    );
}

#[test]
fn unrecognized_event_exits_zero_without_a_decision() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        &serde_json::json!({"hook_event_name": "PostCompact"}),
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn false_positive_corpus_passes_the_binary() {
    let root = TestDir::new();
    for command in writ_core::hook::false_positive_corpus() {
        let output = writ_hook(&root.0, &bash(command));
        assert_eq!(
            output.status.code(),
            Some(0),
            "false positive on {command}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn merge_spellings_are_blocked_at_the_hook() {
    let root = TestDir::new();
    for command in [
        "git merge other",
        "git mergetool",
        "git pull origin main",
        "gh pr merge 1",
        "gh pr merge --auto 1",
        "gh pr merge --merge-queue 1",
        "gh api graphql -f query=mutation",
    ] {
        let output = writ_hook(&root.0, &bash(command));
        assert_eq!(
            output.status.code(),
            Some(2),
            "{command} must be blocked: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn bare_force_is_blocked_and_force_with_lease_is_permitted() {
    let root = TestDir::new();
    let blocked = writ_hook(&root.0, &bash("git push -f origin feature/assigned"));
    assert_eq!(blocked.status.code(), Some(2));
    let permitted = writ_hook(
        &root.0,
        &bash("git push --force-with-lease origin feature/assigned"),
    );
    assert_eq!(
        permitted.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&permitted.stderr)
    );
}

#[test]
fn protected_path_rewrite_is_blocked() {
    let root = TestDir::new();
    let output = writ_hook(&root.0, &bash("tee .claude/settings.local.json"));
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("PROTECTED_PATH"));
}

#[test]
fn wrapper_launching_git_merge_is_blocked() {
    let root = TestDir::new();
    let output = writ_hook(&root.0, &bash("sh -c \"git merge feature\""));
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn cli_exposes_no_merge_subcommand() {
    let help = Command::new(env!("CARGO_BIN_EXE_writ"))
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("hook"));
    assert!(
        !text.split_whitespace().any(|word| word == "merge"),
        "CLI help must not expose a merge command: {text}"
    );
}

#[test]
fn worktree_create_reports_typed_residual_evidence_on_partial_failure() {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let occupied = root.0.join("worktrees/acme/sample/partial");
    fs::create_dir_all(&occupied).unwrap();
    fs::write(occupied.join("occupied"), "keep\n").unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": repo,
        "name": "partial",
        "owner": "acme",
        "repo": "sample",
        "job_id": "partial",
        "branch": "feature/partial",
        "start_point": start,
    });
    let output = writ_hook(&root.0, &payload);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("path_exists=true") || stderr.contains("residual_state"),
        "{stderr}"
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/feature/partial"]),
        start
    );
}
