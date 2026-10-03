use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("writ-worktree-cli-{}-{id}", std::process::id()));
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
    // create binds the requested owner to the repository origin; the CLI tests
    // request the `acme` owner, so the origin must belong to `acme`.
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/sample.git",
        ],
    );
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

fn primed() -> (TestDir, PathBuf) {
    let root = TestDir::new();
    let repo = init_repo(&root.0);
    (root, repo)
}

struct CreateRequest<'a> {
    job: &'a str,
    branch: &'a str,
    start: &'a str,
}

fn writ_cmd(root: &Path, create_args: &[&str]) -> Output {
    writ_cmd_with_owners(root, create_args, Some("acme"))
}

fn writ_cmd_with_owners(root: &Path, create_args: &[&str], allowed_owners: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_writ"));
    command
        .env("WRIT_WORKTREE_BASE", root.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.join("leases.db"))
        .env_remove("WRIT_ALLOWED_OWNERS")
        .env_remove("WH_ALLOWED_OWNERS");
    if let Some(owners) = allowed_owners {
        command.env("WRIT_ALLOWED_OWNERS", owners);
    }
    command
        .args(["--json", "worktree", "create"])
        .args(create_args)
        .output()
        .unwrap()
}

fn identity_args<'a>(repo: &'a Path, request: &CreateRequest<'a>) -> Vec<&'a str> {
    vec![
        "--schema-version",
        "2",
        "--repo",
        repo.to_str().unwrap(),
        "--start-point",
        request.start,
        "acme",
        "sample",
        request.job,
        request.branch,
    ]
}

fn writ_create(root: &Path, repo: &Path, request: CreateRequest<'_>) -> Output {
    writ_cmd(root, &identity_args(repo, &request))
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not a JSON envelope: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_error_envelope(
    output: &Output,
    expected_exit: i32,
    expected_schema: u64,
    expected_code: &str,
) -> serde_json::Value {
    let envelope = json(output);
    assert_eq!(output.status.code(), Some(expected_exit));
    assert_eq!(
        serde_json::json!({
            "ok": false,
            "schema_version": expected_schema,
            "command": "worktree.create",
            "error_code": expected_code,
        }),
        serde_json::json!({
            "ok": envelope["ok"],
            "schema_version": envelope["schema_version"],
            "command": envelope["command"],
            "error_code": envelope["error"]["code"],
        })
    );
    envelope
}

fn assert_uncreated(root: &Path, repo: &Path, job: &str, branch: &str) {
    assert!(
        !root
            .join("worktrees")
            .join("acme")
            .join("sample")
            .join(job)
            .exists()
    );
    assert!(git(repo, &["branch", "--list", branch]).trim().is_empty());
}

struct RejectCase<'a> {
    job: &'a str,
    branch: &'a str,
    exit: i32,
    schema: u64,
    code: &'a str,
}

fn reject_without_mutation(
    root: &Path,
    repo: &Path,
    create_args: &[&str],
    case: RejectCase<'_>,
) -> serde_json::Value {
    let output = writ_cmd(root, create_args);
    let envelope = assert_error_envelope(&output, case.exit, case.schema, case.code);
    assert_uncreated(root, repo, case.job, case.branch);
    envelope
}

#[test]
fn legacy_v1_create_fails_with_machine_readable_upgrade_error_without_mutation() {
    let (root, repo) = primed();
    let envelope = reject_without_mutation(
        &root.0,
        &repo,
        &[
            "--repo",
            repo.to_str().unwrap(),
            "acme",
            "sample",
            "legacy",
            "feature/legacy",
        ],
        RejectCase {
            job: "legacy",
            branch: "feature/legacy",
            exit: 1,
            schema: 1,
            code: "CONTRACT_UPGRADE_REQUIRED",
        },
    );
    assert_eq!(envelope["data"]["required_schema_version"], 2);
}

