//! Allowlisted git and GitHub CLI operations.
//!
//! Safety invariants enforced at the Rust core boundary:
//! - Only allowlisted git subcommands may be executed.
//! - Bare `--force` / `-f` is always rejected; only `--force-with-lease` is permitted.
//! - Merge is blocked only when it is the git subcommand (branch names like `merge` are allowed).
//! - `gh pr merge` and merge-related flags are blocked; `gh api` is not allowlisted.
//! - Mutating commands verify the current branch when `expected_branch` is provided to `run`.
//! - All policy violations carry stable structured error codes.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};

/// Git subcommands allowed for hive jobs.
const ALLOWED_GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "branch",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "config",
    "diff",
    "fetch",
    "log",
    "ls-files",
    "ls-remote",
    "merge-base",
    "mv",
    "pull",
    "push",
    "rebase",
    "remote",
    "reset",
    "restore",
    "rev-parse",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
];

/// Git subcommands that mutate branch state and require branch verification.
const MUTATING_SUBCOMMANDS: &[&str] = &[
    "add",
    "branch",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "config",
    "mv",
    "pull",
    "push",
    "rebase",
    "remote",
    "reset",
    "restore",
    "rm",
    "stash",
    "switch",
    "tag",
];

/// GitHub CLI subcommands allowed for hive jobs.
///
/// Note: `api` is intentionally excluded so merge-related REST/GraphQL cannot be
/// invoked through `gh api` (e.g. `mergePullRequest` / REST merge endpoints).
const ALLOWED_GH_SUBCOMMANDS: &[&str] = &[
    "auth", "browse", "gist", "issue", "label", "pr", "release", "repo", "secret", "ssh-key",
    "variable", "workflow",
];

/// `gh pr` sub-subcommands that are blocked (merge / merge-like updates).
///
/// `update-branch` defaults to a merge commit unless `--rebase` is passed; hive policy
/// rejects merge-style updates entirely (and `--rebase` is already a blocked flag).
const BLOCKED_GH_PR_SUBSUBCOMMANDS: &[&str] = &[
    "merge",
    "ready",
    "update-branch",
    // Switches the worktree to the PR branch; leaves assigned job branch.
    "checkout",
];

/// `gh pr` flags that are blocked (direct merge-related flags).
const BLOCKED_GH_FLAGS: &[&str] = &["--merge", "--squash", "--rebase", "--auto", "--admin"];

/// Pre-validated git command ready for execution.
#[derive(Debug, Clone)]
pub struct SafeGitCommand {
    args: Vec<String>,
}

/// Output from executing a safe git or gh command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl SafeGitCommand {
    /// Create a new safe git command after validating the full argument list.
    ///
    /// Returns an error if the command violates any safety policy.
    pub fn new(args: &[String]) -> Result<Self> {
        if args.is_empty() {
            return Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                message: "no git subcommand provided".to_owned(),
            });
        }

        let subcommand = &args[0];

        // Reject merge only when it is the git subcommand (not a branch/ref named "merge").
        if is_merge_subcommand(subcommand) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                message: format!("merge is not allowed: `git {}`", args.join(" ")),
            });
        }

        // Validate subcommand against allowlist.
        let allowed: HashSet<&str> = ALLOWED_GIT_SUBCOMMANDS.iter().copied().collect();
        if !allowed.contains(subcommand.as_str()) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                message: format!("git subcommand `{subcommand}` is not on the allowlist"),
            });
        }

        // Reject ANY bare --force / -f always; only --force-with-lease is allowed.
        // Exact `-f` / `--force` apply to all subcommands; combined short clusters
        // like `-fu` are only meaningful (and checked) for `push`.
        if args.iter().any(|a| is_bare_force_flag(a)) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                message: "bare --force/-f is not allowed; use --force-with-lease only".to_owned(),
            });
        }

        if subcommand == "push" {
            // Combined short options: `git push -fu origin main`
            if args.iter().any(|a| is_combined_short_force_cluster(a)) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    message: "bare --force/-f is not allowed; use --force-with-lease only"
                        .to_owned(),
                });
            }
            // Force via `+<src>:<dst>` refspecs.
            if args.iter().skip(1).any(|a| is_force_refspec(a)) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    message: "force-push refspecs prefixed with `+` are not allowed; use --force-with-lease only".to_owned(),
                });
            }
            // `--mirror` force-updates and deletes remote refs without a lease.
            if args.iter().any(|a| a == "--mirror") {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    message: "git push --mirror is not allowed; use --force-with-lease only"
                        .to_owned(),
                });
            }
            // Remote ref deletion (`--delete` / `-d` / `:ref` refspecs).
            if args.iter().any(|a| is_push_delete_flag(a))
                || args.iter().skip(1).any(|a| is_delete_refspec(a))
            {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    message:
                        "git push --delete / delete refspecs are not allowed under hive policy"
                            .to_owned(),
                });
            }
            // `--prune` deletes remote refs absent locally under a matching refspec.
            if args.iter().any(|a| a == "--prune") {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BareForcePush,
                    message: "git push --prune is not allowed under hive policy".to_owned(),
                });
            }
            // Broad multi-ref pushes can update branches other than the job branch.
            if args.iter().any(|a| a == "--all") {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::BranchMismatch,
                    message: "git push --all is not allowed under hive policy".to_owned(),
                });
            }
        }

        if subcommand == "pull" && !pull_uses_safe_history_strategy(args) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                message: "git pull requires --rebase (not false) or --ff-only under hive policy"
                    .to_owned(),
            });
        }

        if subcommand == "rebase" && args.iter().any(|a| is_rebase_exec_option(a)) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                message: "git rebase --exec/-x is not allowed under hive policy".to_owned(),
            });
        }

        // Transport helpers that run arbitrary local commands (`--receive-pack`, etc.).
        if matches!(
            subcommand.as_str(),
            "push" | "pull" | "fetch" | "clone" | "ls-remote"
        ) && args
            .iter()
            .any(|a| is_remote_helper_exec_option(a, subcommand))
        {
            return Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                message: "git remote helper exec options (--receive-pack/--upload-pack/--exec) are not allowed"
                    .to_owned(),
            });
        }

        // `ext::<command>` remote helper runs arbitrary local commands.
        if args.iter().any(|a| is_ext_transport_url(a)) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                message: "git ext:: transport URLs are not allowed under hive policy".to_owned(),
            });
        }

        // Branch rename/copy leaves the worktree on a different branch name.
        if subcommand == "branch"
            && args
                .iter()
                .skip(1)
                .any(|a| is_branch_rename_or_copy_flag(a))
        {
            return Err(Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                message: "git branch rename/copy (-m/-M/-c/-C) is not allowed under hive policy"
                    .to_owned(),
            });
        }

        // Detach leaves HEAD off the assigned branch even when the target name matches.
        if matches!(subcommand.as_str(), "checkout" | "switch")
            && args.iter().skip(1).any(|a| is_detach_flag(subcommand, a))
        {
            return Err(Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                message: "git checkout/switch --detach is not allowed under hive policy".to_owned(),
            });
        }

        reject_external_write_targets(subcommand, &args[1..])?;

        Ok(Self {
            args: args.to_vec(),
        })
    }

    /// The validated git subcommand.
    #[must_use]
    pub fn subcommand(&self) -> &str {
        &self.args[0]
    }

    /// Whether this command requires branch verification before execution.
    #[must_use]
    pub fn requires_branch_check(&self) -> bool {
        let allowed: HashSet<&str> = MUTATING_SUBCOMMANDS.iter().copied().collect();
        allowed.contains(self.subcommand())
    }

    /// Verify that the current branch matches the expected job branch.
    ///
    /// Resolves the current branch from the repository at `repo_dir` and compares it
    /// against `expected_branch`. Returns `Ok(())` on match, error otherwise.
    pub fn verify_branch(&self, repo_dir: &Path, expected_branch: &str) -> Result<()> {
        let current = resolve_current_branch(repo_dir)?;
        if current != expected_branch {
            return Err(Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                message: format!(
                    "current branch `{current}` does not match expected `{expected_branch}`"
                ),
            });
        }
        Ok(())
    }

    /// Execute the validated git command in `repo_dir`.
    ///
    /// When `expected_branch` is `Some` and this command is mutating, verifies the
    /// current branch before running.
    pub fn run(&self, repo_dir: &Path, expected_branch: Option<&str>) -> Result<GitOutput> {
        if self.requires_branch_check()
            && let Some(expected) = expected_branch
        {
            self.verify_branch(repo_dir, expected)?;
        }

        let output = Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(&self.args)
            .output()
            .map_err(|e| Error::Io {
                context: "spawn git",
                source: e,
            })?;

        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(1),
        })
    }

    /// Return the full argument list (for display / logging).
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Pre-validated GitHub CLI command ready for execution.
#[derive(Debug, Clone)]
pub struct SafeGhCommand {
    args: Vec<String>,
}

