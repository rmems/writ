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