#[test]
fn v2_create_without_start_point_fails_with_machine_readable_error_without_mutation() {
    let (root, repo) = primed();
    reject_without_mutation(
        &root.0,
        &repo,
        &[
            "--schema-version",
            "2",
            "--repo",
            repo.to_str().unwrap(),
            "acme",
            "sample",
            "missing",
            "feature/missing",
        ],
        RejectCase {
            job: "missing",
            branch: "feature/missing",
            exit: 1,
            schema: 2,
            code: "START_POINT_REQUIRED",
        },
    );
}

#[test]
fn invalid_start_point_emits_v2_error_envelope_with_exit_1() {
    let (root, repo) = primed();
    let output = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "invalid",
            branch: "feature/invalid",
            start: "does-not-exist",
        },
    );
    let envelope = assert_error_envelope(&output, 1, 2, "GIT_COMMAND_FAILED");
    assert_eq!(envelope["data"], serde_json::json!({}));
}

#[test]
fn existing_branch_policy_emits_v2_error_envelope_with_exit_2() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["branch", "feature/existing", &start]);

    let output = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "existing",
            branch: "feature/existing",
            start: &start,
        },
    );
    assert_error_envelope(&output, 2, 2, "WORKTREE_RESUME_UNPROVEN");
}

#[test]
fn v2_success_reports_verified_path_ref_commit_and_registration_identity() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let output = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "success",
            branch: "feature/success",
            start: &start,
        },
    );
    let envelope = json(&output);

    assert!(output.status.success());
    // Match WorktreeManager::with_base: join each identity segment, then
    // canonicalize so macOS /var vs /private/var and Windows 8.3 vs long-name
    // spellings compare equal to the verified success payload.
    let expected_path = writ_core::paths::canonicalize_for_tools(
        &root
            .0
            .join("worktrees")
            .join("acme")
            .join("sample")
            .join("success"),
    )
    .expect("created worktree path must exist and canonicalize");
    assert_eq!(
        serde_json::json!({
            "ok": true,
            "schema_version": 2,
            "path": expected_path,
            "branch": "feature/success",
            "branch_ref": "refs/heads/feature/success",
            "repo_root": repo,
            "start_commit": start,
            "head_commit": start,
            "worktree_registered": true,
        }),
        serde_json::json!({
            "ok": envelope["ok"],
            "schema_version": envelope["schema_version"],
            "path": envelope["data"]["path"],
            "branch": envelope["data"]["branch"],
            "branch_ref": envelope["data"]["branch_ref"],
            "repo_root": envelope["data"]["repo_root"],
            "start_commit": envelope["data"]["start_commit"],
            "head_commit": envelope["data"]["head_commit"],
            "worktree_registered": envelope["data"]["worktree_registered"],
        })
    );
}

#[test]
fn create_remove_reclaim_succeeds_for_same_job_and_start_commit() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let first = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "reclaim",
            branch: "hive/gh-42",
            start: &start,
        },
    );
    assert!(
        first.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&first.stderr)
    );
    let path = json(&first)["data"]["path"].as_str().unwrap().to_owned();

    let remove = Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.0.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.0.join("leases.db"))
        .args(["--json", "worktree", "remove", &path])
        .output()
        .unwrap();
    assert!(
        remove.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&remove.stderr)
    );
    assert_eq!(git(&repo, &["rev-parse", "refs/heads/hive/gh-42"]), start);

    let second = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "reclaim",
            branch: "hive/gh-42",
            start: &start,
        },
    );
    assert!(
        second.status.success(),
        "reclaim failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let envelope = json(&second);
    assert_eq!(envelope["data"]["start_commit"], start);
    assert_eq!(envelope["data"]["head_commit"], start);
}

#[test]
fn partial_create_failure_reports_residual_state_without_deleting_branch() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let target = root.0.join("worktrees/acme/sample/partial");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("occupied"), "keep\n").unwrap();

    let output = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "partial",
            branch: "feature/partial",
            start: &start,
        },
    );
    let envelope = assert_error_envelope(&output, 1, 2, "WORKTREE_CREATE_FAILED");

    assert_eq!(
        serde_json::json!({
            "branch_commit": start,
            "path_exists": true,
            "worktree_registered": false,
            "cleanup_performed": false,
        }),
        serde_json::json!({
            "branch_commit": envelope["data"]["branch_commit"],
            "path_exists": envelope["data"]["path_exists"],
            "worktree_registered": envelope["data"]["worktree_registered"],
            "cleanup_performed": envelope["data"]["cleanup_performed"],
        })
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/feature/partial"]),
        start
    );
    assert_eq!(
        fs::read_to_string(target.join("occupied")).unwrap(),
        "keep\n"
    );
}