impl SafeGhCommand {
    /// Create a new safe gh command after validating the full argument list.
    ///
    /// Returns an error if the command violates any safety policy.
    pub fn new(args: &[String]) -> Result<Self> {
        if args.is_empty() {
            return Err(Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                message: "no gh subcommand provided".to_owned(),
            });
        }

        let subcommand = &args[0];

        // Validate subcommand against allowlist.
        let allowed: HashSet<&str> = ALLOWED_GH_SUBCOMMANDS.iter().copied().collect();
        if !allowed.contains(subcommand.as_str()) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                message: format!("gh subcommand `{subcommand}` is not on the allowlist"),
            });
        }

        // Block `gh pr merge` / `ready` / `update-branch` even when inherited flags
        // precede the subcommand, e.g. `gh pr -R owner/repo merge 1`.
        if subcommand == "pr"
            && let Some(pr_sub) = first_positional_after(&args[1..])
        {
            let blocked: HashSet<&str> = BLOCKED_GH_PR_SUBSUBCOMMANDS.iter().copied().collect();
            if blocked.contains(pr_sub) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::MergeBlocked,
                    message: format!("`gh pr {pr_sub}` is not allowed"),
                });
            }
        }

        // `gh repo clone <repo> [<dir>]` can write outside the worktree.
        if subcommand == "repo"
            && let Some(repo_sub) = first_positional_after(&args[1..])
            && repo_sub == "clone"
        {
            // Find destination after `clone` token (skip option values).
            if let Some(dest) = gh_repo_clone_destination(&args[1..]) {
                reject_external_path(Some(dest), "gh repo clone destination")?;
            }
        }

        // Block merge-related flags anywhere in the argument list.
        let blocked_flags: HashSet<&str> = BLOCKED_GH_FLAGS.iter().copied().collect();
        for arg in &args[1..] {
            if blocked_flags.contains(arg.as_str()) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::GhFlagNotAllowed,
                    message: format!("gh flag `{arg}` is not allowed"),
                });
            }
        }

        Ok(Self {
            args: args.to_vec(),
        })
    }

    /// Execute the validated gh command, returning stdout, stderr, and exit code.
    pub fn run(&self) -> Result<GitOutput> {
        let output = Command::new("gh")
            .args(&self.args)
            .output()
            .map_err(|e| Error::Io {
                context: "spawn gh",
                source: e,
            })?;

        Ok(GitOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(1),
        })
    }

    /// Return the full argument list (for display / logging).
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Resolve the current branch name from a repository working tree.
fn resolve_current_branch(repo_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| Error::Io {
            context: "resolve current branch",
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!("failed to resolve current branch: {}", stderr.trim()),
        });
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if branch.is_empty() || branch == "HEAD" {
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: "current branch name is empty (detached HEAD?)".to_owned(),
        });
    }

    Ok(branch)
}

/// First non-flag positional argument, skipping common `gh` inherited options that take values.
fn first_positional_after(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return args.get(i + 1).map(String::as_str);
        }
        if a.starts_with('-') {
            // Only documented parent flags; anything else fails closed.
            if a == "--help" || a == "-h" {
                i += 1;
                continue;
            }
            if a == "-R" || a == "--repo" {
                i += 2;
                continue;
            }
            if a.starts_with("--repo=") {
                i += 1;
                continue;
            }
            return None;
        }
        return Some(a);
    }
    None
}

