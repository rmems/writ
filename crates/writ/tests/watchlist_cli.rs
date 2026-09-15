use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-watchlist-cli-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn writ(args: &[&str], state: &PathBuf) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}

fn sample_watchlist() -> String {
    r#"{
  "version": 1,
  "prs": [
    {
      "repo": "acme/widgets",
      "number": 7,
      "branch": "feat/a",
      "status": "pending",
      "last_checked": "2026-01-01T00:00:00Z",
      "fix_count": 1,
      "residual_blockers": ["class_b:review"],
      "stack_id": "s1",
      "stack_position": 0,
      "base": "main",
      "title": "Add widgets"
    },
    {
      "repo": "example-org/core",
      "number": 9,
      "branch": "fix/b",
      "status": "healthy",
      "last_checked": "2026-01-02T00:00:00Z",
      "fix_count": 0,
      "residual_blockers": []
    }
  ],
  "groups": {
    "s1": { "repo": "acme/widgets", "numbers": [7] }
  }
}
"#
    .to_owned()
}

#[test]
fn list_prints_all_owners_and_json_envelope() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();

    let human = writ(&["watchlist", "list"], &state);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("acme/widgets"));
    assert!(stdout.contains("example-org/core"));
    assert!(stdout.contains("class_b:review"));

    let json = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(["--json", "watchlist", "list", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(json.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["command"], "watchlist.list");
    assert_eq!(payload["data"]["prs"].as_array().unwrap().len(), 2);
}

#[test]
fn list_filters_by_owner() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(["watchlist", "list", "--owner", "acme", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("acme/widgets"));
    assert!(!stdout.contains("example-org/core"));
}

#[test]
fn remove_one_entry_keeps_the_other() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args([
            "watchlist",
            "remove",
            "--repo",
            "acme/widgets",
            "7",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = writ(&["watchlist", "list"], &state);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(!stdout.contains("acme/widgets"));
    assert!(stdout.contains("example-org/core"));
}

#[test]
fn missing_file_lists_empty() {
    let dir = TestDir::new();
    let state = dir.0.join("missing.json");
    let out = writ(&["watchlist", "list"], &state);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No watched pull requests.\n"
    );
}

#[test]
fn corrupt_file_exits_nonzero_and_quarantines() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, "{nope").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(["--json", "watchlist", "list", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "CORRUPT_STATE");
    assert!(!state.exists());
    let quarantined = fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
    assert!(quarantined);
}

#[test]
fn import_pr_babysit_is_read_only_on_source() {
    let dir = TestDir::new();
    let source = dir.0.join("watched-prs.json");
    let state = dir.0.join("watchlist.json");
    fs::write(
        &source,
        r#"{
          "prs": [{
            "repo": "acme/widgets",
            "number": 3,
            "branch": "feat/import",
            "last_status": "failed",
            "fix_count": 2
          }]
        }"#,
    )
    .unwrap();
    let original = fs::read_to_string(&source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(["watchlist", "import-pr-babysit", "--path"])
        .arg(&source)
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    let listed = writ(&["watchlist", "list"], &state);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("acme/widgets"));
    assert!(stdout.contains("feat/import"));
}

#[test]
fn check_missing_entry_with_repo_is_not_found() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args([
            "--json",
            "watchlist",
            "check",
            "--repo",
            "acme/widgets",
            "999",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "NOT_FOUND");
}

#[test]
fn check_malformed_repo_is_invalid_input() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args([
            "--json",
            "watchlist",
            "check",
            "--repo",
            "not-a-slug",
            "7",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "INVALID_INPUT");
}

#[test]
fn list_repo_filter_ignores_slug_case() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(["watchlist", "list", "--repo", "ACME/widgets", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("acme/widgets"));
    assert!(!stdout.contains("example-org/core"));
}

#[test]
fn check_all_without_allowlist_is_policy_exit() {
    let dir = TestDir::new();
    let state = dir.0.join("watchlist.json");
    fs::write(&state, sample_watchlist()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_writ"))
        .env_remove("WRIT_ALLOWED_OWNERS")
        .env_remove("WH_ALLOWED_OWNERS")
        .args(["--json", "watchlist", "check-all", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["error"]["code"], "OWNER_ALLOWLIST_REQUIRED");
}
