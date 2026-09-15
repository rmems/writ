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

fn writ_hook(root: &std::path::Path, payload: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.join("leases.db"))
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
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("BARE_FORCE_PUSH"), "stderr={stderr}");
}

#[test]
fn hook_allows_git_status() {
    let root = TestDir::new();
    let output = writ_hook(
        &root.0,
        r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
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
