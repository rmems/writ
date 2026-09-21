//! Supervised command policy: argv allowlist, wrapper refusal, and repo bind.
//!
//! Split from [`super`] so hang-recovery stays under CodeScene's file-size gate.
//! Recovery still never merges, never bare-force-pushes, and never deletes a checkout.

use std::path::PathBuf;

use crate::error::{Error, PolicyCode, Result};
use crate::git_safe::{SafeGhCommand, SafeGitCommand};
use crate::owners::OwnerAllowlist;

use super::RunOptions;

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

/// Normalize an executable path to a basename without platform extensions.
#[must_use]
pub fn normalize_program_name(program: &str) -> String {
    // Accept both Unix and Windows separators even when running on Linux (tests / cross config).
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(program);
    let lower = base.to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower)
        .to_owned()
}

/// Enforce safety policy for a supervised command (used by CLI and core).
pub fn check_command_policy(program: &str, args: &[&str], options: &RunOptions) -> Result<()> {
    prepare_supervised_command(program, args, options).map(|_| ())
}

pub(super) fn prepare_supervised_command(
    program: &str,
    args: &[&str],
    options: &RunOptions,
) -> Result<PreparedCommand> {
    let name = normalize_program_name(program);
    let owned_args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();

    // Shells, interpreters, launchers, and direct network clients are never policy-safe
    // under substring checks; require direct allowlisted binaries for sensitive actions.
    if is_forbidden_wrapper(&name) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            message: format!(
                "supervised program `{name}` can launch or tunnel unreviewed commands and is not allowed; invoke git, gh, or another binary directly"
            ),
        });
    }

    match name.as_str() {
        "git" => prepare_git_command(owned_args, options),
        "gh" => prepare_gh_command(owned_args, options),
        _ => {
            // Fail closed on path-qualified / relative scripts (./tools/run, tools/run).
            // Basename-only PATH lookups remain for non-sensitive tooling; git/gh above are
            // always PATH-forced. Shebang wrappers in the worktree cannot be invoked by path.
            if program_is_path_qualified(program) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::SubcommandNotAllowed,
                    message: format!(
                        "supervised program `{program}` is path-qualified; invoke a PATH binary by basename only (git, gh, …)"
                    ),
                });
            }
            Ok(PreparedCommand {
                program: program.to_owned(),
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
fn prepare_git_command(owned_args: Vec<String>, options: &RunOptions) -> Result<PreparedCommand> {
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
    reject_supervised_checkout_mismatch(expected.as_deref(), &owned_args)?;
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
fn prepare_gh_command(owned_args: Vec<String>, options: &RunOptions) -> Result<PreparedCommand> {
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

fn reject_supervised_checkout_mismatch(expected: Option<&str>, args: &[String]) -> Result<()> {
    let (Some(exp), Some(target)) = (expected, crate::git_safe::checkout_or_switch_target(args))
    else {
        return Ok(());
    };
    if target == exp || target == "HEAD" {
        return Ok(());
    }
    Err(Error::PolicyViolation {
        code: PolicyCode::BranchMismatch,
        message: format!(
            "git checkout/switch target `{target}` must equal --expected-branch `{exp}`"
        ),
    })
}

fn program_is_path_qualified(program: &str) -> bool {
    program.contains('/')
        || program.contains('\\')
        || program.starts_with('.')
        || (program.len() > 2 && program.as_bytes().get(1) == Some(&b':'))
}

const FORBIDDEN_WRAPPERS: &[&str] = &[
    "bash",
    "bun",
    "chroot",
    "cmd",
    "curl",
    "dash",
    "deno",
    "doas",
    "env",
    "fish",
    "http",
    "httpie",
    "ipython",
    "ipython3",
    "lua",
    "nc",
    "ncat",
    "netcat",
    "nice",
    "node",
    "nodejs",
    "nohup",
    "nsenter",
    "open",
    "perl",
    "php",
    "powershell",
    "pwsh",
    "py",
    "python",
    "python2",
    "python3",
    "rscript",
    "ruby",
    "script",
    "setsid",
    "sh",
    "socat",
    "stdbuf",
    "su",
    "sudo",
    "time",
    "timeout",
    "unshare",
    "wget",
    "xargs",
    "xdg-open",
    "zsh",
];

const VERSIONED_WRAPPER_PREFIXES: &[&str] = &[
    "python", "python2", "python3", "perl", "ruby", "node", "nodejs", "php", "lua", "ipython",
];

fn is_forbidden_wrapper(name: &str) -> bool {
    FORBIDDEN_WRAPPERS.contains(&name) || versioned_wrapper_name(name)
}

fn versioned_wrapper_name(name: &str) -> bool {
    VERSIONED_WRAPPER_PREFIXES.iter().any(|prefix| {
        let Some(rest) = name.strip_prefix(prefix) else {
            return false;
        };
        rest.is_empty()
            || rest.starts_with('.')
            || rest.chars().next().is_some_and(|c| c.is_ascii_digit())
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
