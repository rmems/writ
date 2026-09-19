use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

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

fn writ_hook(
    root: &std::path::Path,
    payload: &str,
    allowed_owners: Option<&str>,
) -> std::process::Output {
    let worktree_base = root.join("worktrees");
    let lease_path = root.join("leases.db");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_writ"));
    cmd.env("WRIT_WORKTREE_BASE", &worktree_base)
        .env("WRIT_LEASE_PATH", &lease_path)
        .env_remove("WRIT_ALLOWED_OWNERS")
        .env_remove("WH_ALLOWED_OWNERS");
    if let Some(owners) = allowed_owners {
        cmd.args(["--allowed-owners", owners]);
    }
    let mut child = cmd
        .args(["hook"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn hook_blocks_git_force_push_with_exit_2() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push --force origin main"}}"#,
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("BARE_FORCE_PUSH"), "stderr={stderr}");
}

#[test]
fn hook_blocks_quoted_force_push_and_config_injection() {
    let root = TestDir::new();
    let quoted = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push \"--force\""}}"#,
        None,
    );
    assert_eq!(quoted.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&quoted.stderr);
    assert!(stderr.contains("BARE_FORCE_PUSH"), "stderr={stderr}");

    let config = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git -c alias.status='!git push --force' status"}}"#,
        None,
    );
    assert_eq!(config.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&config.stderr);
    assert!(stderr.contains("SUBCOMMAND_NOT_ALLOWED"), "stderr={stderr}");
}

#[test]
fn hook_worktree_create_requires_source_ref() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        r#"{"hook_event_name":"WorktreeCreate","cwd":"/tmp","name":"wt"}"#,
        None,
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("START_POINT_REQUIRED"), "stderr={stderr}");
}

#[test]
fn hook_allows_git_status() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
        None,
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn install_is_idempotent() {
    let root = TestDir::new();
    let settings = root.0.join(".claude/settings.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_writ"))
            .args([
                "--json",
                "install",
                "--settings",
                settings.to_str().unwrap(),
                "--writ-bin",
                env!("CARGO_BIN_EXE_writ"),
            ])
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["data"]["changed"], true);

    let second = run();
    assert!(second.status.success());
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second_json["data"]["changed"], false);

    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        parsed["hooks"]["PreToolUse"][0]["hooks"][0]["if"],
        "Bash(git *)"
    );
}

#[test]
fn hook_boundary_fail_closed_cases() {
    let root = TestDir::new();
    for (payload, exit, needle) in [
        ("{", 2, "IO_ERROR"),
        (
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git merge origin/main"}}"#,
            2,
            "MERGE_BLOCKED",
        ),
        (
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push --force-with-lease origin HEAD"}}"#,
            0,
            "",
        ),
        (r#"{"hook_event_name":"SessionStart"}"#, 0, ""),
    ] {
        let output = writ_hook(&root.0, payload, None);
        assert_eq!(output.status.code(), Some(exit), "payload={payload}");
        if !needle.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains(needle), "needle={needle} stderr={stderr}");
        }
    }
}

#[test]
fn hook_global_allowed_owners_flag_enforces_gh_repo_targets() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh repo delete other/project --yes"}}"#,
        Some("acme"),
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OWNER_NOT_ALLOWED"), "stderr={stderr}");
}
