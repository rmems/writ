//! Allowlisted git and GitHub CLI operations.
//!
//! Safety invariants enforced at the Rust core boundary:
//! - Only allowlisted git subcommands may be executed.
//! - Bare `--force` / `-f` is always rejected; only `--force-with-lease` is permitted.
//! - Local `git merge` on an assigned feature branch is allowed; `git mergetool` stays blocked.
//! - Merge into `main`/`master`, or with uncommitted work, is refused so WIP is preserved.
//! - `gh pr merge` and merge-related flags are blocked; `gh api` is not allowlisted.
//! - Mutating commands verify the current branch when `expected_branch` is provided to `run`.
//! - `gh -R` / `--repo` selectors are checked against the configured owner allowlist.
//! - All policy violations carry stable structured error codes.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};

mod gh;
mod identity;
mod restricted;
#[cfg(test)]
mod tests;

pub use gh::{
    SafeGhCommand, bind_gh_repo_selector_to_origin, effective_gh_repo_selector,
    enforce_gh_repo_targets, first_positional_after, gh_repo_env_target, gh_repo_selector,
    gh_requires_branch_check, pin_gh_repo_selector,
};
pub use identity::{
    github_owner_name, github_repo_slugs_match, is_supported_github_remote,
    normalize_github_repo_identity, normalize_github_repo_slug, origin_github_repo_selector,
    origin_github_slug,
};
pub(crate) use restricted::{
    run_allowlisted_git_restricted, run_allowlisted_git_restricted_with_file,
};

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
    "merge",
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
    "merge",
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

        // Interactive mergetool can run configured helpers; keep it blocked.
        // A branch or ref named `merge` is still allowed as a non-subcommand.
        if subcommand == "mergetool" {
            return Err(Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                message: format!("mergetool is not allowed: `git {}`", args.join(" ")),
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

    /// Admit a local `git merge` or `git pull` in `repo_dir` without building a
    /// merge-permission engine.
    ///
    /// Feature-branch integration and `--abort`/`--continue`/`--quit` are allowed.
    /// Default-branch (`main`/`master`) integration and dirty-tree merges that would
    /// clobber uncommitted work are refused.
    pub fn admit_local_merge(&self, repo_dir: &Path) -> Result<()> {
        let verb = match self.subcommand() {
            "merge" => "merge",
            "pull" => "pull",
            _ => return Ok(()),
        };
        if merge_is_recovery(&self.args) {
            return Ok(());
        }
        let current = resolve_current_branch(repo_dir)?;
        if is_default_integration_branch(&current) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                message: format!(
                    "local {verb} on default branch `{current}` is not allowed; GitHub owns protected-branch integration"
                ),
            });
        }
        if working_tree_is_dirty(repo_dir)? {
            return Err(Error::PolicyViolation {
                code: PolicyCode::MergeBlocked,
                message: format!(
                    "refusing git {verb} with uncommitted work; commit, stash, or abort to preserve WIP"
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
        self.admit_local_merge(repo_dir)?;
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

pub(crate) fn is_ext_transport_url(arg: &str) -> bool {
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

fn merge_is_recovery(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--abort" || a == "--continue" || a == "--quit")
}

fn is_default_integration_branch(branch: &str) -> bool {
    matches!(branch, "main" | "master")
}

fn working_tree_is_dirty(repo_dir: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["status", "--porcelain", "-uall"])
        .output()
        .map_err(|e| Error::Io {
            context: "inspect worktree dirtiness",
            source: e,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::PolicyViolation {
            code: PolicyCode::GitDirUnavailable,
            message: format!("failed to inspect worktree: {}", stderr.trim()),
        });
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
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