/// Whether a validated `gh` command mutates local checkout/worktree state.
#[must_use]
pub fn gh_requires_branch_check(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("pr") {
        return false;
    }
    let Some(pr_sub) = first_positional_after(&args[1..]) else {
        return false;
    };
    matches!(
        pr_sub,
        "checkout" | "create" | "close" | "reopen" | "edit" | "ready" | "merge" | "review"
    )
}

fn reject_external_write_targets(subcommand: &str, args: &[String]) -> Result<()> {
    match subcommand {
        "clone" => {
            reject_external_path(clone_destination(args), "git clone destination")?;
            // Also reject absolute --separate-git-dir paths and command-valued -c/--config.
            let mut i = 0;
            while i < args.len() {
                let a = args[i].as_str();
                if a == "--separate-git-dir" {
                    if let Some(path) = args.get(i + 1) {
                        reject_external_path(Some(path.as_str()), "git clone --separate-git-dir")?;
                    }
                    i += 2;
                    continue;
                }
                if let Some(path) = a.strip_prefix("--separate-git-dir=") {
                    reject_external_path(Some(path), "git clone --separate-git-dir")?;
                }
                if a == "-c" || a == "--config" {
                    if let Some(kv) = args.get(i + 1) {
                        reject_command_valued_config_assignment(kv)?;
                    }
                    i += 2;
                    continue;
                }
                if let Some(kv) = a.strip_prefix("--config=") {
                    reject_command_valued_config_assignment(kv)?;
                }
                // Attached rare form not used by git for -c; skip.
                i += 1;
            }
        }
        "config" => {
            if args.iter().any(|a| a == "--global" || a == "--system") {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::PathNotAllowed,
                    message: "git config --global/--system is not allowed under hive policy"
                        .to_owned(),
                });
            }
            if let Some(key) = config_key_name(args)
                && is_command_launching_config_key(key)
            {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::SubcommandNotAllowed,
                    message: format!(
                        "git config key `{key}` can launch external commands and is not allowed"
                    ),
                });
            }
            let mut i = 0;
            while i < args.len() {
                let a = args[i].as_str();
                if a == "-f" || a == "--file" {
                    if let Some(path) = args.get(i + 1) {
                        reject_external_path(Some(path.as_str()), "git config file")?;
                    }
                    i += 2;
                    continue;
                }
                // Attached short form: `-f/tmp/cfg` or `-f./rel`.
                if let Some(path) = a.strip_prefix("-f")
                    && !path.is_empty()
                    && !path.starts_with('-')
                {
                    reject_external_path(Some(path), "git config file")?;
                }
                if let Some(path) = a.strip_prefix("--file=") {
                    reject_external_path(Some(path), "git config file")?;
                }
                i += 1;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `git clone` options that consume a following value (must not be treated as positionals).
const CLONE_VALUE_OPTS: &[&str] = &[
    "-b",
    "--branch",
    "-c",
    "--config",
    "-o",
    "--origin",
    "-u",
    "--upload-pack",
    "--reference",
    "--reference-if-able",
    "--separate-git-dir",
    "--template",
    "--depth",
    "--shallow-since",
    "--shallow-exclude",
    "--jobs",
    "-j",
    "--filter",
    "--recurse-submodules",
    "--server-option",
    "--bundle-uri",
    "--revision",
];

/// Destination directory of `git clone` after skipping option values, if present.
fn clone_destination(args: &[String]) -> Option<&str> {
    let mut i = 0;
    let mut positionals: Vec<&str> = Vec::new();
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            positionals.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        if a.starts_with('-') {
            if a.starts_with("--") && a.contains('=') {
                i += 1;
                continue;
            }
            // Attached short form like `-bmain` is uncommon for clone; skip whole token.
            if CLONE_VALUE_OPTS.contains(&a) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        positionals.push(a);
        i += 1;
    }
    // positionals: <repo> [<dir>]
    if positionals.len() >= 2 {
        Some(positionals[1])
    } else {
        None
    }
}

/// Destination directory of `gh repo clone <repository> [<directory>]`, if present.
fn gh_repo_clone_destination(args: &[String]) -> Option<&str> {
    // args begin after top-level `repo` (caller passes &args[1..]).
    let mut i = 0;
    // Find `clone` token (may be preceded by global flags already stripped).
    while i < args.len() {
        let a = args[i].as_str();
        if a == "clone" {
            i += 1;
            break;
        }
        if a.starts_with('-') {
            if a == "-R" || a == "--repo" {
                i += 2;
                continue;
            }
            if a.starts_with("--repo=") || a == "--help" || a == "-h" {
                i += 1;
                continue;
            }
            // Unknown flag before clone: stop (fail closed for dest detection).
            return None;
        }
        // Unexpected positional before clone.
        return None;
    }
    let mut positionals: Vec<&str> = Vec::new();
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            positionals.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        if a.starts_with('-') {
            // Skip known value-taking options for gh repo clone.
            if matches!(a, "-u" | "--upstream-remote-name" | "--") {
                i += 2;
                continue;
            }
            if a.starts_with("--") && a.contains('=') {
                i += 1;
                continue;
            }
            i += 1;
            continue;
        }
        positionals.push(a);
        i += 1;
    }
    // positionals: <repository> [<directory>]
    if positionals.len() >= 2 {
        Some(positionals[1])
    } else {
        None
    }
}

fn path_is_external(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    // Unix absolute, parent traversal, Windows drive-letter, root-relative, UNC.
    path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("..")
        || path.starts_with("//")
        || path.starts_with("\\\\")
        || (path.len() > 2 && path.as_bytes().get(1) == Some(&b':'))
}

fn reject_external_path(path: Option<&str>, label: &str) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    if path_is_external(path) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: format!("`{label}` `{path}` must be a relative path under the worktree"),
        });
    }
    Ok(())
}

