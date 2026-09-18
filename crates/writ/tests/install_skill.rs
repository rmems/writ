#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-install-skill-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn script() -> PathBuf {
    repo_root().join("scripts/install-skill.sh")
}

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new("bash")
        .arg(script())
        .args(args)
        .env("HOME", home)
        .env_remove("WRIT_CLONE")
        .env_remove("WORKTREES_HIVES_CLONE")
        .output()
        .unwrap()
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "status={:?} stdout={} stderr={}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn skill_link(home: &Path, root: &str) -> PathBuf {
    home.join(root).join("skills/writ")
}

fn fake_clone(root: &Path) -> PathBuf {
    let clone = root.join("clone");
    fs::create_dir_all(&clone).unwrap();
    fs::write(clone.join("SKILL.md"), "# writ\n").unwrap();
    clone.canonicalize().unwrap()
}

fn assert_linked_to(home: &Path, root: &str, clone: &Path) {
    let link = skill_link(home, root);
    let dest = fs::read_link(&link).unwrap_or_else(|err| panic!("read_link {link:?}: {err}"));
    let dest_canon = dest.canonicalize().unwrap();
    assert_eq!(dest_canon, clone, "symlink {link:?} -> {dest:?}");
    assert!(
        link.join("SKILL.md").is_file(),
        "SKILL.md missing under {link:?}"
    );
}

#[test]
fn default_root_is_idempotent() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = repo_root();
    let clone_str = clone.to_str().unwrap();

    let first = run(home, &["--clone-dir", clone_str]);
    assert_success(&first);
    assert_linked_to(home, ".agents", &clone);
    assert!(stdout(&first).contains("/skills writ"));

    let second = run(home, &["--clone-dir", clone_str]);
    assert_success(&second);
    assert!(stdout(&second).contains("already"));
    assert_linked_to(home, ".agents", &clone);
}

#[test]
fn refuses_non_symlink_conflict_without_force() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = repo_root();
    let target_parent = home.join(".agents/skills");
    fs::create_dir_all(&target_parent).unwrap();
    fs::write(target_parent.join("writ"), "not a skill tree").unwrap();

    let output = run(home, &["--clone-dir", clone.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("pass --force"),
        "stderr={}",
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(target_parent.join("writ")).unwrap(),
        "not a skill tree"
    );
}

#[test]
fn force_replaces_conflict() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = repo_root();
    let target_parent = home.join(".agents/skills");
    fs::create_dir_all(&target_parent).unwrap();
    fs::write(target_parent.join("writ"), "not a skill tree").unwrap();

    let output = run(home, &["--force", "--clone-dir", clone.to_str().unwrap()]);
    assert_success(&output);
    assert_linked_to(home, ".agents", &clone);
}

#[test]
fn all_roots_share_one_clone() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = repo_root();
    let output = run(
        home,
        &["--root", "all", "--clone-dir", clone.to_str().unwrap()],
    );
    assert_success(&output);
    for root in [".agents", ".grok", ".cline", ".claude", ".cursor"] {
        assert_linked_to(home, root, &clone);
        let dest = fs::read_link(skill_link(home, root))
            .unwrap()
            .canonicalize()
            .unwrap();
        assert_eq!(dest, clone);
    }
    assert!(!skill_link(home, ".codex").exists());
}

#[test]
fn unknown_root_errors() {
    let tmp = TestDir::new();
    let output = run(
        tmp.0.as_path(),
        &[
            "--root",
            "nope",
            "--clone-dir",
            repo_root().to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown --root"));
}

#[test]
fn missing_skill_md_errors() {
    let tmp = TestDir::new();
    let empty = tmp.0.join("empty-clone");
    fs::create_dir_all(&empty).unwrap();
    let output = run(tmp.0.as_path(), &["--clone-dir", empty.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("SKILL.md not found"));
}

#[test]
fn force_replaces_symlink_to_other_path() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let other = tmp.0.join("other");
    fs::create_dir_all(&other).unwrap();
    let parent = home.join(".agents/skills");
    fs::create_dir_all(&parent).unwrap();
    symlink(&other, parent.join("writ")).unwrap();

    let without = run(home, &["--clone-dir", repo_root().to_str().unwrap()]);
    assert!(!without.status.success());

    let with_force = run(
        home,
        &["--force", "--clone-dir", repo_root().to_str().unwrap()],
    );
    assert_success(&with_force);
    assert_linked_to(home, ".agents", &repo_root());
}

#[test]
fn writ_clone_env_selects_checkout() {
    let tmp = TestDir::new();
    let home = tmp.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let output = Command::new("bash")
        .arg(script())
        .env("HOME", &home)
        .env("WRIT_CLONE", repo_root())
        .env_remove("WORKTREES_HIVES_CLONE")
        .output()
        .unwrap();
    assert_success(&output);
    assert_linked_to(&home, ".agents", &repo_root());
}

#[test]
fn comma_separated_roots() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = repo_root();
    let output = run(
        home,
        &[
            "--root",
            "grok,cline",
            "--clone-dir",
            clone.to_str().unwrap(),
        ],
    );
    assert_success(&output);
    assert_linked_to(home, ".grok", &clone);
    assert_linked_to(home, ".cline", &clone);
    assert!(!skill_link(home, ".agents").exists());
}

#[test]
fn clone_already_at_skill_path_is_ok() {
    let tmp = TestDir::new();
    let clone = tmp.0.join(".agents/skills/writ");
    fs::create_dir_all(&clone).unwrap();
    fs::write(clone.join("SKILL.md"), "# writ\n").unwrap();
    let clone = clone.canonicalize().unwrap();

    let output = run(&tmp.0, &["--clone-dir", clone.to_str().unwrap()]);
    assert_success(&output);
    assert!(stdout(&output).contains("no symlink needed"));
    assert!(clone.join("SKILL.md").is_file());
    let meta = clone.symlink_metadata().unwrap();
    assert!(meta.file_type().is_dir());
    assert!(!meta.file_type().is_symlink());
}

#[test]
fn force_refuses_to_delete_clone_nested_under_skill_path() {
    let tmp = TestDir::new();
    let home = &tmp.0;
    let clone = home.join(".agents/skills/writ/nested");
    fs::create_dir_all(&clone).unwrap();
    fs::write(clone.join("SKILL.md"), "# writ\n").unwrap();

    let output = run(home, &["--force", "--clone-dir", clone.to_str().unwrap()]);
    assert!(!output.status.success(), "stderr={}", stderr(&output));
    assert!(
        stderr(&output).contains("contains this clone"),
        "stderr={}",
        stderr(&output)
    );
    assert!(clone.join("SKILL.md").is_file());
}

#[test]
fn refuses_skill_root_inside_clone() {
    let tmp = TestDir::new();
    let clone = fake_clone(&tmp.0);
    let output = run(&clone, &["--clone-dir", clone.to_str().unwrap()]);
    assert!(!output.status.success(), "stderr={}", stderr(&output));
    assert!(
        stderr(&output).contains("inside this clone"),
        "stderr={}",
        stderr(&output)
    );
    assert!(!clone.join(".agents").exists());
}
