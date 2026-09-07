//! Borrowed identities for isolated worktree creation.
//!
//! These newtypes keep owner, repo, job, branch, start-point, and commit values
//! distinct at the Rust boundary. JSON and CLI contracts still exchange plain
//! strings.

use std::path::Path;

use crate::error::{Error, Result};

macro_rules! borrowed_identity {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name<'a>(pub &'a str);

        impl<'a> $name<'a> {
            #[must_use]
            pub const fn as_str(self) -> &'a str {
                self.0
            }
        }
    };
}

borrowed_identity!(Owner);
borrowed_identity!(Repo);
borrowed_identity!(JobId);
borrowed_identity!(BranchName);
borrowed_identity!(StartPoint);
borrowed_identity!(CommitId);
borrowed_identity!(BranchRef);
borrowed_identity!(HexOidPrefix);

/// Resolve a caller-supplied commit-ish to one exact commit object.
pub(crate) fn resolve_start_commit(
    repo_root: &Path,
    start_point: StartPoint<'_>,
) -> Result<String> {
    reject_empty_start_point(start_point)?;
    enforce_leading_hex_oid(start_point, None)?;
    let commit = peel_to_commit(repo_root, start_point)?;
    reject_empty_resolved_commit(start_point, CommitId(&commit))?;
    enforce_leading_hex_oid(start_point, Some(CommitId(&commit)))?;
    Ok(commit)
}

fn reject_empty_start_point(start_point: StartPoint<'_>) -> Result<()> {
    if start_point.as_str().is_empty() {
        return Err(rev_parse_error(GitErrorText(
            "start point must not be empty".to_owned(),
        )));
    }
    Ok(())
}

fn reject_empty_resolved_commit(start_point: StartPoint<'_>, commit: CommitId<'_>) -> Result<()> {
    if commit.as_str().is_empty() {
        return Err(rev_parse_error(GitErrorText(format!(
            "start point {:?} resolved to an empty commit id",
            start_point.as_str()
        ))));
    }
    Ok(())
}

fn enforce_leading_hex_oid(
    start_point: StartPoint<'_>,
    resolved: Option<CommitId<'_>>,
) -> Result<()> {
    let Some(hex_prefix) = leading_hex_oid_prefix(start_point) else {
        return Ok(());
    };
    reject_non_full_hex_oid(hex_prefix)?;
    match resolved {
        Some(commit) => reject_hex_oid_mismatch(start_point, hex_prefix, commit),
        None => Ok(()),
    }
}

/// Leading all-hex object-id text when it is the entire start point or is
/// immediately followed by a commit-ish decoration (`~`, `^`, `@{`).
///
/// This closes abbreviated-OID smuggling such as `<abbrev>~0` while leaving
/// symbolic refs (`refs/heads/x`, `develop~1`, tags) untouched.
fn leading_hex_oid_prefix(start_point: StartPoint<'_>) -> Option<HexOidPrefix<'_>> {
    let text = start_point.as_str();
    let hex_len = text.bytes().take_while(u8::is_ascii_hexdigit).count();
    if hex_len == 0 {
        return None;
    }
    if !hex_oid_rest_is_boundary(SelectorSuffix(&text[hex_len..])) {
        return None;
    }
    Some(HexOidPrefix(&text[..hex_len]))
}

#[derive(Clone, Copy)]
struct SelectorSuffix<'a>(&'a str);

/// True when a leading hex run is a complete selector: bare, or immediately
/// followed by a commit-ish decoration. Guard clauses keep the match set
/// explicit without a compound boolean.
fn hex_oid_rest_is_boundary(suffix: SelectorSuffix<'_>) -> bool {
    let rest = suffix.0;
    if rest.is_empty() {
        return true;
    }
    if rest.starts_with('~') {
        return true;
    }
    if rest.starts_with('^') {
        return true;
    }
    rest.starts_with("@{")
}

fn reject_non_full_hex_oid(hex_prefix: HexOidPrefix<'_>) -> Result<()> {
    let len = hex_prefix.as_str().len();
    if len == 40 {
        return Ok(());
    }
    if len == 64 {
        return Ok(());
    }
    Err(rev_parse_error(GitErrorText(
        "all-hex start point must be a full 40- or 64-character object id".to_owned(),
    )))
}

fn reject_hex_oid_mismatch(
    start_point: StartPoint<'_>,
    hex_prefix: HexOidPrefix<'_>,
    commit: CommitId<'_>,
) -> Result<()> {
    if hex_oid_matches_commit(hex_prefix, commit) {
        return Ok(());
    }
    Err(rev_parse_error(GitErrorText(format!(
        "all-hex start point must equal the full canonical object id; requested \
         {:?}, resolved {:?}",
        start_point.as_str(),
        commit.as_str()
    ))))
}

fn hex_oid_matches_commit(hex_prefix: HexOidPrefix<'_>, commit: CommitId<'_>) -> bool {
    let prefix = hex_prefix.as_str();
    let commit = commit.as_str();
    if prefix.len() != commit.len() {
        return false;
    }
    commit.eq_ignore_ascii_case(prefix)
}