/// First positional target of `checkout`/`switch` (branch/ref name).
#[must_use]
pub fn checkout_or_switch_target(args: &[String]) -> Option<&str> {
    if args.is_empty() {
        return None;
    }
    let sub = args[0].as_str();
    if sub != "checkout" && sub != "switch" {
        return None;
    }
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return args.get(i + 1).map(String::as_str);
        }
        if a.starts_with('-') {
            // Equals form: --create=main, --force-create=main, -c=main (rare)
            if let Some(v) = a.strip_prefix("--create=") {
                return Some(v);
            }
            if let Some(v) = a.strip_prefix("--force-create=") {
                return Some(v);
            }
            if let Some(v) = a.strip_prefix("--orphan=") {
                return Some(v);
            }
            if matches!(
                a,
                "-b" | "-B"
                    | "-c"
                    | "-C"
                    | "--create"
                    | "--force-create"
                    | "--orphan"
                    | "--track"
                    | "-t"
            ) {
                return args.get(i + 1).map(String::as_str);
            }
            if a.starts_with("--") && a.contains('=') {
                i += 1;
                continue;
            }
            i += 1;
            continue;
        }
        return Some(a);
    }
    None
}

/// Whether `git pull` uses a non-merge history strategy.
///
/// Accepts bare `--rebase`, `--rebase=true|merges|interactive`, or `--ff-only`.
/// Rejects `--rebase=false` and `--no-rebase` (merge-style pulls).
fn pull_uses_safe_history_strategy(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--ff-only") {
        return true;
    }
    if args.iter().any(|a| a == "--no-rebase") {
        return false;
    }
    for a in args {
        if a == "--rebase" {
            return true;
        }
        if let Some(v) = a.strip_prefix("--rebase=") {
            return matches!(v, "true" | "merges" | "interactive");
        }
    }
    false
}

fn is_rebase_exec_option(arg: &str) -> bool {
    arg == "--exec"
        || arg == "-x"
        || arg.starts_with("--exec=")
        // Attached short form: `-xtrue` / `-x"cmd"`
        || (arg.starts_with("-x") && arg.len() > 2 && !arg.starts_with("--"))
}

fn is_remote_helper_exec_option(arg: &str, subcommand: &str) -> bool {
    if matches!(arg, "--receive-pack" | "--upload-pack" | "--exec")
        || arg.starts_with("--receive-pack=")
        || arg.starts_with("--upload-pack=")
        || arg.starts_with("--exec=")
    {
        return true;
    }
    // `git clone -u <upload-pack>` is the short form of --upload-pack.
    // (`git push -u` is --set-upstream and must NOT match.)
    if subcommand == "clone"
        && (arg == "-u" || (arg.starts_with("-u") && arg.len() > 2 && !arg.starts_with("--")))
    {
        return true;
    }
    false
}

fn is_ext_transport_url(arg: &str) -> bool {
    let a = arg.trim();
    a.starts_with("ext::") || a.contains("ext::") || a.to_ascii_lowercase().starts_with("ext::")
}

fn is_push_delete_flag(arg: &str) -> bool {
    if arg == "--delete" || arg == "-d" {
        return true;
    }
    // Combined short clusters containing `d` (e.g. `-ud`) on push.
    arg.starts_with('-')
        && !arg.starts_with("--")
        && arg.len() > 2
        && arg.chars().skip(1).all(|c| c.is_ascii_alphanumeric())
        && arg.chars().skip(1).any(|c| c == 'd')
}

fn is_delete_refspec(arg: &str) -> bool {
    // Empty-source refspecs delete the destination: `:branch` or `+:branch`.
    let s = arg.strip_prefix('+').unwrap_or(arg);
    s.starts_with(':') && s.len() > 1
}

fn is_branch_rename_or_copy_flag(arg: &str) -> bool {
    matches!(arg, "-m" | "-M" | "--move" | "-c" | "-C" | "--copy")
}

fn is_detach_flag(subcommand: &str, arg: &str) -> bool {
    if arg == "--detach" || arg.starts_with("--detach=") {
        return true;
    }
    // `git switch -d` is --detach; do not treat bare `-d` on checkout (unused/rare).
    subcommand == "switch"
        && (arg == "-d" || (arg.starts_with("-d") && arg.len() > 2 && !arg.starts_with("--")))
}

/// First config key name in `git config` args (after flags), if present.
fn config_key_name(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return args.get(i + 1).map(String::as_str);
        }
        if a.starts_with('-') {
            if matches!(
                a,
                "-f" | "--file"
                    | "--get"
                    | "--get-all"
                    | "--get-regexp"
                    | "--unset"
                    | "--unset-all"
                    | "--replace-all"
                    | "--add"
                    | "--name-only"
                    | "-l"
                    | "--list"
                    | "-e"
                    | "--edit"
                    | "--bool"
                    | "--int"
                    | "--bool-or-int"
                    | "--path"
                    | "--type"
                    | "--default"
                    | "--show-origin"
                    | "--show-scope"
                    | "--local"
                    | "--worktree"
                    | "--global"
                    | "--system"
                    | "-z"
                    | "--null"
                    | "-h"
                    | "--help"
            ) {
                // value-taking flags
                if matches!(a, "-f" | "--file" | "--type" | "--default") {
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if let Some(rest) = a.strip_prefix("-f")
                && !rest.is_empty()
                && !rest.starts_with('-')
            {
                i += 1;
                continue;
            }
            if a.starts_with("--file=") || a.starts_with("--type=") || a.starts_with("--default=") {
                i += 1;
                continue;
            }
            // Unknown flag: skip token only.
            i += 1;
            continue;
        }
        return Some(a);
    }
    None
}

fn is_command_launching_config_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    if k.starts_with("alias.") || k.starts_with("filter.") || k.contains(".cmd") {
        return true;
    }
    // credential.helper and URL-scoped credential.<url>.helper
    if k == "credential.helper" || (k.starts_with("credential.") && k.ends_with(".helper")) {
        return true;
    }
    // Enables git-remote-ext command transports.
    if k == "protocol.ext.allow" || k.starts_with("protocol.ext.") {
        return true;
    }
    matches!(
        k.as_str(),
        "core.sshcommand"
            | "core.editor"
            | "core.pager"
            | "core.askpass"
            | "core.fsmonitor"
            | "core.hookspath"
            | "sequence.editor"
            | "gpg.program"
            | "diff.external"
            | "diff.tool"
            | "merge.tool"
            | "uploadpack.packobjectshook"
            | "trace2.eventtarget"
            | "trace2.normaltarget"
            | "trace2.perftarget"
    )
}