#[test]
fn empty_allowlist_rejects_create_without_mutation() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let args = identity_args(
        &repo,
        &CreateRequest {
            job: "denied-empty",
            branch: "feature/denied-empty",
            start: &start,
        },
    );
    let output = writ_cmd_with_owners(&root.0, &args, None);
    assert_error_envelope(&output, 2, 2, "OWNER_NOT_ALLOWED");
    assert_uncreated(&root.0, &repo, "denied-empty", "feature/denied-empty");
}

#[test]
fn owner_outside_allowlist_rejects_create_without_mutation() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let args = identity_args(
        &repo,
        &CreateRequest {
            job: "denied-other",
            branch: "feature/denied-other",
            start: &start,
        },
    );
    let output = writ_cmd_with_owners(&root.0, &args, Some("other"));
    assert_error_envelope(&output, 2, 2, "OWNER_NOT_ALLOWED");
    assert_uncreated(&root.0, &repo, "denied-other", "feature/denied-other");
}

#[test]
fn explicit_allowed_owners_flag_overrides_env_and_creates() {
    let (root, repo) = primed();
    let start = git(&repo, &["rev-parse", "HEAD"]);
    let args = identity_args(
        &repo,
        &CreateRequest {
            job: "explicit",
            branch: "feature/explicit",
            start: &start,
        },
    );
    let output = Command::new(env!("CARGO_BIN_EXE_writ"))
        .env("WRIT_WORKTREE_BASE", root.0.join("worktrees"))
        .env("WRIT_LEASE_PATH", root.0.join("leases.db"))
        .env("WRIT_ALLOWED_OWNERS", "other")
        .args([
            "--json",
            "--allowed-owners",
            "github.com/Acme/Repo",
            "worktree",
            "create",
        ])
        .args(&args)
        .output()
        .unwrap();
    let envelope = json(&output);
    assert!(output.status.success());
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["branch"], "feature/explicit");
}

