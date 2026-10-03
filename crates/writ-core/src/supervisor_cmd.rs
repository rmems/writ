//! Supervised command policy: argv allowlist, wrapper refusal, and repo bind.
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

    // Shells, interpreters, launchers, and direct network clients are never policy-safe
    // under substring checks; require direct allowlisted binaries for sensitive actions.
    if super::is_forbidden_wrapper(&name) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            message: format!(
                "supervised program `{name}` can launch or tunnel unreviewed commands and is not allowed; invoke git, gh, or another binary directly"
            ),
        });
    }

    match name.as_str() {
        "git" => prepare_git_command(req),
        "gh" => prepare_gh_command(req),
        _ => {
            // Fail closed on path-qualified / relative scripts (./tools/run, tools/run).
            // Basename-only PATH lookups remain for non-sensitive tooling; git/gh above are
            // always PATH-forced. Shebang wrappers in the worktree cannot be invoked by path.
            if super::program_is_path_qualified(req.program) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::SubcommandNotAllowed,
                    message: format!(
                        "supervised program `{}` is path-qualified; invoke a PATH binary by basename only (git, gh, …)",
                        req.program
                    ),
                });
            }
            Ok(PreparedCommand {
                program: req.program.to_owned(),
                args: owned_args,
                cwd: None,
                branch_check: None,
            })
        }
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

/// Prepare a supervised `gh` command: enforce argv/allowlist policy and, for
/// mutating `gh pr` commands, require an expected branch and bind the effective
/// repo selector (explicit `-R` or implicit `GH_REPO`) to the verified local
/// origin before PATH-forcing the `gh` binary.
fn prepare_gh_command(prep: &CommandRequest<'_>) -> Result<PreparedCommand> {
    let owned_args: Vec<String> = prep.args.iter().map(|s| (*s).to_owned()).collect();
    let options = prep.options;
    let allowlist = options
        .allowlist
        .clone()
        .unwrap_or_else(OwnerAllowlist::from_env);
    let _safe = SafeGhCommand::with_allowlist(&owned_args, &allowlist)?;
    // Always spawn PATH `gh`, never a user-supplied path-qualified binary.
    let (cwd, branch_check, owned_args) = if crate::git_safe::gh_requires_branch_check(&owned_args)
    {
        let expected = options
            .expected_branch
            .clone()
            .ok_or_else(|| Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                message: "mutating gh pr commands require --expected-branch under supervisor"
                    .to_owned(),
            })?;
        let repo = resolve_supervised_repo(options.repo.as_deref())?;
        // Bind the effective repo selector to the verified local checkout so jobs
        // cannot mutate a different GitHub repository after the branch gate. An
        // explicit `-R/--repo` wins; otherwise gh reads the implicit `GH_REPO`
        // environment selector, so bind that too.
        let env_selector = crate::git_safe::gh_repo_env_target();
        let local = crate::git_safe::origin_github_slug(&repo)?;
        crate::git_safe::bind_gh_repo_selector_to_origin(
            &owned_args,
            env_selector.as_deref(),
            &local,
        )?;
        // Pin the validated slug before any later permit wait so a TOCTOU
        // origin rewrite cannot retarget `gh`.
        let owned_args = if env_selector.is_some() {
            owned_args
        } else {
            crate::git_safe::pin_gh_repo_selector(owned_args, &local)
        };
        (
            Some(repo.clone()),
            Some(BranchCheck {
                expected_branch: expected,
                repo,
            }),
            owned_args,
        )
    } else {
        (None, None, owned_args)
    };
    Ok(PreparedCommand {
        program: "gh".to_owned(),
        args: owned_args,
        cwd,
        branch_check,
    })
}

/// Resolve and validate `--repo` for supervised git (cwd + branch checks).
///
/// - Rejects `..` path components in the input.
/// - Canonicalizes to an existing directory.
/// - Requires the path to stay under `WRIT_WORKTREE_BASE` when set, otherwise under
///   the documented default `{user_data_dir}/writ/worktrees` root.
pub(super) fn verify_repo_branch(repo: &std::path::Path, expected_branch: &str) -> Result<()> {
    let cmd = SafeGitCommand::new(&["rev-parse".to_owned(), "HEAD".to_owned()])?;
    cmd.verify_branch(repo, expected_branch)
}

fn resolve_supervised_repo(repo: Option<&std::path::Path>) -> Result<PathBuf> {
    use std::path::{Component, Path};

    let worktree_base = crate::paths::worktree_base_path()?;

    let raw = repo.unwrap_or_else(|| Path::new("."));
    if raw.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: "parent-directory components are not allowed in --repo".to_owned(),
        });
    }

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

    let base = normalize_existing_or_future_dir(&worktree_base)?;
    if !canon.starts_with(&base) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: format!(
                "supervised --repo `{}` escapes worktree base `{}`",
                canon.display(),
                base.display()
            ),
        });
    }

    Ok(canon)
}

fn normalize_existing_or_future_dir(path: &std::path::Path) -> Result<PathBuf> {
    if path.exists() {
        return crate::paths::canonicalize_for_tools(path).map_err(|e| Error::Io {
            context: "canonicalize worktree base",
            source: e,
        });
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| Error::Io {
                context: "resolve worktree base",
                source: e,
            })
    }
}
