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
///   `start_commit`, excluding merge commits and any commit already reachable
///   from an upstream remote-tracking ref. A remote ref counts as upstream
///   when it is not this branch's own and does not contain `HEAD`; a pushed
///   stacked child or copy of the branch contains `HEAD` and is ignored, so
///   the job's own commits stay attributed to it. Merging the base branch, or
///   rebasing onto a fetched base, therefore does not attribute upstream
///   changes to the job. A fast-forward or rebase onto local-only base
///   commits still counts them until the checkout is re-registered (which
///   refreshes `start_commit`);
/// - staged and unstaged changes relative to `HEAD`;
/// - untracked files that are not ignored.
///
/// Renames are reported as the old and the new path. Working-tree paths come
/// from `git status`, which honors `--no-optional-locks`. `git diff` does not:
/// it still refreshes stat-only changes and takes `index.lock`. A path that is
/// not valid UTF-8 is an error rather than a lossy guess, since declarations
/// are UTF-8 strings.
pub(super) fn changed_paths(checkout: Checkout<'_>) -> Result<Vec<String>> {
    let worktree = checkout.worktree;
    let range = if checkout.start_commit.is_empty() {
        "HEAD".to_owned()
    } else {
        format!("{}..HEAD", checkout.start_commit)
    };
    let upstream = upstream_remote_refs(checkout)?;
    let mut log_args = vec![
        "--no-optional-locks",
        "log",
        "--first-parent",
        "--no-merges",
        "--no-renames",
        "--name-only",
        "--format=",
        "-z",
        range.as_str(),
    ];
    if !upstream.is_empty() {
        log_args.push("--not");
        log_args.extend(upstream.iter().map(String::as_str));
    }
    log_args.push("--");
    let mut paths = nul_separated(run(worktree, &log_args)?)?;
    paths.extend(porcelain_paths(run(
        worktree,
        &[
            "--no-optional-locks",
            "-c",
            "status.renames=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
        ],
    )?)?);
    paths.extend(nul_separated(run(
        worktree,
        &[
            "--no-optional-locks",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?)?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The registered checkout to inspect: its path, branch, and lease start.
#[derive(Debug, Clone, Copy)]
pub(super) struct Checkout<'a> {
    pub(super) worktree: &'a Path,
    pub(super) branch: &'a str,
    pub(super) start_commit: &'a str,
}

/// Remote-tracking refs that stand for upstream: every remote ref except the
/// branch's own (`*/<branch>`) and any ref that already contains `HEAD`,
/// which is a pushed copy or stacked child of this branch, not its base.
fn upstream_remote_refs(checkout: Checkout<'_>) -> Result<Vec<String>> {
    let own = format!("/{}", checkout.branch);
    let refs = nul_separated(run(
        checkout.worktree,
        &[
            "--no-optional-locks",
            "for-each-ref",
            "--format=%(refname)%00",
            "--no-contains=HEAD",
            "refs/remotes",
        ],
    )?)?;
    Ok(refs
        .into_iter()
        .filter(|name| !name.ends_with(&own))
        .collect())
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
    let command = git_subcommand(args);
    let output = git_in(worktree, args).map_err(|error| Error::LeaseStore {
        context: "inspect changed paths",
        message: format!("git {command} in {}: {error}", worktree.display()),
    })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(Error::LeaseStore {
        context: "inspect changed paths",
        message: format!(
            "git {command} in {} failed: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    })
}

/// First non-option argument. Skips a `-c <value>` pair so the error names
/// `status` rather than the config that precedes it.
fn git_subcommand<'a>(args: &'a [&'a str]) -> &'a str {
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        if arg == "-c" {
            let _ = args.next();
            continue;
        }
        if !arg.starts_with('-') {
            return arg;
        }
    }
    "git"
}

fn nul_separated(bytes: Vec<u8>) -> Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .map(|entry| entry.strip_prefix(b"\n").unwrap_or(entry))
        .filter(|entry| !entry.is_empty())
        .map(changed_path)
        .collect()
}

/// Porcelain v1 `-z` records are `XY PATH` plus a NUL. A rename or copy adds
/// the original path as the next record. Renames are disabled on the command,
/// and both sides are kept if a rename record still appears.
fn porcelain_paths(bytes: Vec<u8>) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    let mut entries = bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let record = changed_path(entry)?;
        let path = record
            .get(3..)
            .filter(|path| !path.is_empty() && record.len() >= 4 && record.as_bytes()[2] == b' ');
        let Some(path) = path else {
            return Err(Error::LeaseStore {
                context: "inspect changed paths",
                message: format!("unrecognized git status record: {record}"),
            });
        };
        paths.push(path.to_owned());
        let code = &record[..2];
        if code.contains('R') || code.contains('C') {
            let Some(old) = entries.next() else {
                return Err(Error::LeaseStore {
                    context: "inspect changed paths",
                    message: "git status rename record is missing its original path".to_owned(),
                });
            };
            paths.push(changed_path(old)?);
        }
    }
    Ok(paths)
}

fn changed_path(entry: &[u8]) -> Result<String> {
    std::str::from_utf8(entry)
        .map(str::to_owned)
        .map_err(|_| Error::LeaseStore {
            context: "inspect changed paths",
            message: format!(
                "changed path is not valid UTF-8: {}",
                String::from_utf8_lossy(entry)
            ),
        })
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