fn fork_layout() -> (TestDir, PathBuf, String, String) {
    let root = TestDir::new();
    let origin = root.0.join("origin.git");
    let init = Command::new("git")
        .current_dir(&root.0)
        .args(["init", "--bare", origin.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let repo = init_repo(&root.0);
    git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            "https://github.com/acme/sample.git",
        ],
    );
    git(&repo, &["push", "origin", "HEAD:refs/heads/trunk"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);

    let fork = root.0.join("fork");
    let clone = Command::new("git")
        .current_dir(&root.0)
        .args(["clone", origin.to_str().unwrap(), fork.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    git(&fork, &["config", "user.email", "test@example.com"]);
    git(&fork, &["config", "user.name", "Test User"]);
    git(&fork, &["commit", "--allow-empty", "-m", "fork"]);
    let fork_head = git(&fork, &["rev-parse", "HEAD"]);
    git(&fork, &["push", "origin", "HEAD:refs/pull/42/head"]);
    (root, repo, base, fork_head)
}

#[test]
fn v2_create_imports_fork_pr_head_absent_from_clone() {
    let (root, repo, base, fork_head) = fork_layout();
    assert_ne!(base, fork_head);
    let output = writ_cmd(
        &root.0,
        &[
            "--schema-version",
            "2",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            &fork_head,
            "--pr-number",
            "42",
            "--head-repo",
            "acme/fork",
            "acme",
            "sample",
            "fork-job",
            "feature/fork",
        ],
    );
    let envelope = json(&output);
    assert!(
        output.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope["data"]["start_commit"], fork_head);
    assert_eq!(envelope["data"]["head_commit"], fork_head);
}

#[test]
fn v2_create_mismatching_pr_head_reports_residual_without_mutation() {
    let (root, repo, base, _) = fork_layout();
    let output = writ_cmd(
        &root.0,
        &[
            "--schema-version",
            "2",
            "--repo",
            repo.to_str().unwrap(),
            "--start-point",
            &base,
            "--pr-number",
            "42",
            "acme",
            "sample",
            "mismatch",
            "feature/mismatch",
        ],
    );
    let envelope = assert_error_envelope(&output, 1, 2, "PR_IMPORT_FAILED");
    assert_eq!(envelope["data"]["cleanup_performed"], false);
    assert_eq!(envelope["data"]["import_ref_exists"], true);
    assert_uncreated(&root.0, &repo, "mismatch", "feature/mismatch");
}

fn collision_repo(root: &Path) -> (PathBuf, String, String) {
    let repo = init_repo(root);
    let branch_commit = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["branch", "collision", &branch_commit]);
    git(&repo, &["commit", "--allow-empty", "-m", "second"]);
    let tag_commit = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["tag", "collision", &tag_commit]);
    git(&repo, &["config", "core.warnAmbiguousRefs", "false"]);
    (repo, branch_commit, tag_commit)
}

#[test]
fn ambiguous_unqualified_start_point_emits_v2_error_without_mutation() {
    let root = TestDir::new();
    let (repo, branch_commit, tag_commit) = collision_repo(&root.0);
    let envelope = reject_without_mutation(
        &root.0,
        &repo,
        &identity_args(
            &repo,
            &CreateRequest {
                job: "ambiguous",
                branch: "feature/selected",
                start: "collision",
            },
        ),
        RejectCase {
            job: "ambiguous",
            branch: "feature/selected",
            exit: 1,
            schema: 2,
            code: "AMBIGUOUS_START_POINT",
        },
    );

    assert_eq!(envelope["data"]["start_point"], "collision");
    let refs = envelope["data"]["refs"].as_array().expect("refs array");
    let names: Vec<&str> = refs
        .iter()
        .filter_map(|value| value["refname"].as_str())
        .collect();
    assert!(names.contains(&"refs/heads/collision"), "{names:?}");
    assert!(names.contains(&"refs/tags/collision"), "{names:?}");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("collision"), "{message}");
    assert!(message.contains(&branch_commit), "{message}");
    assert!(message.contains(&tag_commit), "{message}");

    reject_without_mutation(
        &root.0,
        &repo,
        &identity_args(
            &repo,
            &CreateRequest {
                job: "decorated",
                branch: "feature/decorated",
                start: "collision~1",
            },
        ),
        RejectCase {
            job: "decorated",
            branch: "feature/decorated",
            exit: 1,
            schema: 2,
            code: "AMBIGUOUS_START_POINT",
        },
    );
}

#[test]
fn fully_qualified_collision_refs_and_full_object_id_succeed() {
    let root = TestDir::new();
    let (repo, branch_commit, tag_commit) = collision_repo(&root.0);

    let from_heads = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "from-heads",
            branch: "feature/from-heads",
            start: "refs/heads/collision",
        },
    );
    assert!(
        from_heads.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&from_heads.stderr)
    );
    assert_eq!(json(&from_heads)["data"]["start_commit"], branch_commit);

    let from_tag = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "from-tag",
            branch: "feature/from-tag",
            start: "refs/tags/collision",
        },
    );
    assert!(
        from_tag.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&from_tag.stderr)
    );
    assert_eq!(json(&from_tag)["data"]["start_commit"], tag_commit);

    let uppercase = branch_commit.to_ascii_uppercase();
    let from_oid = writ_create(
        &root.0,
        &repo,
        CreateRequest {
            job: "from-oid",
            branch: "feature/from-oid",
            start: &uppercase,
        },
    );
    assert!(
        from_oid.status.success(),
        "stderr={:?}",
        String::from_utf8_lossy(&from_oid.stderr)
    );
    assert_eq!(json(&from_oid)["data"]["start_commit"], branch_commit);
}