/// Reject `key=value` (or bare key) config assignments that launch commands.
fn reject_command_valued_config_assignment(kv: &str) -> Result<()> {
    let key = kv.split_once('=').map(|(k, _)| k).unwrap_or(kv).trim();
    if key.is_empty() {
        return Ok(());
    }
    if is_command_launching_config_key(key) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::SubcommandNotAllowed,
            message: format!(
                "git config key `{key}` can launch external commands and is not allowed"
            ),
        });
    }
    Ok(())
}

/// Extract `-R` / `--repo` / `--repo=` selector from a `gh` argv (including `pr` etc.).
#[must_use]
pub fn gh_repo_selector(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            break;
        }
        if a == "-R" || a == "--repo" {
            return args.get(i + 1).map(String::as_str);
        }
        if let Some(v) = a.strip_prefix("--repo=") {
            return Some(v);
        }
        i += 1;
    }
    None
}

/// Normalize a GitHub repo selector or remote URL to `(host, owner/repo)` (lowercase).
///
/// Host defaults to `github.com` when the selector is bare `owner/repo`.
#[must_use]
pub fn normalize_github_repo_identity(spec: &str) -> Option<(String, String)> {
    let mut s = spec.trim().trim_end_matches('/').to_owned();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix(".git") {
        s = rest.to_owned();
    }
    let mut host = String::from("github.com");
    // scp-like: git@host:owner/repo
    if let Some(at) = s.find('@')
        && let Some(rel) = s[at..].find(':')
    {
        let colon = at + rel;
        let host_part = &s[at + 1..colon];
        let after = &s[colon + 1..];
        if after.contains('/') && !after.contains("://") {
            if !host_part.is_empty() {
                host = host_part.to_ascii_lowercase();
            }
            s = after.to_owned();
        }
    }
    for prefix in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_owned();
            break;
        }
    }
    // Drop userinfo in URL path form user@host/owner/repo
    if let Some(at) = s.find('@')
        && !s[..at].contains('/')
    {
        s = s[at + 1..].to_owned();
    }
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    let (owner, repo) = match parts.as_slice() {
        [owner, repo] => (*owner, *repo),
        [h, owner, repo] => {
            // host/owner/repo or HOST/OWNER/REPO from -R
            if h.contains('.') || *h == "github.com" || h.contains(':') {
                host = h.split(':').next().unwrap_or(h).to_ascii_lowercase();
            }
            (*owner, *repo)
        }
        [h, _extra, owner, repo] => {
            host = h.split(':').next().unwrap_or(h).to_ascii_lowercase();
            (*owner, *repo)
        }
        _ => return None,
    };
    let owner = owner.split(':').next().unwrap_or(owner);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((
        host,
        format!(
            "{}/{}",
            owner.to_ascii_lowercase(),
            repo.to_ascii_lowercase()
        ),
    ))
}

/// Normalize to `owner/repo` (lowercase), dropping host. Prefer
/// [`normalize_github_repo_identity`] when host must be compared.
#[must_use]
pub fn normalize_github_repo_slug(spec: &str) -> Option<String> {
    normalize_github_repo_identity(spec).map(|(_, slug)| slug)
}

/// Whether two GitHub repo selectors refer to the same host + owner/repo.
#[must_use]
pub fn github_repo_slugs_match(a: &str, b: &str) -> bool {
    match (
        normalize_github_repo_identity(a),
        normalize_github_repo_identity(b),
    ) {
        (Some((ha, sa)), Some((hb, sb))) => ha == hb && sa == sb,
        _ => false,
    }
}

/// Reject `git push` refspecs that update a remote branch other than `expected`.
///
/// Called from the supervisor when `--expected-branch` is known. Bare
/// `git push` / `git push origin` (no refspec) is allowed because the pre-spawn
/// branch check already ensures HEAD is on `expected`.
pub fn reject_push_outside_expected_branch(args: &[String], expected: &str) -> Result<()> {
    if args.first().map(String::as_str) != Some("push") {
        return Ok(());
    }
    if args.iter().any(|a| a == "--all") {
        return Err(Error::PolicyViolation {
            code: PolicyCode::BranchMismatch,
            message: "git push --all is not allowed under hive policy".to_owned(),
        });
    }
    for dest in push_destination_names(&args[1..]) {
        if !push_dest_matches_expected(&dest, expected) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                message: format!(
                    "git push destination `{dest}` must match --expected-branch `{expected}`"
                ),
            });
        }
    }
    Ok(())
}

fn push_dest_matches_expected(dest: &str, expected: &str) -> bool {
    let d = dest.trim();
    if d == "HEAD" {
        return true;
    }
    let leaf = d
        .strip_prefix("refs/heads/")
        .or_else(|| d.strip_prefix("refs/remotes/origin/"))
        .unwrap_or(d);
    leaf == expected
}

