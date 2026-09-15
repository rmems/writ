//! Exact-object import of untrusted fork PR heads.
//!
//! Fetch argv, ref namespace selection, and object verification stay inside the
//! Rust boundary. The only allowed source is the base clone's `origin` remote
//! and its `refs/pull/<n>/head` ref. Mutable fork branch names are never used as
//! checkout authority.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, PolicyCode, PrImportFailure, Result};
use crate::git_safe::{
    github_repo_slugs_match, is_ext_transport_url, normalize_github_repo_identity,
    run_allowlisted_git_restricted,
};
use crate::identity::{
    CommitId, HexOidPrefix, StartPoint, hex_oid_matches_commit, peel_to_commit,
    require_bare_full_object_id,
};

/// Named remote bound to the base repository. Callers cannot substitute a fork URL.
const BASE_SOURCE_REMOTE: &str = "origin";

static IMPORT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Identity required to import a fork PR head that may be absent locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrHeadImportRequest<'a> {
    pub repo_root: &'a Path,
    pub owner: &'a str,
    pub repo: &'a str,
    pub pr_number: u64,
    pub expected_oid: &'a str,
    pub source_remote: &'a str,
    pub head_repo: Option<&'a str>,
}

/// Fetch a PR head into an isolated ref namespace and verify the exact object id.
pub(crate) fn import_and_verify_pr_head(request: PrHeadImportRequest<'_>) -> Result<String> {
    let expected = StartPoint(request.expected_oid);
    require_bare_full_object_id(expected)?;
    validate_pr_number(request.pr_number)?;
    validate_source_remote(request.source_remote)?;
    if let Some(head_repo) = request.head_repo {
        validate_head_repo(head_repo)?;
    }

    let source_ref = format!("refs/pull/{}/head", request.pr_number);
    let import_ref = allocate_import_ref(request.pr_number);
    reject_occupied_import_ref(request.repo_root, &import_ref, &request, &source_ref)?;
    verify_origin_scope(&request)?;

    let args = pr_head_fetch_args(request.source_remote, &source_ref, &import_ref);
    let output = run_allowlisted_git_restricted(request.repo_root, &args)?;
    if output.exit_code != 0 {
        return Err(import_failure(
            &request,
            &source_ref,
            &import_ref,
            format!("fetch failed: {}", output.stderr.trim()),
            output.stderr,
        ));
    }

    let imported = match peel_to_commit(request.repo_root, StartPoint(&import_ref)) {
        Ok(commit) => commit,
        Err(error) => {
            return Err(import_failure(
                &request,
                &source_ref,
                &import_ref,
                format!("imported object is not a commit: {error}"),
                error.to_string(),
            ));
        }
    };

    if !hex_oid_matches_commit(HexOidPrefix(request.expected_oid), CommitId(&imported)) {
        return Err(import_failure(
            &request,
            &source_ref,
            &import_ref,
            format!(
                "imported commit {imported} does not equal expected head {}",
                request.expected_oid
            ),
            String::new(),
        ));
    }
    Ok(imported)
}

pub(crate) fn pr_head_fetch_args(remote: &str, source_ref: &str, dest_ref: &str) -> Vec<String> {
    vec![
        "fetch".to_owned(),
        "--no-tags".to_owned(),
        "--no-recurse-submodules".to_owned(),
        "--no-write-fetch-head".to_owned(),
        "--".to_owned(),
        remote.to_owned(),
        format!("{source_ref}:{dest_ref}"),
    ]
}

fn validate_pr_number(pr_number: u64) -> Result<()> {
    if pr_number == 0 {
        return Err(unauthorized(
            "pull request number must be a positive integer bound to refs/pull/<n>/head",
        ));
    }
    Ok(())
}