fn peel_to_commit(repo_root: &Path, start_point: StartPoint<'_>) -> Result<String> {
    let commitish = format!("{}^{{commit}}", start_point.as_str());
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("--verify")
        .arg("--end-of-options")
        .arg(&commitish)
        .output()
        .map_err(|e| Error::Io {
            context: "resolve worktree start point",
            source: e,
        })?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(Error::GitCommand {
        args: vec![
            "rev-parse".into(),
            "--verify".into(),
            "--end-of-options".into(),
            commitish,
        ],
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn rev_parse_error(stderr: GitErrorText) -> Error {
    Error::GitCommand {
        args: vec!["rev-parse".into(), "--verify".into()],
        stderr: stderr.0,
    }
}

struct GitErrorText(String);

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1: &str = "e5389f2c530e6d6a298b9bdd7b3b44616154104e";
    const SHA256: &str = "898ba747a8267a634ad8c578eee74e4c93772150898ba747a8267a634ad8c578";

    fn prefix_of(text: &str) -> Option<&str> {
        leading_hex_oid_prefix(StartPoint(text)).map(HexOidPrefix::as_str)
    }

    // --- leading_hex_oid_prefix: what counts as an object-id selector ---

    #[test]
    fn bare_hex_run_is_a_selector() {
        assert_eq!(prefix_of(SHA1), Some(SHA1));
        assert_eq!(prefix_of("deadbeef"), Some("deadbeef"));
    }

    #[test]
    fn commit_ish_decorations_still_expose_the_hex_prefix() {
        // This is the abbreviated-OID smuggling case: the decoration must not
        // hide the leading hex from the full-length check.
        for suffix in ["~0", "~1", "^0", "^{commit}", "@{0}", "@{upstream}"] {
            let text = format!("deadbeef{suffix}");
            assert_eq!(
                prefix_of(&text),
                Some("deadbeef"),
                "decoration {suffix:?} should not hide the hex prefix"
            );
        }
    }

    #[test]
    fn symbolic_refs_are_left_alone() {
        // `develop` starts with two hex characters (d, e) but does not continue
        // into a decoration, so it is not an object-id selector.
        for text in [
            "develop~1",
            "refs/heads/main",
            "refs/tags/v1",
            "main",
            "feature/abc123",
            "origin/main",
        ] {
            assert_eq!(
                prefix_of(text),
                None,
                "{text:?} should not look like an OID"
            );
        }
    }

    #[test]
    fn at_without_a_brace_is_not_a_decoration() {
        // Only `@{` opens a reflog selector. A bare `@` does not, so the value
        // is a symbolic name and passes through to git untouched.
        assert_eq!(prefix_of("deadbeef@x"), None);
        assert_eq!(prefix_of("deadbeef@"), None);
        assert_eq!(prefix_of("deadbeef-tag"), None);
    }

    #[test]
    fn empty_input_has_no_prefix() {
        assert_eq!(prefix_of(""), None);
    }

    // --- reject_non_full_hex_oid: only full-width object ids are accepted ---

    #[test]
    fn full_width_object_ids_are_accepted() {
        assert!(reject_non_full_hex_oid(HexOidPrefix(SHA1)).is_ok());
        assert!(reject_non_full_hex_oid(HexOidPrefix(SHA256)).is_ok());
    }

    #[test]
    fn off_by_one_widths_are_rejected() {
        // The boundary either side of both accepted widths.
        for len in [39, 41, 63, 65] {
            let text = "a".repeat(len);
            assert!(
                reject_non_full_hex_oid(HexOidPrefix(&text)).is_err(),
                "{len}-character hex must be rejected"
            );
        }
    }

    #[test]
    fn a_short_all_hex_branch_name_is_rejected_as_an_object_id() {
        // Documents a deliberate fail-closed trade: a branch named `cafe` is
        // all-hex, so it is treated as a malformed object id rather than a ref.
        // Fully qualifying it (`refs/heads/cafe`) is the supported escape.
        assert_eq!(prefix_of("cafe"), Some("cafe"));
        assert!(reject_non_full_hex_oid(HexOidPrefix("cafe")).is_err());
        assert_eq!(prefix_of("refs/heads/cafe"), None);
    }

    // --- hex_oid_matches_commit: exact, case-insensitive, width-sensitive ---

    #[test]
    fn object_id_comparison_ignores_case() {
        let upper = SHA1.to_ascii_uppercase();
        assert!(hex_oid_matches_commit(HexOidPrefix(&upper), CommitId(SHA1)));
    }

    #[test]
    fn a_sha1_width_prefix_never_matches_a_sha256_commit() {
        // The width check is what stops a 40-character prefix of a 64-character
        // object id from being accepted as that commit.
        assert!(!hex_oid_matches_commit(
            HexOidPrefix(&SHA256[..40]),
            CommitId(SHA256)
        ));
    }

    #[test]
    fn a_different_object_id_of_equal_width_is_rejected() {
        let other = "f".repeat(40);
        assert!(!hex_oid_matches_commit(
            HexOidPrefix(&other),
            CommitId(SHA1)
        ));
    }

    // --- enforce_leading_hex_oid: the composed gate ---

    #[test]
    fn decorated_abbreviation_is_rejected_before_any_resolution() {
        // `resolved: None` is the pre-git call. Catching it here is what keeps
        // the check ahead of repository mutation.
        let err = enforce_leading_hex_oid(StartPoint("deadbeef~0"), None).unwrap_err();
        assert!(
            format!("{err:?}").contains("full 40- or 64-character object id"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn a_symbolic_ref_passes_the_gate_at_both_stages() {
        assert!(enforce_leading_hex_oid(StartPoint("refs/heads/main"), None).is_ok());
        assert!(
            enforce_leading_hex_oid(StartPoint("refs/heads/main"), Some(CommitId(SHA1))).is_ok()
        );
    }

    #[test]
    fn a_full_object_id_must_equal_the_resolved_commit() {
        assert!(enforce_leading_hex_oid(StartPoint(SHA1), Some(CommitId(SHA1))).is_ok());
        let other = "f".repeat(40);
        assert!(enforce_leading_hex_oid(StartPoint(&other), Some(CommitId(SHA1))).is_err());
    }
}