/// Destination branch/ref names from push argv (after the `push` token).
fn push_destination_names(args: &[String]) -> Vec<String> {
    let mut i = 0;
    let mut positionals: Vec<&str> = Vec::new();
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            positionals.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        if a.starts_with('-') {
            // value-taking push options
            if matches!(
                a,
                "--repo" | "--receive-pack" | "--exec" | "--push-option" | "-o" | "--signed"
            ) {
                i += 2;
                continue;
            }
            if a.starts_with("--repo=")
                || a.starts_with("--receive-pack=")
                || a.starts_with("--exec=")
                || a.starts_with("--push-option=")
                || a.starts_with("--signed=")
            {
                i += 1;
                continue;
            }
            i += 1;
            continue;
        }
        positionals.push(a);
        i += 1;
    }
    // positionals: [repository] [refspec ...]
    if positionals.is_empty() {
        return Vec::new();
    }
    // First positional is usually the remote; remaining are refspecs.
    // If only one positional and it contains ':', it is a refspec (no remote).
    let refspecs: &[&str] = if positionals.len() == 1 && positionals[0].contains(':') {
        &positionals[..]
    } else if positionals.len() == 1 {
        // remote only, or single bare branch name without remote — treat as branch dest
        // `git push origin` has no dest; `git push main` is unusual (remote named main).
        // Prefer no dest constraint for single non-refspec token.
        return Vec::new();
    } else {
        &positionals[1..]
    };
    let mut dests = Vec::new();
    for rs in refspecs {
        let rs = rs.strip_prefix('+').unwrap_or(rs);
        if let Some((_src, dst)) = rs.split_once(':') {
            if !dst.is_empty() {
                dests.push(dst.to_owned());
            }
        } else if !rs.is_empty() {
            // bare ref name: src and dest share the name
            dests.push((*rs).to_owned());
        }
    }
    dests
}

/// Resolve `origin` remote URL for a repo and return `owner/repo` if parseable.
pub fn origin_github_slug(repo_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| Error::Io {
            context: "resolve origin remote",
            source: e,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!("failed to resolve origin remote: {}", stderr.trim()),
        });
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    normalize_github_repo_slug(&url).ok_or_else(|| Error::PolicyViolation {
        code: PolicyCode::GitDirUnavailable,
        message: format!("could not parse origin remote as GitHub owner/repo: {url}"),
    })
}

fn is_merge_subcommand(subcommand: &str) -> bool {
    matches!(subcommand, "merge" | "mergetool")
}

/// True for bare force flags that are never allowed.
///
/// `--force-with-lease` and `--force-with-lease=<ref>` are allowed and must not match.
/// Combined short clusters (`-fu`) are handled separately for `push` only so that
/// `git clean -fd` / `git rm -f` are not false-positives.
fn is_bare_force_flag(arg: &str) -> bool {
    if arg == "-f" || arg == "--force" {
        return true;
    }
    // Reject `--force=...` but not `--force-with-lease` / `--force-with-lease=...`.
    arg.starts_with("--force=")
}

/// Combined short options containing `f` (e.g. `-fu`, `-uf`) used with `git push`.
fn is_combined_short_force_cluster(arg: &str) -> bool {
    arg.starts_with('-')
        && !arg.starts_with("--")
        && arg.len() > 2
        && arg.chars().skip(1).all(|c| c.is_ascii_alphanumeric())
        && arg.chars().skip(1).any(|c| c == 'f')
}

