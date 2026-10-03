//! Direct git/gh checks and harness-selected working directories for supervision.
//!
//! Split from [`super`] so hang-recovery stays under CodeScene's file-size gate.
//! Recovery still never merges, never bare-force-pushes, and never deletes a checkout.

use std::path::PathBuf;

use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::owners::OwnerAllowlist;

use super::{RunOptions, normalize_program_name};

/// Bundled argv + options so this module is not scored as string-argument-heavy.
pub(super) struct CommandRequest<'a> {
    pub(super) program: &'a str,
    pub(super) args: &'a [&'a str],
    pub(super) options: &'a RunOptions,
}

/// Deferred branch verification performed after the concurrency permit is held.
pub(super) struct BranchCheck {
    pub(super) expected_branch: String,
    pub(super) repo: PathBuf,
}

/// Normalized program + args ready to spawn after policy checks.
pub(super) struct PreparedCommand {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    /// Working directory for the child (verified git repo when applicable).
    pub(super) cwd: Option<PathBuf>,
    /// When set, re-verify branch immediately before spawn (post-permit).
    pub(super) branch_check: Option<BranchCheck>,
}

/// Enforce safety policy for a supervised command (used by CLI and core).
pub fn check_command_policy(program: &str, args: &[&str], options: &RunOptions) -> Result<()> {
    prepare_supervised_command(&CommandRequest {
        program,
        args,
        options,
    })
    .map(|_| ())
}

pub(super) fn prepare_supervised_command(req: &CommandRequest<'_>) -> Result<PreparedCommand> {
    let name = normalize_program_name(req.program);
    let owned_args: Vec<String> = req.args.iter().map(|s| (*s).to_owned()).collect();

    match name.as_str() {
        "git" => prepare_git_command(req),
        "gh" => prepare_gh_command(req),
        _ => Ok(PreparedCommand {
            program: req.program.to_owned(),
            args: owned_args,
            cwd: req
                .options
                .repo
                .as_deref()
                .map(|repo| resolve_supervised_repo(Some(repo)))
                .transpose()?,
            branch_check: None,
        }),
    }
}

/// Prepare a supervised `git` command: enforce argv policy, resolve the required
/// expected-branch for mutating commands, bind checkout/switch and push targets
/// to that branch, and PATH-force the `git` binary.
fn prepare_git_command(prep: &CommandRequest<'_>) -> Result<PreparedCommand> {
    let owned_args: Vec<String> = prep.args.iter().map(|s| (*s).to_owned()).collect();
    let options = prep.options;
    let safe = SafeGitCommand::new(&owned_args)?;
    let expected = if safe.requires_branch_check() {
        Some(
            options
                .expected_branch
                .clone()
                .ok_or_else(|| Error::PolicyViolation {
                    code: PolicyCode::BranchMismatch,
                    message: "mutating git commands require --expected-branch under supervisor"
                        .to_owned(),
                })?,
        )
    } else {
        None
    };
    super::reject_mismatched_checkout(expected.as_deref(), &owned_args)?;
    if let Some(exp) = expected.as_deref() {
        crate::git_safe::reject_push_outside_expected_branch(&owned_args, exp)?;
    }
    // Always spawn PATH `git`, never a user-supplied path-qualified binary.
    let repo = resolve_supervised_repo(options.repo.as_deref())?;
    let branch_check = expected.map(|expected_branch| BranchCheck {
        expected_branch,
        repo: repo.clone(),
    });
    Ok(PreparedCommand {
        program: "git".to_owned(),
        args: owned_args,
        cwd: Some(repo),
        branch_check,
    })
}

/// Validate direct GitHub CLI operations without requiring local branch state.
/// An optional working directory remains useful for gh's implicit repo lookup.
fn prepare_gh_command(prep: &CommandRequest<'_>) -> Result<PreparedCommand> {
    let args: Vec<String> = prep.args.iter().map(|s| (*s).to_owned()).collect();
    let allowlist = prep
        .options
        .allowlist
        .clone()
        .unwrap_or_else(OwnerAllowlist::from_env);
    SafeGhCommand::with_allowlist(&args, &allowlist)?;
    Ok(PreparedCommand {
        program: "gh".to_owned(),
        args,
        cwd: prep
            .options
            .repo
            .as_deref()
            .map(|repo| resolve_supervised_repo(Some(repo)))
            .transpose()?,
        branch_check: None,
    })
}

/// Check assigned branch identity and preserve uncommitted WIP immediately
/// before a supervised Git command is spawned.
pub(super) fn verify_repo_branch(
    repo: &std::path::Path,
    expected_branch: &str,
    args: &[String],
) -> Result<()> {
    let cmd = SafeGitCommand::new(args)?;
    cmd.verify_branch(repo, expected_branch)?;
    cmd.admit_local_merge(repo)
}

fn resolve_supervised_repo(repo: Option<&std::path::Path>) -> Result<PathBuf> {
    use std::path::Path;

    let raw = repo.unwrap_or_else(|| Path::new("."));
    let canon = crate::paths::canonicalize_for_tools(raw).map_err(|e| Error::Io {
        context: "canonicalize supervised --repo",
        source: e,
    })?;
    if !canon.is_dir() {
        return Err(Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: format!(
                "supervised --repo must be an existing directory: {}",
                canon.display()
            ),
        });
    }

    Ok(canon)
}
