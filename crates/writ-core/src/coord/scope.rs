//! Declared-path versus actual-change comparison for a leased checkout.
//!
//! Declarations are intent; the checkout is evidence. Comparing the two at
//! handoff (or on demand) makes a stale declaration visible to the target
//! without turning declared paths into a write veto.

use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::git_cmd::git_in;
use crate::lease::Lease;

use super::types::CoordClaim;

/// Declared paths compared with the paths the checkout actually changed
/// since the lease start commit (committed, staged, unstaged, or untracked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeDrift {
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub agent_id: String,
    pub branch: String,
    pub worktree_path: String,
    pub start_commit: String,
    pub owner_generation: i64,
    /// Normalized declaration from the latest `announce`.
    pub declared_paths: Vec<String>,
    /// Every path changed relative to `start_commit`, sorted and unique.
    pub changed_paths: Vec<String>,
    /// Changed paths that no declared path covers: the declaration is stale.
    pub undeclared_paths: Vec<String>,
    /// Declared paths with no change beneath them (yet).
    pub untouched_declared_paths: Vec<String>,
}

impl ScopeDrift {
    pub(super) fn build(claim: &CoordClaim, lease: &Lease, changed: Vec<String>) -> Self {
        let undeclared = undeclared(&claim.declared_paths, &changed);
        let untouched = claim
            .declared_paths
            .iter()
            .filter(|declared| !changed.iter().any(|path| covers(declared, path)))
            .cloned()
            .collect();
        Self {
            owner: claim.owner.clone(),
            repo_name: claim.repo_name.clone(),
            job_id: claim.job_id.clone(),
            agent_id: claim.agent_id.clone(),
            branch: claim.branch.clone(),
            worktree_path: claim.worktree_path.clone(),
            start_commit: lease.start_commit.clone(),
            owner_generation: claim.owner_generation,
            declared_paths: claim.declared_paths.clone(),
            changed_paths: changed,
            undeclared_paths: undeclared,
            untouched_declared_paths: untouched,
        }
    }
}

/// True when a normalized declaration covers a repo-relative changed path.
pub(super) fn covers(declared: &str, path: &str) -> bool {
    declared == "."
        || declared == path
        || path
            .strip_prefix(declared)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Paths this job touched in `worktree` since `start_commit`:
///
/// - every path named by a commit on the branch's first-parent history after
///   `start_commit`, merges excluded, so integrating the base branch does not
///   attribute upstream changes to this job;
/// - staged and unstaged changes relative to `HEAD`;
/// - untracked files that are not ignored.
///
/// Renames are reported as the old and the new path. Read-only; never touches
/// the index or the checkout. A path that is not valid UTF-8 is an error
/// rather than a lossy guess, since declarations are UTF-8 strings.
pub(super) fn changed_paths(worktree: &Path, start_commit: &str) -> Result<Vec<String>> {
    let range = if start_commit.is_empty() {
        "HEAD".to_owned()
    } else {
        format!("{start_commit}..HEAD")
    };
    let mut paths = nul_separated(run(
        worktree,
        &[
            "log",
            "--first-parent",
            "--no-merges",
            "--no-renames",
            "--name-only",
            "--format=",
            "-z",
            &range,
            "--",
        ],
    )?)?;
    paths.extend(nul_separated(run(
        worktree,
        &["diff", "--name-only", "--no-renames", "-z", "HEAD", "--"],
    )?)?);
    paths.extend(nul_separated(run(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?)?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Changed paths that no declared path covers.
pub(super) fn undeclared(declared: &[String], changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|path| !declared.iter().any(|d| covers(d, path)))
        .cloned()
        .collect()
}

fn run(worktree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_in(worktree, args).map_err(|error| Error::LeaseStore {
        context: "inspect changed paths",
        message: format!("git {} in {}: {error}", args[0], worktree.display()),
    })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(Error::LeaseStore {
        context: "inspect changed paths",
        message: format!(
            "git {} in {} failed: {}",
            args[0],
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

fn nul_separated(bytes: Vec<u8>) -> Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .map(|entry| entry.strip_prefix(b"\n").unwrap_or(entry))
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            std::str::from_utf8(entry)
                .map(|path| path.replace('\\', "/"))
                .map_err(|_| Error::LeaseStore {
                    context: "inspect changed paths",
                    message: format!(
                        "changed path is not valid UTF-8: {}",
                        String::from_utf8_lossy(entry)
                    ),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::covers;

    #[test]
    fn coverage_is_exact_or_ancestor_only() {
        assert!(covers(".", "anything/at/all"));
        assert!(covers("src/a", "src/a"));
        assert!(covers("src/a", "src/a/x.rs"));
        assert!(!covers("src/a", "src/ab/x.rs"));
        assert!(!covers("src/a/x.rs", "src/a"));
    }
}
