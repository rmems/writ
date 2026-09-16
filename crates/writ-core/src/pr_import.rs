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
    run_allowlisted_git_restricted, run_allowlisted_git_restricted_with_file,
};
use crate::identity::{
    CommitId, HeadRepo, HexOidPrefix, RefName, RemoteName, StartPoint, hex_oid_matches_commit,
    peel_to_commit, require_bare_full_object_id,
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
    validate_source_remote(RemoteName(request.source_remote))?;
    if let Some(head_repo) = request.head_repo {
        validate_head_repo(HeadRepo(head_repo))?;
    }

    let source_ref = format!("refs/pull/{}/head", request.pr_number);
    let import_ref = allocate_import_ref(request.pr_number);
    let refs = ImportRefs {
        source: RefName(&source_ref),
        import: RefName(&import_ref),
    };
    reject_occupied_import_ref(request.repo_root, refs, &request)?;
    let allow_file_protocol = verify_origin_scope(&request)?;

    let args = pr_head_fetch_args(RemoteName(request.source_remote), refs.source, refs.import);
    let output =
        run_allowlisted_git_restricted_with_file(request.repo_root, &args, allow_file_protocol)?;
    if output.exit_code != 0 {
        return Err(import_failure(
            &request,
            refs,
            FailureDetail {
                reason: format!("fetch failed: {}", output.stderr.trim()),
                stderr: output.stderr,
            },
        ));
    }

    let imported = match peel_to_commit(request.repo_root, StartPoint(refs.import.as_str())) {
        Ok(commit) => commit,
        Err(error) => {
            return Err(import_failure(
                &request,
                refs,
                FailureDetail {
                    reason: format!("imported object is not a commit: {error}"),
                    stderr: error.to_string(),
                },
            ));
        }
    };

    if !hex_oid_matches_commit(HexOidPrefix(request.expected_oid), CommitId(&imported)) {
        return Err(import_failure(
            &request,
            refs,
            FailureDetail {
                reason: format!(
                    "imported commit {imported} does not equal expected head {}",
                    request.expected_oid
                ),
                stderr: String::new(),
            },
        ));
    }
    Ok(imported)
}

#[derive(Clone, Copy)]
struct ImportRefs<'a> {
    source: RefName<'a>,
    import: RefName<'a>,
}

struct FailureDetail {
    reason: String,
    stderr: String,
}