fn is_force_refspec(arg: &str) -> bool {
    arg.starts_with('+') && arg.len() > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- git allowlist tests ----

    #[test]
    fn allowed_subcommand_passes() {
        let cmd = SafeGitCommand::new(&["status".to_owned()]).unwrap();
        assert_eq!(cmd.subcommand(), "status");
    }

    #[test]
    fn push_passes() {
        let cmd = SafeGitCommand::new(&["push".to_owned()]).unwrap();
        assert_eq!(cmd.subcommand(), "push");
    }

    #[test]
    fn unknown_subcommand_rejected() {
        let err = SafeGitCommand::new(&["gc".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn empty_args_rejected() {
        let err = SafeGitCommand::new(&[]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    // ---- merge tests ----

    #[test]
    fn merge_subcommand_rejected() {
        let err = SafeGitCommand::new(&["merge".to_owned(), "feature".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn mergetool_subcommand_rejected() {
        let err = SafeGitCommand::new(&["mergetool".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn checkout_branch_named_merge_allowed() {
        // Merge detection is subcommand-only; a branch named "merge" is fine.
        let cmd = SafeGitCommand::new(&["checkout".to_owned(), "merge".to_owned()]).unwrap();
        assert_eq!(cmd.subcommand(), "checkout");
        assert_eq!(cmd.args(), &["checkout", "merge"]);
    }

    // ---- force push tests ----

    #[test]
    fn bare_force_rejected() {
        let err = SafeGitCommand::new(&["push".to_owned(), "--force".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn bare_f_flag_rejected() {
        let err = SafeGitCommand::new(&["push".to_owned(), "-f".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn force_with_lease_accepted() {
        let cmd =
            SafeGitCommand::new(&["push".to_owned(), "--force-with-lease".to_owned()]).unwrap();
        assert_eq!(cmd.subcommand(), "push");
    }

    #[test]
    fn force_with_lease_and_bare_force_rejected() {
        // Bare --force is always rejected, even when --force-with-lease is also present.
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "--force".to_owned(),
            "--force-with-lease".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn force_equals_form_rejected() {
        let err = SafeGitCommand::new(&["push".to_owned(), "--force=true".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn push_mirror_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "--mirror".to_owned(),
            "origin".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn pull_without_rebase_or_ff_only_rejected() {
        let err = SafeGitCommand::new(&["pull".to_owned(), "origin".to_owned(), "main".to_owned()])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn pull_with_rebase_allowed() {
        SafeGitCommand::new(&[
            "pull".to_owned(),
            "--rebase".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ])
        .unwrap();
    }

    #[test]
    fn rebase_exec_rejected() {
        let err = SafeGitCommand::new(&[
            "rebase".to_owned(),
            "-x".to_owned(),
            "true".to_owned(),
            "main".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn clone_absolute_dest_rejected() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "https://example.com/r.git".to_owned(),
            "/tmp/outside".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn config_global_rejected() {
        let err = SafeGitCommand::new(&[
            "config".to_owned(),
            "--global".to_owned(),
            "user.name".to_owned(),
            "x".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn push_combined_short_force_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "-fu".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn clone_with_branch_opt_still_rejects_abs_dest() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "-b".to_owned(),
            "main".to_owned(),
            "https://example.com/r.git".to_owned(),
            "/tmp/outside".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn config_attached_short_file_rejected() {
        let err = SafeGitCommand::new(&[
            "config".to_owned(),
            "-f/tmp/outside".to_owned(),
            "user.name".to_owned(),
            "x".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn switch_create_equals_form_target() {
        let args = vec!["switch".to_owned(), "--create=main2".to_owned()];
        assert_eq!(checkout_or_switch_target(&args), Some("main2"));
    }

    #[test]
    fn gh_pr_update_branch_rejected() {
        let err =
            SafeGhCommand::new(&["pr".to_owned(), "update-branch".to_owned(), "1".to_owned()])
                .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn gh_repo_clone_abs_dest_rejected() {
        let err = SafeGhCommand::new(&[
            "repo".to_owned(),
            "clone".to_owned(),
            "cli/cli".to_owned(),
            "/tmp/outside".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn gh_pr_checkout_rejected() {
        let err = SafeGhCommand::new(&["pr".to_owned(), "checkout".to_owned(), "1".to_owned()])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn config_url_scoped_credential_helper_rejected() {
        let err = SafeGitCommand::new(&[
            "config".to_owned(),
            "credential.https://github.com.helper".to_owned(),
            "!true".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn push_prune_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "--prune".to_owned(),
            "origin".to_owned(),
            "refs/heads/*:refs/heads/*".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn clone_c_ssh_command_rejected() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "-c".to_owned(),
            "core.sshCommand=sh -c true".to_owned(),
            "https://example.com/r.git".to_owned(),
            "dst".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn windows_unc_clone_dest_rejected() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "https://example.com/r.git".to_owned(),
            r"\\server\share\out".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn pull_rebase_false_rejected() {
        let err = SafeGitCommand::new(&[
            "pull".to_owned(),
            "--rebase=false".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn pull_rebase_true_allowed() {
        SafeGitCommand::new(&[
            "pull".to_owned(),
            "--rebase=true".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ])
        .unwrap();
    }

    #[test]
    fn rebase_attached_exec_rejected() {
        let err =
            SafeGitCommand::new(&["rebase".to_owned(), "-xtrue".to_owned(), "main".to_owned()])
                .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn clone_template_opt_still_rejects_abs_dest() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "--template".to_owned(),
            "/tmp/t".to_owned(),
            "https://example.com/r.git".to_owned(),
            "/tmp/outside".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::PathNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn config_ssh_command_rejected() {
        let err = SafeGitCommand::new(&[
            "config".to_owned(),
            "core.sshCommand".to_owned(),
            "sh -c true".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn push_delete_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "--delete".to_owned(),
            "origin".to_owned(),
            "main".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn push_delete_refspec_rejected() {
        let err =
            SafeGitCommand::new(&["push".to_owned(), "origin".to_owned(), ":main".to_owned()])
                .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn push_receive_pack_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "--receive-pack=sh".to_owned(),
            "origin".to_owned(),
            "HEAD".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn checkout_detach_rejected() {
        let err = SafeGitCommand::new(&[
            "checkout".to_owned(),
            "--detach".to_owned(),
            "feature".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }
        ));
    }

    #[test]
    fn branch_rename_rejected() {
        let err = SafeGitCommand::new(&["branch".to_owned(), "-m".to_owned(), "main".to_owned()])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }
        ));
    }

    #[test]
    fn gh_repo_selector_extracts_r_flag() {
        let args = vec![
            "pr".to_owned(),
            "-R".to_owned(),
            "other/repo".to_owned(),
            "close".to_owned(),
            "1".to_owned(),
        ];
        assert_eq!(gh_repo_selector(&args), Some("other/repo"));
    }

    #[test]
    fn github_slug_normalize_and_match() {
        assert_eq!(
            normalize_github_repo_slug("https://github.com/Acme/Repo.git").as_deref(),
            Some("acme/repo")
        );
        assert_eq!(
            normalize_github_repo_slug("git@github.com:Acme/Repo.git").as_deref(),
            Some("acme/repo")
        );
        assert!(github_repo_slugs_match("Acme/Repo", "github.com/acme/repo"));
        assert!(!github_repo_slugs_match("Acme/Repo", "other/repo"));
        // Host is part of identity: enterprise origin must not match github.com -R.
        assert!(!github_repo_slugs_match(
            "git@github.enterprise:acme/repo.git",
            "github.com/acme/repo"
        ));
        assert!(github_repo_slugs_match(
            "git@github.enterprise:acme/repo.git",
            "github.enterprise/acme/repo"
        ));
    }

    #[test]
    fn push_refspec_to_other_branch_rejected() {
        let err = reject_push_outside_expected_branch(
            &[
                "push".to_owned(),
                "origin".to_owned(),
                "HEAD:main".to_owned(),
            ],
            "feature",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }
        ));
    }

    #[test]
    fn push_to_expected_branch_ok() {
        reject_push_outside_expected_branch(
            &[
                "push".to_owned(),
                "origin".to_owned(),
                "HEAD:feature".to_owned(),
            ],
            "feature",
        )
        .unwrap();
    }

    #[test]
    fn clone_u_upload_pack_rejected() {
        let err = SafeGitCommand::new(&[
            "clone".to_owned(),
            "-u".to_owned(),
            "sh -c true".to_owned(),
            "https://example.com/r.git".to_owned(),
            "dst".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn ext_transport_url_rejected() {
        let err = SafeGitCommand::new(&[
            "ls-remote".to_owned(),
            "ext::gh pr merge 1 --merge".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::SubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn force_refspec_rejected() {
        let err = SafeGitCommand::new(&[
            "push".to_owned(),
            "origin".to_owned(),
            "+HEAD:main".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    // ---- mutating subcommand detection ----

    #[test]
    fn push_is_mutating() {
        let cmd = SafeGitCommand::new(&["push".to_owned()]).unwrap();
        assert!(cmd.requires_branch_check());
    }

    #[test]
    fn commit_is_mutating() {
        let cmd =
            SafeGitCommand::new(&["commit".to_owned(), "-m".to_owned(), "msg".to_owned()]).unwrap();
        assert!(cmd.requires_branch_check());
    }

    #[test]
    fn add_is_mutating() {
        let cmd = SafeGitCommand::new(&["add".to_owned(), ".".to_owned()]).unwrap();
        assert!(cmd.requires_branch_check());
    }

    #[test]
    fn clean_is_mutating() {
        let cmd = SafeGitCommand::new(&["clean".to_owned(), "-fd".to_owned()]).unwrap();
        assert!(cmd.requires_branch_check());
    }

    #[test]
    fn status_is_not_mutating() {
        let cmd = SafeGitCommand::new(&["status".to_owned()]).unwrap();
        assert!(!cmd.requires_branch_check());
    }

    #[test]
    fn diff_is_not_mutating() {
        let cmd = SafeGitCommand::new(&["diff".to_owned()]).unwrap();
        assert!(!cmd.requires_branch_check());
    }

    // ---- gh allowlist tests ----

    #[test]
    fn gh_pr_create_allowed() {
        let cmd = SafeGhCommand::new(&[
            "pr".to_owned(),
            "create".to_owned(),
            "--title".to_owned(),
            "test".to_owned(),
        ])
        .unwrap();
        assert_eq!(cmd.args(), &["pr", "create", "--title", "test"]);
    }

    #[test]
    fn gh_pr_merge_after_repo_flag_rejected() {
        let err = SafeGhCommand::new(&[
            "pr".to_owned(),
            "-R".to_owned(),
            "acme/widgets".to_owned(),
            "merge".to_owned(),
            "1".to_owned(),
            "-m".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn gh_pr_merge_rejected() {
        let err = SafeGhCommand::new(&["pr".to_owned(), "merge".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn gh_pr_ready_rejected() {
        let err = SafeGhCommand::new(&["pr".to_owned(), "ready".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[test]
    fn gh_merge_flag_rejected() {
        let err = SafeGhCommand::new(&["pr".to_owned(), "create".to_owned(), "--merge".to_owned()])
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::GhFlagNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn gh_api_rejected() {
        // api removed from allowlist to block merge via REST/GraphQL.
        let err = SafeGhCommand::new(&[
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            "query=mutation { mergePullRequest }".to_owned(),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn gh_unknown_subcommand_rejected() {
        let err = SafeGhCommand::new(&["codespace".to_owned()]).unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                ..
            }
        ));
    }

    #[test]
    fn gh_issue_list_allowed() {
        let cmd = SafeGhCommand::new(&["issue".to_owned(), "list".to_owned()]).unwrap();
        assert_eq!(cmd.args(), &["issue", "list"]);
    }

    // ---- error display tests ----

    #[test]
    fn policy_code_display() {
        assert_eq!(PolicyCode::BareForcePush.as_str(), "BARE_FORCE_PUSH");
        assert_eq!(PolicyCode::MergeBlocked.as_str(), "MERGE_BLOCKED");
        assert_eq!(
            PolicyCode::SubcommandNotAllowed.as_str(),
            "SUBCOMMAND_NOT_ALLOWED"
        );
        assert_eq!(PolicyCode::BranchMismatch.as_str(), "BRANCH_MISMATCH");
        assert_eq!(
            PolicyCode::GhSubcommandNotAllowed.as_str(),
            "GH_SUBCOMMAND_NOT_ALLOWED"
        );
        assert_eq!(PolicyCode::GhFlagNotAllowed.as_str(), "GH_FLAG_NOT_ALLOWED");
    }

    #[test]
    fn error_display_includes_code_and_message() {
        let err = Error::PolicyViolation {
            code: PolicyCode::BareForcePush,
            message: "test message".to_owned(),
        };
        let display = format!("{err}");
        assert!(display.contains("BARE_FORCE_PUSH"));
        assert!(display.contains("test message"));
    }

    /// Isolated git repo with a named branch for execution tests.
    ///
    /// Avoids depending on the workspace checkout (tarpaulin / detached HEAD).
    fn temp_repo_with_branch(branch: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "writ-core-git-safe-{}-{}-{}",
            std::process::id(),
            seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(&dir).expect("create temp repo dir");

        let null_dev = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", null_dev)
                .output()
                .expect("spawn git");
            assert!(
                output.status.success(),
                "git {args:?} failed in {}: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        };

        // Portable across Git versions / Windows template races.
        git(&["init"]);
        git(&["checkout", "-b", branch]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "writ-core-test"]);
        std::fs::write(
            dir.join("README"),
            "init
",
        )
        .expect("write README");
        git(&["add", "README"]);
        git(&["commit", "-m", "init"]);
        dir
    }

    #[test]
    fn run_status_in_repo_executes() {
        let repo = temp_repo_with_branch("main");
        let cmd =
            SafeGitCommand::new(&["rev-parse".to_owned(), "--is-inside-work-tree".to_owned()])
                .unwrap();
        let out = cmd.run(&repo, None).expect("git should run in temp repo");
        assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);
        assert_eq!(out.stdout.trim(), "true");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn run_verifies_expected_branch_for_mutating() {
        let repo = temp_repo_with_branch("job-branch");
        let current = resolve_current_branch(&repo).expect("resolve branch");
        assert_eq!(current, "job-branch");

        // status is not mutating — expected_branch is ignored.
        let cmd = SafeGitCommand::new(&["status".to_owned(), "--porcelain".to_owned()]).unwrap();
        let out = cmd
            .run(&repo, Some("definitely-not-this-branch"))
            .expect("status should run");
        assert_eq!(out.exit_code, 0, "stderr={}", out.stderr);

        // Mutating with wrong branch is rejected before spawn.
        let push = SafeGitCommand::new(&["push".to_owned(), "--dry-run".to_owned()]).unwrap();
        let err = push
            .run(&repo, Some("definitely-not-this-branch"))
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }
        ));

        // Matching branch passes branch verification (push may still fail without a remote).
        let push_ok = SafeGitCommand::new(&["push".to_owned(), "--dry-run".to_owned()]).unwrap();
        match push_ok.run(&repo, Some("job-branch")) {
            Ok(_) => {}
            Err(Error::PolicyViolation {
                code: PolicyCode::BranchMismatch,
                ..
            }) => panic!("matching branch must not fail branch verification"),
            Err(_) => {} // e.g. no remote configured
        }

        let _ = std::fs::remove_dir_all(&repo);
    }
}
