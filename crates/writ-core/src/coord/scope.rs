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
///   from a base ref. The base refs are the ones git itself records: each
///   remote's default branch (`refs/remotes/<remote>/HEAD`) and the branch's
///   configured upstream, unless that upstream is the remote copy of the
///   branch itself. Merging the base, or rebasing onto a fetched base,
///   therefore does not attribute upstream changes to the job, while a
///   pushed copy or stacked child of the branch is never a base, so the
///   job's own commits stay attributed to it however far the branch moves on.
///   A fast-forward or rebase onto base commits that no base ref records
///   (local-only, or a remote without a default head or tracking config)
///   still counts them until the checkout is re-registered (which refreshes
///   `start_commit`);
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
    let upstream = base_refs(checkout)?;
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

/// Remote-tracking refs that stand for the job's base: every remote's
/// default branch (the `refs/remotes/<remote>/HEAD` symref target) and the
/// branch's configured upstream, except when that upstream is the remote copy
/// of the branch itself. Only refs that exist locally are returned, so a
/// dangling default head or an unfetched upstream cannot fail the inspection.
fn base_refs(checkout: Checkout<'_>) -> Result<Vec<String>> {
    let remote_refs = ref_records(run(
        checkout.worktree,
        &[
            "--no-optional-locks",
            "for-each-ref",
            "--format=%(refname)%00%(symref)",
            "refs/remotes",
        ],
    )?)?;
    let mut bases: Vec<String> = remote_refs
        .iter()
        .filter(|(name, symref)| name.ends_with("/HEAD") && !symref.is_empty())
        .map(|(_, symref)| symref.clone())
        .collect();
    bases.extend(configured_upstream(checkout)?);
    bases.retain(|base| remote_refs.iter().any(|(name, _)| name == base));
    bases.sort();
    bases.dedup();
    Ok(bases)
}

/// The branch's configured upstream ref, unless it is `<remote>/<branch>`,
/// which is the job's own pushed copy rather than its base.
fn configured_upstream(checkout: Checkout<'_>) -> Result<Option<String>> {
    let branch_ref = format!("refs/heads/{}", checkout.branch);
    let records = ref_records(run(
        checkout.worktree,
        &[
            "--no-optional-locks",
            "for-each-ref",
            "--format=%(upstream)%00%(upstream:remotename)",
            branch_ref.as_str(),
        ],
    )?)?;
    Ok(records.into_iter().find_map(|(upstream, remote)| {
        let own_copy = format!("refs/remotes/{remote}/{}", checkout.branch);
        (!upstream.is_empty() && upstream != own_copy).then_some(upstream)
    }))
}

/// `for-each-ref` records of the form `<field>NUL<field>`, one per line.
fn ref_records(bytes: Vec<u8>) -> Result<Vec<(String, String)>> {
    let text = utf8(&bytes)?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .map(|(first, second)| (first.to_owned(), second.to_owned()))
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
    utf8(entry).map(str::to_owned)
}

fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| Error::LeaseStore {
        context: "inspect changed paths",
        message: format!(
            "changed path is not valid UTF-8: {}",
            String::from_utf8_lossy(bytes)
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