pub(crate) fn pr_head_fetch_args(
    remote: RemoteName<'_>,
    source_ref: RefName<'_>,
    dest_ref: RefName<'_>,
) -> Vec<String> {
    vec![
        "fetch".to_owned(),
        "--no-tags".to_owned(),
        "--no-recurse-submodules".to_owned(),
        "--no-write-fetch-head".to_owned(),
        "--".to_owned(),
        remote.as_str().to_owned(),
        format!("{}:{}", source_ref.as_str(), dest_ref.as_str()),
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

fn validate_source_remote(remote: RemoteName<'_>) -> Result<()> {
    if remote.as_str() != BASE_SOURCE_REMOTE {
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

fn is_configured_remote_name(name: RemoteName<'_>) -> bool {
    let text = name.as_str();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if text.len() > 64 {
        return false;
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return false;
    }
    if text.contains("..") {
        return false;
    }
    !text.ends_with('.')
}

fn validate_head_repo(head_repo: HeadRepo<'_>) -> Result<()> {
    if is_ext_transport_url(head_repo.as_str()) {
        return Err(unauthorized(
            "head_repo is not a fetch source; pass an owner/repo slug only",
        ));
    }
    if looks_like_url_or_path(head_repo) {
        return Err(unauthorized(
            "head_repo is not a fetch source; pass an owner/repo slug only",
        ));
    }
    if !is_owner_repo_slug(head_repo) {
        return Err(unauthorized(
            "head_repo must be an owner/repo slug, not a remote URL",
        ));
    }
    Ok(())
}

fn is_owner_repo_slug(head_repo: HeadRepo<'_>) -> bool {
    let mut parts = head_repo.as_str().split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(repo) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    if owner.is_empty() {
        return false;
    }
    !repo.is_empty()
}

/// Shared location tokens for head-repo rejection vs origin file-protocol.
/// Callers interpret the flags differently; do not collapse those policies.
#[derive(Clone, Copy)]
struct LocationText<'a>(&'a str);

impl<'a> LocationText<'a> {
    fn new(text: &'a str) -> Self {
        Self(text.trim())
    }

    fn as_str(self) -> &'a str {
        self.0
    }

    fn is_filesystem_path(self) -> bool {
        let text = self.0;
        text.starts_with('/') || text.starts_with('\\') || text.contains('\\')
    }

    fn has_url_scheme(self) -> bool {
        self.0.contains("://")
    }

    fn has_ssh_user(self) -> bool {
        self.0.contains('@')
    }
}

fn looks_like_url_or_path(head_repo: HeadRepo<'_>) -> bool {
    let loc = LocationText::new(head_repo.as_str());
    loc.has_url_scheme()
        || loc.has_ssh_user()
        || loc.as_str().starts_with('-')
        || loc.is_filesystem_path()
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
    refs: ImportRefs<'_>,
    request: &PrHeadImportRequest<'_>,
) -> Result<()> {
    if optional_rev_parse(repo_root, refs.import).is_some() {
        return Err(import_failure(
            request,
            refs,
            FailureDetail {
                reason: "import ref namespace is already occupied".to_owned(),
                stderr: String::new(),
            },
        ));
    }
    Ok(())
}

fn verify_origin_scope(request: &PrHeadImportRequest<'_>) -> Result<bool> {
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
    origin_url_is_in_scope(request, output.stdout.trim())
}

fn origin_url_is_in_scope(request: &PrHeadImportRequest<'_>, url: &str) -> Result<bool> {
    if is_ext_transport_url(url) {
        return Err(unauthorized(
            "base remote uses the forbidden ext:: transport",
        ));
    }
    if looks_like_local_remote(url) {
        return Ok(true);
    }
    let Some((_, slug)) = normalize_github_repo_identity(url) else {
        return Ok(false);
    };
    let expected = format!("{}/{}", request.owner, request.repo);
    if github_repo_slugs_match(&slug, &expected) {
        return Ok(false);
    }
    Err(unauthorized(&format!(
        "base remote `{url}` is not {expected}"
    )))
}

fn looks_like_local_remote(url: &str) -> bool {
    let loc = LocationText::new(url);
    if loc.is_filesystem_path() {
        return true;
    }
    let text = loc.as_str();
    if text.starts_with('.') || text.starts_with("file://") {
        return true;
    }
    // scp-like `[user@]host:path` is a remote even without `user@`
    // (`github.com:evil/repo.git`). Git resolves it over SSH, so it must flow
    // through identity normalization and unauthorized-source rejection instead
    // of being trusted as a local path (RM-824).
    if is_scp_like_remote(text) {
        return false;
    }
    !loc.has_url_scheme() && !loc.has_ssh_user()
}

/// Whether `text` uses git's scp-like `[user@]host:path` syntax, which git
/// resolves as an SSH remote rather than a local path. Mirrors git's own rule:
/// no URL scheme, and a `:` before the first `/`. Windows drive prefixes
/// (`C:/...`) are local paths, and the `ext::` transport is rejected
/// separately before this classifier runs.
fn is_scp_like_remote(text: &str) -> bool {
    if text.contains("://") || text.starts_with("ext::") {
        return false;
    }
    let Some(colon) = text.find(':') else {
        return false;
    };
    let (host, path) = text.split_at(colon);
    let path = &path[1..];
    if host.is_empty() || path.is_empty() {
        return false;
    }
    if host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic() {
        return false;
    }
    !host.contains('/') && !host.contains('\\')
}

fn optional_rev_parse(repo_root: &Path, rev: RefName<'_>) -> Option<String> {
    optional_rev_parse_spec(repo_root, format!("{}^{{commit}}", rev.as_str()))
        .or_else(|| optional_rev_parse_spec(repo_root, rev.as_str().to_owned()))
}

fn optional_rev_parse_spec(repo_root: &Path, spec: String) -> Option<String> {
    let args = vec![
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        "--end-of-options".to_owned(),
        spec,
    ];
    let output = run_allowlisted_git_restricted(repo_root, &args).ok()?;
    if output.exit_code != 0 {
        return None;
    }
    nonempty_oid(&output.stdout)
}

fn nonempty_oid(stdout: &str) -> Option<String> {
    let oid = stdout.trim();
    if oid.is_empty() {
        None
    } else {
        Some(oid.to_owned())
    }
}

fn import_failure(
    request: &PrHeadImportRequest<'_>,
    refs: ImportRefs<'_>,
    detail: FailureDetail,
) -> Error {
    let imported = optional_rev_parse(request.repo_root, refs.import);
    Error::PrImportFailed(Box::new(PrImportFailure {
        expected_commit: request.expected_oid.to_owned(),
        source_remote: request.source_remote.to_owned(),
        source_ref: refs.source.as_str().to_owned(),
        import_ref: refs.import.as_str().to_owned(),
        imported_commit: imported.clone(),
        import_ref_exists: imported.is_some(),
        cleanup_performed: false,
        reason: detail.reason,
        stderr: detail.stderr,
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
    use crate::identity::{RefName, RemoteName};
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
            RemoteName("origin"),
            RefName("refs/pull/42/head"),
            RefName("refs/writ/import/1/pr-42/head"),
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
    fn scp_like_remote_without_user_is_rejected_as_unauthorized() {
        // RM-824: `github.com:evil/widgets.git` (no `user@`) was misclassified
        // as a local path, bypassing unauthorized-source enforcement, while
        // the same identity with `git@` was correctly rejected. Both must now
        // fail closed through the same rejection path.
        let harness = OriginHarness::new();
        for origin_url in [
            "github.com:evil/widgets.git",
            "git@github.com:evil/widgets.git",
        ] {
            git(&harness.clone, &["remote", "set-url", "origin", origin_url]);
            let err =
                import_and_verify_pr_head(harness.request(1, &harness.missing_head)).unwrap_err();
            assert!(
                matches!(
                    err,
                    Error::PolicyViolation {
                        code: PolicyCode::UnauthorizedSource,
                        ..
                    }
                ),
                "origin `{origin_url}` was not rejected: {err:?}"
            );
        }
    }

    #[test]
    fn scp_like_expected_repo_and_local_remotes_keep_working() {
        // The expected repo in scp-like form (with or without `user@`) stays
        // allowed, and legitimate local remotes still enable file protocol.
        let dir = tempdir().unwrap();
        let oid = "0".repeat(40);
        let request = PrHeadImportRequest {
            repo_root: dir.path(),
            owner: "acme",
            repo: "widgets",
            pr_number: 1,
            expected_oid: &oid,
            source_remote: "origin",
            head_repo: None,
        };
        assert!(!origin_url_is_in_scope(&request, "github.com:acme/widgets.git").unwrap());
        assert!(!origin_url_is_in_scope(&request, "git@github.com:acme/widgets.git").unwrap());
        assert!(origin_url_is_in_scope(&request, "/srv/repos/widgets.git").unwrap());
        assert!(origin_url_is_in_scope(&request, "./widgets.git").unwrap());
        assert!(origin_url_is_in_scope(&request, "file:///srv/repos/widgets.git").unwrap());
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

    #[test]
    fn location_tokens_keep_head_repo_and_origin_policies_distinct() {
        let cases = [
            ("/abs/repo.git", true, true),
            (r"C:\src\repo.git", true, true),
            ("file:///tmp/origin.git", true, true),
            ("./rel.git", false, true),
            ("https://github.com/acme/widgets.git", true, false),
            ("git@github.com:acme/widgets.git", true, false),
            // RM-824: scp-like without `user@` is a remote, not a local path.
            ("github.com:evil/repo.git", false, false),
            ("github.com:acme/widgets.git", false, false),
            // Windows drive prefix with forward slashes stays a local path.
            ("C:/src/repo.git", false, true),
            ("ext::sh -c evil", false, true),
            ("-upload-pack", true, true),
            ("acme/fork", false, true),
        ];
        for (input, url_or_path, local_remote) in cases {
            assert_eq!(
                looks_like_url_or_path(HeadRepo(input)),
                url_or_path,
                "url_or_path {input}"
            );
            assert_eq!(
                looks_like_local_remote(input),
                local_remote,
                "local {input}"
            );
        }
    }
}
