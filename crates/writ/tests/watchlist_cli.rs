use std::fs;
use std::path::PathBuf;
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
    let output = Command::new(env!("CARGO_BIN_EXE_writ"))
        .args(args)
        .env("WRIT_LEASE_PATH", root.0.join("leases.db"))
        .env("WRIT_ALLOWED_OWNERS", "acme")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
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
