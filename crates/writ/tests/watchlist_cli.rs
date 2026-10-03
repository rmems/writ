use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::{TempDir, tempdir};
use writ_core::lease::{LeaseGrant, LeaseStore};

fn writ(root: &TempDir, args: &[&str]) -> (i32, String, String) {
    writ_with_store(&root.path().join("leases.db"), args)
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
        assert_json_store_error(path, verb, command, error_code);
        assert_human_store_error(path, verb);
    }
}

fn assert_json_store_error(path: &Path, verb: &str, command: &str, error_code: &str) {
    let (code, stdout, stderr) = writ_with_store(path, &["--json", "watchlist", verb]);
    assert_eq!(code, 1, "{verb}: stdout={stdout}; stderr={stderr}");
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        (
            envelope["ok"].as_bool(),
            envelope["command"].as_str(),
            envelope["error"]["code"].as_str(),
        ),
        (Some(false), Some(command), Some(error_code)),
        "{verb}: {envelope}"
    );
    assert!(
        envelope["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
}

fn assert_human_store_error(path: &Path, verb: &str) {
    let (code, stdout, stderr) = writ_with_store(path, &["watchlist", verb]);
    assert_eq!(code, 1, "{verb}: stdout={stdout}; stderr={stderr}");
    assert!(stdout.is_empty(), "{verb}: {stdout}");
    assert!(!stderr.is_empty(), "{verb}: missing human-readable error");
}

fn successful_json(output: (i32, String, String)) -> serde_json::Value {
    let (code, stdout, stderr) = output;
    assert_eq!(code, 0, "stdout={stdout}; stderr={stderr}");
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope
}

#[test]
fn watchlist_rejects_regular_file_ancestors() {
    let root = tempdir().unwrap();
    let parent = root.path().join("not-a-directory");
    fs::write(&parent, b"preserve me").unwrap();
    for path in [parent.join("leases.db"), parent.join("nested/leases.db")] {
        assert_store_error(&path, "IO_ERROR");
    }
    assert_eq!(fs::read(&parent).unwrap(), b"preserve me");
}

#[test]
fn watchlist_rejects_directory_store() {
    let root = tempdir().unwrap();
    let path = root.path().join("leases.db");
    fs::create_dir(&path).unwrap();
    assert_store_error(&path, "LEASE_STORE_FAILED");
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
}

#[test]
fn watchlist_rejects_corrupt_store_without_modifying_it() {
    let root = tempdir().unwrap();
    let path = root.path().join("leases.db");
    fs::write(&path, b"not a SQLite database").unwrap();
    assert_store_error(&path, "LEASE_STORE_FAILED");
    assert_eq!(fs::read(path).unwrap(), b"not a SQLite database");
}

#[test]
fn watchlist_missing_store_is_empty_without_creating_files() {
    let root = tempdir().unwrap();
    for path in [
        root.path().join("leases.db"),
        root.path().join("missing/nested/leases.db"),
    ] {
        for (verb, command) in VIEW_COMMANDS {
            let envelope = successful_json(writ_with_store(&path, &["--json", "watchlist", verb]));
            let snapshot = serde_json::json!({
                "command": envelope["command"],
                "entries": envelope["data"]["entries"],
                "coord_available": envelope["data"]["coord_available"],
                "github_probed": envelope["data"]["github_probed"],
                "error": envelope["error"],
            });
            assert_eq!(
                snapshot,
                serde_json::json!({
                    "command": command,
                    "entries": [],
                    "coord_available": false,
                    "github_probed": verb != "list",
                    "error": null,
                })
            );
        }
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn watchlist_reports_dangling_symlink_instead_of_empty_store() {
    let root = tempdir().unwrap();
    let path = root.path().join("leases.db");
    std::os::unix::fs::symlink("missing.db", &path).unwrap();
    assert_store_error(&path, "IO_ERROR");
    assert_eq!(fs::read_link(path).unwrap(), Path::new("missing.db"));
}

#[cfg(unix)]
#[test]
fn watchlist_reports_dangling_ancestor_symlink_instead_of_empty_store() {
    let root = tempdir().unwrap();
    let link = root.path().join("store-directory");
    std::os::unix::fs::symlink("missing-directory", &link).unwrap();
    for path in [link.join("leases.db"), link.join("nested/leases.db")] {
        assert_store_error(&path, "IO_ERROR");
    }
    assert_eq!(fs::read_link(link).unwrap(), Path::new("missing-directory"));
    assert!(!root.path().join("missing-directory").exists());
}

#[cfg(unix)]
#[test]
fn watchlist_reads_valid_store_symlink_without_modifying_it() {
    let root = tempdir().unwrap();
    seed_lease(&root);
    let store = root.path().join("leases.db");
    let original = fs::read(&store).unwrap();
    let link = root.path().join("store-link");
    std::os::unix::fs::symlink("leases.db", &link).unwrap();

    let envelope = successful_json(writ_with_store(&link, &["--json", "watchlist", "list"]));
    assert_eq!(envelope["data"]["entries"][0]["job_id"], "job-1");
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("leases.db"));
    assert_eq!(fs::read(&store).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn watchlist_missing_store_under_valid_directory_symlink_is_empty() {
    let root = tempdir().unwrap();
    let target = root.path().join("empty-directory");
    fs::create_dir(&target).unwrap();
    let link = root.path().join("directory-link");
    std::os::unix::fs::symlink("empty-directory", &link).unwrap();

    for (verb, _) in VIEW_COMMANDS {
        let envelope = successful_json(writ_with_store(
            &link.join("nested/leases.db"),
            &["--json", "watchlist", verb],
        ));
        assert_eq!(envelope["data"]["entries"], serde_json::json!([]));
    }
    assert_eq!(fs::read_link(link).unwrap(), Path::new("empty-directory"));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn watchlist_reports_symlink_loop_instead_of_empty_store() {
    let root = tempdir().unwrap();
    let path = root.path().join("leases.db");
    std::os::unix::fs::symlink("leases.db", &path).unwrap();
    assert_store_error(&path, "IO_ERROR");
    assert_eq!(fs::read_link(path).unwrap(), Path::new("leases.db"));
}

fn seed_lease(root: &TempDir) {
    let store = LeaseStore::open(root.path().join("leases.db")).unwrap();
    let wt = root.path().join("checkout");
    fs::create_dir_all(&wt).unwrap();
    store
        .grant(LeaseGrant {
            repo: &root.path().join("repo"),
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
        let root = tempdir().unwrap();
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
            assert!(!root.path().join("watchlist.json").exists());
        }
    }
}