fn validate_source_remote(remote: &str) -> Result<()> {
    if remote != BASE_SOURCE_REMOTE {
        return Err(unauthorized(
            "fork PR import may only fetch from the base repository remote `origin`",
        ));
    }
    if !is_configured_remote_name(remote) {
        return Err(unauthorized(
            "source remote must be a configured remote name, not a URL",
        ));
    }
    Ok(())
}

fn is_configured_remote_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.contains("..")
        && !name.ends_with('.')
}

fn validate_head_repo(head_repo: &str) -> Result<()> {
    if is_ext_transport_url(head_repo) || looks_like_url_or_path(head_repo) {
        return Err(unauthorized(
            "head_repo is not a fetch source; pass an owner/repo slug only",
        ));
    }
    let mut parts = head_repo.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("");
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(unauthorized(
            "head_repo must be an owner/repo slug, not a remote URL",
        ));
    }
    Ok(())
}

fn looks_like_url_or_path(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.contains("://")
        || trimmed.contains('@')
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.starts_with('-')
        || trimmed.contains('\\')
}

fn allocate_import_ref(pr_number: u64) -> String {
    let seq = IMPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "refs/writ/import/{pid}-{seq}-{nanos}/pr-{pr_number}/head",
        pid = std::process::id(),
    )
}

fn reject_occupied_import_ref(
    repo_root: &Path,
    import_ref: &str,
    request: &PrHeadImportRequest<'_>,
    source_ref: &str,
) -> Result<()> {
    if optional_rev_parse(repo_root, import_ref).is_some() {
        return Err(import_failure(
            request,
            source_ref,
            import_ref,
            "import ref namespace is already occupied".to_owned(),
            String::new(),
        ));
    }
    Ok(())
}

fn verify_origin_scope(request: &PrHeadImportRequest<'_>) -> Result<()> {
    let args = vec![
        "remote".to_owned(),
        "get-url".to_owned(),
        request.source_remote.to_owned(),
    ];
    let output = run_allowlisted_git_restricted(request.repo_root, &args)?;
    if output.exit_code != 0 {
        return Err(unauthorized(&format!(
            "base remote `{}` is not configured: {}",
            request.source_remote,
            output.stderr.trim()
        )));
    }
    let url = output.stdout.trim();
    if is_ext_transport_url(url) {
        return Err(unauthorized(
            "base remote uses the forbidden ext:: transport",
        ));
    }
    if looks_like_local_remote(url) {
        return Ok(());
    }
    let Some((_, slug)) = normalize_github_repo_identity(url) else {
        return Ok(());
    };
    let expected = format!("{}/{}", request.owner, request.repo);
    if github_repo_slugs_match(&slug, &expected) {
        return Ok(());
    }
    Err(unauthorized(&format!(
        "base remote `{url}` is not {expected}"
    )))
}

fn looks_like_local_remote(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.starts_with('/')
        || trimmed.starts_with('.')
        || trimmed.starts_with("file://")
        || trimmed.contains('\\')
        || (!trimmed.contains("://") && !trimmed.contains('@'))
}

fn optional_rev_parse(repo_root: &Path, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    let args = vec![
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        "--end-of-options".to_owned(),
        spec,
    ];
    let output = run_allowlisted_git_restricted(repo_root, &args).ok()?;
    if output.exit_code != 0 {
        let object_args = vec![
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            "--end-of-options".to_owned(),
            rev.to_owned(),
        ];
        let object = run_allowlisted_git_restricted(repo_root, &object_args).ok()?;
        if object.exit_code != 0 {
            return None;
        }
        let oid = object.stdout.trim();
        if oid.is_empty() {
            return None;
        }
        return Some(oid.to_owned());
    }
    let oid = output.stdout.trim();
    if oid.is_empty() {
        None
    } else {
        Some(oid.to_owned())
    }
}

