use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use writ_core::lease::{LeaseGrant, LeaseStore};

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

fn writ(root: &TestDir, args: &[&str]) -> (i32, String, String) {
    writ_with_store(&root.0.join("leases.db"), args)
}

fn writ_with_store(path: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .env("WRIT_LEASE_PATH", path)
        .env("WRIT_ALLOWED_OWNERS", "acme")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const VIEW_COMMANDS: [(&str, &str); 3] = [
    ("list", "cli.watchlist.list"),
    ("check", "cli.watchlist.check"),
    ("check-all", "cli.watchlist.check_all"),
];

fn assert_store_error(path: &Path, error_code: &str) {
    for (verb, command) in VIEW_COMMANDS {
        let (code, stdout, stderr) = writ_with_store(path, &["--json", "watchlist", verb]);
        assert_eq!(code, 1, "{verb}: stdout={stdout}; stderr={stderr}");
        let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(envelope["ok"], false, "{verb}: {envelope}");
        assert_eq!(envelope["command"], command);
        assert_eq!(envelope["error"]["code"], error_code);
        assert!(
            envelope["error"]["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );

        let (code, stdout, stderr) = writ_with_store(path, &["watchlist", verb]);
        assert_eq!(code, 1, "{verb}: stdout={stdout}; stderr={stderr}");
        assert!(stdout.is_empty(), "{verb}: {stdout}");
        assert!(!stderr.is_empty(), "{verb}: missing human-readable error");
    }
}

#[test]
fn watchlist_rejects_regular_file_ancestors() {
    let root = TestDir::new();
    let parent = root.0.join("not-a-directory");
    fs::write(&parent, b"preserve me").unwrap();
    for path in [parent.join("leases.db"), parent.join("nested/leases.db")] {
        assert_store_error(&path, "IO_ERROR");
    }
    assert_eq!(fs::read(&parent).unwrap(), b"preserve me");
}

#[test]
fn watchlist_rejects_directory_store() {
    let root = TestDir::new();
    let path = root.0.join("leases.db");
    fs::create_dir(&path).unwrap();
    assert_store_error(&path, "LEASE_STORE_FAILED");
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}

#[test]
fn watchlist_rejects_corrupt_store_without_modifying_it() {
    let root = TestDir::new();
    let path = root.0.join("leases.db");
    fs::write(&path, b"not a SQLite database").unwrap();
    assert_store_error(&path, "LEASE_STORE_FAILED");
    assert_eq!(fs::read(path).unwrap(), b"not a SQLite database");
}

#[test]
fn watchlist_missing_store_is_empty_without_creating_files() {
    let root = TestDir::new();
    for path in [
        root.0.join("leases.db"),
        root.0.join("missing/nested/leases.db"),
    ] {
        for (verb, command) in VIEW_COMMANDS {
            let (code, stdout, stderr) = writ_with_store(&path, &["--json", "watchlist", verb]);
            assert_eq!(code, 0, "{verb}: stderr={stderr}");
            let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(envelope["ok"], true);
            assert_eq!(envelope["command"], command);
            assert_eq!(envelope["data"]["entries"], serde_json::json!([]));
            assert_eq!(envelope["data"]["coord_available"], false);
            assert_eq!(envelope["data"]["github_probed"], verb != "list");
            assert!(envelope["error"].is_null());
        }
    }
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn watchlist_reports_dangling_symlink_instead_of_empty_store() {
    let root = TestDir::new();
    let path = root.0.join("leases.db");
    std::os::unix::fs::symlink("missing.db", &path).unwrap();
    assert_store_error(&path, "IO_ERROR");
    assert_eq!(fs::read_link(path).unwrap(), Path::new("missing.db"));
}

#[cfg(unix)]
#[test]
fn watchlist_reports_symlink_loop_instead_of_empty_store() {
    let root = TestDir::new();
    let path = root.0.join("leases.db");
    std::os::unix::fs::symlink("leases.db", &path).unwrap();
    assert_store_error(&path, "IO_ERROR");
    assert_eq!(fs::read_link(path).unwrap(), Path::new("leases.db"));
}

fn seed_lease(root: &TestDir) {
    let store = LeaseStore::open(root.0.join("leases.db")).unwrap();
    let wt = root.0.join("checkout");
    fs::create_dir_all(&wt).unwrap();
    store
        .grant(LeaseGrant {
            repo: &root.0.join("repo"),
            owner: "acme",
            repo_name: "sample",
            job_id: "job-1",
            branch: "hive/job-1",
            worktree_path: &wt,
            start_commit: "abc123",
        })
        .unwrap();
}

struct CliCase {
    args: &'static [&'static str],
    seed: bool,
    needles: &'static [&'static str],
    no_json_store: bool,
}

#[test]
fn watchlist_cli_cases() {
    let cases = [
        CliCase {
            args: &["--json", "watchlist", "list"],
            seed: true,
            needles: &["cli.watchlist.list", "job-1", "running"],
            no_json_store: true,
        },
        CliCase {
            args: &["--json", "watchlist", "add"],
            seed: false,
            needles: &["\"persisted\":false"],
            no_json_store: true,
        },
        CliCase {
            args: &["watchlist", "list"],
            seed: false,
            needles: &["writ worktree register"],
            no_json_store: false,
        },
        CliCase {
            args: &["--json", "watchlist", "check-all"],
            seed: false,
            needles: &["cli.watchlist.check_all", "\"github_probed\":true"],
            no_json_store: false,
        },
        CliCase {
            args: &["watchlist", "remove"],
            seed: false,
            needles: &["leases.db"],
            no_json_store: false,
        },
        CliCase {
            args: &["watchlist", "list"],
            seed: true,
            needles: &["job-1", "running", "live"],
            no_json_store: false,
        },
    ];
    for case in cases {
        let root = TestDir::new();
        if case.seed {
            seed_lease(&root);
        }
        let (code, stdout, stderr) = writ(&root, case.args);
        assert_eq!(code, 0, "args={:?} stderr={stderr}", case.args);
        for needle in case.needles {
            assert!(
                stdout.contains(needle),
                "missing {needle} in {stdout} for {:?}",
                case.args
            );
        }
        if case.no_json_store {
            assert!(!root.0.join("watchlist.json").exists());
        }
    }
}