fn import_failure(
    request: &PrHeadImportRequest<'_>,
    source_ref: &str,
    import_ref: &str,
    reason: String,
    stderr: String,
) -> Error {
    let imported = optional_rev_parse(request.repo_root, import_ref);
    Error::PrImportFailed(Box::new(PrImportFailure {
        expected_commit: request.expected_oid.to_owned(),
        source_remote: request.source_remote.to_owned(),
        source_ref: source_ref.to_owned(),
        import_ref: import_ref.to_owned(),
        imported_commit: imported.clone(),
        import_ref_exists: imported.is_some(),
        cleanup_performed: false,
        reason,
        stderr,
    }))
}

fn unauthorized(message: &str) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::UnauthorizedSource,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_safe::SafeGitCommand;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use tempfile::tempdir;

    struct OriginHarness {
        _temp: tempfile::TempDir,
        origin: PathBuf,
        clone: PathBuf,
        missing_head: String,
    }

    impl OriginHarness {
        fn new() -> Self {
            let temp = tempdir().unwrap();
            let origin = temp.path().join("origin.git");
            let clone = temp.path().join("clone");
            git_cwd(temp.path(), &["init", "--bare", origin.to_str().unwrap()]);

            let seed = temp.path().join("seed");
            fs::create_dir(&seed).unwrap();
            git(&seed, &["init", "-b", "main"]);
            git(&seed, &["config", "user.email", "test@example.com"]);
            git(&seed, &["config", "user.name", "Test User"]);
            git(&seed, &["commit", "--allow-empty", "-m", "base"]);
            git(
                &seed,
                &["remote", "add", "origin", origin.to_str().unwrap()],
            );
            git(&seed, &["push", "origin", "HEAD:refs/heads/main"]);
            let missing_head = git(&seed, &["rev-parse", "HEAD"]);

            git_cwd(
                temp.path(),
                &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
            );
            git(&clone, &["config", "user.email", "test@example.com"]);
            git(&clone, &["config", "user.name", "Test User"]);

            Self {
                _temp: temp,
                origin,
                clone,
                missing_head,
            }
        }

        fn publish_pr_commit(&self, pr_number: u64, message: &str) -> String {
            let work = self.clone.parent().unwrap().join(format!("pr-{pr_number}"));
            git_cwd(
                self.clone.parent().unwrap(),
                &[
                    "clone",
                    self.origin.to_str().unwrap(),
                    work.to_str().unwrap(),
                ],
            );
            git(&work, &["config", "user.email", "test@example.com"]);
            git(&work, &["config", "user.name", "Test User"]);
            git(&work, &["commit", "--allow-empty", "-m", message]);
            let commit = git(&work, &["rev-parse", "HEAD"]);
            git(
                &work,
                &[
                    "push",
                    "origin",
                    &format!("HEAD:refs/pull/{pr_number}/head"),
                ],
            );
            commit
        }

        fn publish_blob_pr(&self, pr_number: u64) -> String {
            let blob = git_with_stdin(
                &self.origin,
                &["hash-object", "-w", "--stdin"],
                Some("not a commit\n"),
            );
            git(
                &self.origin,
                &["update-ref", &format!("refs/pull/{pr_number}/head"), &blob],
            );
            blob
        }

        fn request<'a>(&'a self, pr_number: u64, expected_oid: &'a str) -> PrHeadImportRequest<'a> {
            PrHeadImportRequest {
                repo_root: &self.clone,
                owner: "acme",
                repo: "widgets",
                pr_number,
                expected_oid,
                source_remote: "origin",
                head_repo: None,
            }
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        git_with_stdin(repo, args, None)
    }

    fn git_with_stdin(repo: &Path, args: &[&str], stdin: Option<&str>) -> String {
        let mut command = Command::new("git");
        command.arg("-C").arg(repo).args(args);
        if stdin.is_some() {
            command.stdin(std::process::Stdio::piped());
        }
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = stdin {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn git_cwd(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
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

    #[test]
    fn fetch_argv_is_allowlisted_and_rejects_helpers() {
        let args = pr_head_fetch_args(
            "origin",
            "refs/pull/42/head",
            "refs/writ/import/1/pr-42/head",
        );
        SafeGitCommand::new(&args).unwrap();

        let mut upload = args.clone();
        upload.insert(1, "--upload-pack=sh".to_owned());
        assert!(matches!(
            SafeGitCommand::new(&upload),
            Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            })
        ));

        let mut ext = args.clone();
        ext[5] = "ext::gh pr merge 1".to_owned();
        assert!(matches!(
            SafeGitCommand::new(&ext),
            Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            })
        ));

        let mut force = args.clone();
        force.insert(1, "--force".to_owned());
        assert!(matches!(
            SafeGitCommand::new(&force),
            Err(Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            })
        ));
    }

    #[test]
    fn imports_absent_fork_head_at_exact_commit() {
        let harness = OriginHarness::new();
        let head = harness.publish_pr_commit(42, "fork head");
        assert_ne!(head, harness.missing_head);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&harness.clone)
                .args(["cat-file", "-e", &head])
                .status()
                .unwrap()
                .code()
                != Some(0)
        );

        let imported = import_and_verify_pr_head(harness.request(42, &head)).unwrap();
        assert_eq!(imported, head);
        assert_eq!(
            git(
                &harness.clone,
                &["rev-parse", "--verify", &format!("{head}^{{commit}}")]
            ),
            head
        );
    }

    #[test]
    fn mismatching_source_object_fails_before_returning_id() {
        let harness = OriginHarness::new();
        let actual = harness.publish_pr_commit(7, "other head");
        let err = import_and_verify_pr_head(harness.request(7, &harness.missing_head)).unwrap_err();
        match err {
            Error::PrImportFailed(failure) => {
                assert_eq!(failure.expected_commit, harness.missing_head);
                assert_eq!(failure.imported_commit.as_deref(), Some(actual.as_str()));
                assert!(failure.import_ref_exists);
                assert!(!failure.cleanup_performed);
                assert_eq!(
                    git(&harness.clone, &["rev-parse", &failure.import_ref]),
                    actual,
                    "mismatch must leave the residual import ref in place"
                );
            }
            other => panic!("expected PrImportFailed, got {other:?}"),
        }
    }

    #[test]
    fn missing_pr_ref_fails_closed() {
        let harness = OriginHarness::new();
        let err =
            import_and_verify_pr_head(harness.request(99, &harness.missing_head)).unwrap_err();
        match err {
            Error::PrImportFailed(failure) => {
                assert!(!failure.import_ref_exists);
                assert!(!failure.cleanup_performed);
                assert!(failure.reason.contains("fetch failed"));
            }
            other => panic!("expected PrImportFailed, got {other:?}"),
        }
        assert!(
            git(&harness.clone, &["branch", "--list", "feature/missing"])
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn non_commit_pr_ref_fails_closed() {
        let harness = OriginHarness::new();
        let blob = harness.publish_blob_pr(8);
        let err = import_and_verify_pr_head(harness.request(8, &harness.missing_head)).unwrap_err();
        match err {
            Error::PrImportFailed(failure) => {
                assert!(failure.reason.contains("not a commit"));
                assert!(!failure.cleanup_performed);
                assert_eq!(failure.imported_commit.as_deref(), Some(blob.as_str()));
            }
            other => panic!("expected PrImportFailed, got {other:?}"),
        }
    }

    #[test]
    fn abbreviated_and_symbolic_expected_ids_fail_before_fetch() {
        let harness = OriginHarness::new();
        let head = harness.publish_pr_commit(3, "abbrev");
        let abbreviated = &head[..12];
        assert!(import_and_verify_pr_head(harness.request(3, abbreviated)).is_err());
        assert!(import_and_verify_pr_head(harness.request(3, "refs/heads/main")).is_err());
        let decorated = format!("{head}~0");
        assert!(import_and_verify_pr_head(harness.request(3, &decorated)).is_err());
        let listing = git(&harness.clone, &["for-each-ref", "refs/writ/import"]);
        assert!(
            listing.is_empty(),
            "invalid expected ids must not fetch: {listing}"
        );
    }

    #[test]
    fn unauthorized_sources_fail_closed() {
        let harness = OriginHarness::new();
        let head = harness.publish_pr_commit(4, "auth");
        let mut url = harness.request(4, &head);
        url.source_remote = "https://example.com/acme/widgets.git";
        assert!(matches!(
            import_and_verify_pr_head(url),
            Err(Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            })
        ));

        let mut fork_remote = harness.request(4, &head);
        fork_remote.source_remote = "fork";
        assert!(matches!(
            import_and_verify_pr_head(fork_remote),
            Err(Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            })
        ));

        let mut helper = harness.request(4, &head);
        helper.source_remote = "--upload-pack=sh";
        assert!(matches!(
            import_and_verify_pr_head(helper),
            Err(Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            })
        ));

        let mut head_url = harness.request(4, &head);
        head_url.head_repo = Some("https://evil.example/acme/fork.git");
        assert!(matches!(
            import_and_verify_pr_head(head_url),
            Err(Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            })
        ));

        let mut ext = harness.request(4, &head);
        ext.head_repo = Some("ext::gh pr merge 1");
        assert!(matches!(
            import_and_verify_pr_head(ext),
            Err(Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            })
        ));
    }

    #[test]
    fn github_origin_owner_mismatch_is_unauthorized() {
        let harness = OriginHarness::new();
        git(
            &harness.clone,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/evil/malware.git",
            ],
        );
        let err = import_and_verify_pr_head(harness.request(1, &harness.missing_head)).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::UnauthorizedSource,
                ..
            }
        ));
    }

    #[test]
    fn concurrent_imports_use_isolated_namespaces() {
        let harness = OriginHarness::new();
        let first = harness.publish_pr_commit(11, "one");
        let second = harness.publish_pr_commit(12, "two");
        assert_ne!(first, second);

        let repo = harness.clone.clone();
        let results = thread::scope(|scope| {
            let one = scope.spawn(|| {
                import_and_verify_pr_head(PrHeadImportRequest {
                    repo_root: &repo,
                    owner: "acme",
                    repo: "widgets",
                    pr_number: 11,
                    expected_oid: &first,
                    source_remote: "origin",
                    head_repo: Some("acme/fork"),
                })
            });
            let two = scope.spawn(|| {
                import_and_verify_pr_head(PrHeadImportRequest {
                    repo_root: &repo,
                    owner: "acme",
                    repo: "widgets",
                    pr_number: 12,
                    expected_oid: &second,
                    source_remote: "origin",
                    head_repo: Some("acme/fork"),
                })
            });
            (one.join().unwrap(), two.join().unwrap())
        });
        assert_eq!(results.0.unwrap(), first);
        assert_eq!(results.1.unwrap(), second);

        let listing = git(
            &harness.clone,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/writ/import",
            ],
        );
        assert!(listing.contains(&first), "{listing}");
        assert!(listing.contains(&second), "{listing}");
        let refs: Vec<_> = listing.lines().collect();
        assert_eq!(refs.len(), 2);
        assert_ne!(
            refs[0].split_whitespace().next(),
            refs[1].split_whitespace().next()
        );
    }

    #[test]
    fn imported_refs_stay_in_writ_import_namespace() {
        let harness = OriginHarness::new();
        let head = harness.publish_pr_commit(21, "ns");
        import_and_verify_pr_head(harness.request(21, &head)).unwrap();
        let listing = git(
            &harness.clone,
            &["for-each-ref", "--format=%(refname)", "refs/writ/import"],
        );
        assert!(
            listing.lines().all(|r| r.starts_with("refs/writ/import/")),
            "{listing}"
        );
        assert!(
            git(
                &harness.clone,
                &["for-each-ref", "--format=%(refname)", "refs/namespaces"]
            )
            .is_empty()
        );
    }
}
