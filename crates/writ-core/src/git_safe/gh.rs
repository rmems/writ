//! Allowlisted GitHub CLI (`gh`) policy.

use std::collections::HashSet;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};
use crate::owners::OwnerAllowlist;

use super::identity::{github_repo_slugs_match, normalize_github_repo_identity};
use super::{GitOutput, reject_external_path};

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

/// Pre-validated GitHub CLI command ready for execution.
#[derive(Debug, Clone)]
pub struct SafeGhCommand {
    args: Vec<String>,
}

impl SafeGhCommand {
    /// Create a new safe gh command after validating the full argument list.
    ///
    /// Returns an error if the command violates any safety policy. Owner
    /// allowlist checks for `-R` / `--repo` use [`OwnerAllowlist::from_env`].
    pub fn new(args: &[String]) -> Result<Self> {
        Self::with_allowlist(args, &OwnerAllowlist::from_env())
    }

    /// Validate a gh command against an explicit owner allowlist.
    ///
    /// `-R` / `--repo` selectors are rejected unless their owner is allowlisted.
    /// Commands without a repo selector are not multi-owner operations and skip
    /// that check. The allowlist is enforced after argv policy so merge blocks
    /// remain the more specific rejection when both would apply, and always
    /// before [`Self::run`].
    pub fn with_allowlist(args: &[String], allowlist: &OwnerAllowlist) -> Result<Self> {
        let subcommand = gh_subcommand(args)?;
        gh_reject_blocked_pr_subsubcommand(subcommand, args)?;
        gh_reject_external_clone_destination(subcommand, args)?;
        gh_enforce_clone_target_owner(subcommand, args, allowlist)?;
        gh_reject_blocked_flags(args)?;
        enforce_gh_repo_targets(args, allowlist, gh_repo_env_selector().as_deref())?;

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

/// Validate that `args[0]` is a non-empty, allowlisted gh subcommand and return it.
fn gh_subcommand(args: &[String]) -> Result<&str> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(Error::PolicyViolation {
            code: PolicyCode::GhSubcommandNotAllowed,
            message: "no gh subcommand provided".to_owned(),
        });
    };
    let allowed: HashSet<&str> = ALLOWED_GH_SUBCOMMANDS.iter().copied().collect();
    if !allowed.contains(subcommand) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::GhSubcommandNotAllowed,
            message: format!("gh subcommand `{subcommand}` is not on the allowlist"),
        });
    }
    Ok(subcommand)
}

/// Block `gh pr merge` / `ready` / `update-branch` even when inherited flags
/// precede the subcommand, e.g. `gh pr -R owner/repo merge 1`.
fn gh_reject_blocked_pr_subsubcommand(subcommand: &str, args: &[String]) -> Result<()> {
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
    Ok(())
}

/// `gh repo clone <repo> [<dir>]` can write outside the worktree; reject an
/// external destination.
fn gh_reject_external_clone_destination(subcommand: &str, args: &[String]) -> Result<()> {
    if subcommand != "repo" {
        return Ok(());
    }
    let Some(repo_sub) = first_positional_after(&args[1..]) else {
        return Ok(());
    };
    if repo_sub != "clone" {
        return Ok(());
    }
    let Some(dest) = gh_repo_clone_destination(&args[1..]) else {
        return Ok(());
    };
    reject_external_path(Some(dest), "gh repo clone destination")
}

/// `gh repo clone <repository> [<dir>]` targets an explicit repository that no
/// `-R` selector covers; authorize its owner against the allowlist.
fn gh_enforce_clone_target_owner(
    subcommand: &str,
    args: &[String],
    allowlist: &OwnerAllowlist,
) -> Result<()> {
    if subcommand != "repo" {
        return Ok(());
    }
    let Some(repo_sub) = first_positional_after(&args[1..]) else {
        return Ok(());
    };
    if repo_sub != "clone" {
        return Ok(());
    }
    let Some(start) = gh_repo_clone_token_end(&args[1..]) else {
        return allowlist.enforce_repo_selector("");
    };
    let positionals = gh_repo_clone_positionals(&args[1..], start);
    allowlist.enforce_repo_selector(positionals.first().copied().unwrap_or(""))
}

/// Block merge-related flags anywhere in the argument list.
fn gh_reject_blocked_flags(args: &[String]) -> Result<()> {
    let blocked_flags: HashSet<&str> = BLOCKED_GH_FLAGS.iter().copied().collect();
    for arg in &args[1..] {
        if blocked_flags.contains(arg.as_str()) {
            return Err(Error::PolicyViolation {
                code: PolicyCode::GhFlagNotAllowed,
                message: format!("gh flag `{arg}` is not allowed"),
            });
        }
    }
    Ok(())
}

/// Resolve the current branch name from a repository working tree.
const GH_VALUE_TAKING_OPTIONS: &[&str] = &[
    "--template",
    "-t",
    "--json",
    "-q",
    "--jq",
    "--limit",
    "-L",
    "--search",
    "-S",
    "--state",
    "--label",
    "--assignee",
    "--author",
    "--base",
    "--head",
    "--milestone",
    "--project",
    "--body",
    "-b",
    "--body-file",
    "-F",
    "--title",
    "-T",
    "--comment",
];

/// `gh` boolean (non-value-taking) flags whose separate-token form does NOT
/// consume the following argv token. When one of these immediately precedes a
/// literal `--`, that `--` is genuinely the end-of-options terminator.
///
/// This list underpins the fail-closed arity decision in
/// [`separate_token_option_may_consume_dashdash`]: only when the preceding option
/// is *known* to be boolean can we be certain the `--` terminates options. Any
/// other separate-token option (recognized value-taking, a repo selector, or an
/// unrecognized one) is treated conservatively as possibly consuming the `--`, so
/// a later `-R other/repo` is never skipped. Keeping this list to well-known
/// flags is safe: an omission here only makes the scanner *more* conservative
/// (it keeps scanning), never less.
///
/// Only unambiguous long-form booleans are listed. Short forms are deliberately
/// excluded because a single letter is frequently overloaded across subcommands
/// (e.g. `-c` is `--comment` for `gh pr close` but `--comments` for `gh pr view`,
/// and `-d` is `--draft` for create but `--delete-branch`/other elsewhere).
/// Treating any short form as boolean here would risk failing open, so short
/// forms fall through to the conservative "may consume `--`" branch.
const GH_KNOWN_BOOLEAN_FLAGS: &[&str] = &[
    "--help",
    "--web",
    "--comments",
    "--draft",
    "--fill",
    "--fill-first",
    "--fill-verbose",
    "--no-maintainer-edit",
    "--delete-branch",
    "--dry-run",
    "--merged",
    "--closed",
];

/// Whether a separate-token option `flag` may consume the following argv token as
/// its value (fail-closed). Repo selectors that expect a value (`-R`, `--repo`,
/// clustered `-wR`) and the known value-taking options consume it; a *known*
/// boolean flag does not; and any *unrecognized* separate-token option is treated
/// conservatively as if it might, so `<unknown-opt> -- -R other/repo` never lets
/// the trailing selector slip past a scanner that stops at `--`.
fn separate_token_option_may_consume_dashdash(flag: &str) -> bool {
    if GH_VALUE_TAKING_OPTIONS.contains(&flag) {
        return true;
    }
    if matches!(gh_repo_flag(flag, Some("--")), Some((_, true))) {
        return true;
    }
    // Known boolean flag: `--` after it is a real terminator.
    if GH_KNOWN_BOOLEAN_FLAGS.contains(&flag) {
        return false;
    }
    // Unrecognized separate-token option: fail closed, assume it may take `--` as
    // its value so the scanner keeps looking for a later repo selector.
    true
}

/// Whether the token at `args[idx]` is a `--` that a preceding option consumes as
/// its value (so scanning must continue past it), rather than the end-of-options
/// terminator.
///
/// Fails closed: a `--` preceded by any separate-token option that is not a known
/// boolean flag is treated as that option's value, so a later `-R other/repo` is
/// still detected and enforced. Only a bare positional or a known boolean flag
/// immediately before `--` makes it a genuine terminator.
fn dashdash_is_option_value(args: &[String], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let prev = args[idx - 1].as_str();
    // The previous token must itself be an option in separate-token form (no
    // attached `=value`), otherwise it does not pull in this `--`.
    if !prev.starts_with('-') || prev.contains('=') {
        return false;
    }
    separate_token_option_may_consume_dashdash(prev)
}

/// One step of the `first_positional_after` argv scan.
enum PositionalScan {
    /// Advance the cursor by `usize` tokens and keep scanning.
    Skip(usize),
    /// Stop scanning; the positional (or absence thereof) is decided.
    Stop(Option<usize>),
}

/// Classify the token at `args[idx]` for [`first_positional_after`], keeping the
/// scan loop itself flat. `--` that is a preceding option's value is skipped
/// (not treated as a terminator); unknown flags fail closed by stopping.
fn classify_positional_token(args: &[String], idx: usize) -> PositionalScan {
    let a = args[idx].as_str();
    if a == "--" {
        // `--` consumed as a preceding option's value is not a terminator;
        // keep scanning so a later positional/flag is still seen.
        if dashdash_is_option_value(args, idx) {
            return PositionalScan::Skip(1);
        }
        return PositionalScan::Stop(idx.checked_add(1).filter(|&n| n < args.len()));
    }
    if !a.starts_with('-') {
        return PositionalScan::Stop(Some(idx));
    }
    // Only documented parent flags; anything else fails closed.
    if a == "--help" || a == "-h" {
        return PositionalScan::Skip(1);
    }
    if let Some((_, consume_next)) = gh_repo_flag(a, args.get(idx + 1).map(String::as_str)) {
        return PositionalScan::Skip(if consume_next { 2 } else { 1 });
    }
    // Value-taking option in separate-token form: skip it and its value so the
    // value (which may be `--`) is not mistaken for a terminator.
    if GH_VALUE_TAKING_OPTIONS.contains(&a) {
        return PositionalScan::Skip(2);
    }
    PositionalScan::Stop(None)
}

/// First non-flag positional argument, skipping common `gh` inherited options that take values.
pub fn first_positional_after(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        match classify_positional_token(args, i) {
            PositionalScan::Skip(n) => i += n,
            PositionalScan::Stop(pos) => return pos.map(|p| args[p].as_str()),
        }
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
const GH_REPO_CLONE_VALUE_OPTIONS: &[&str] = &["-u", "--upstream-remote-name"];

/// Whether the token immediately before `args[idx]` is an option that consumes
/// this token (here, a `--`) as its value for `gh repo clone` scanning.
///
/// Fails closed the same way [`dashdash_is_option_value`] does: the `gh repo
/// clone` value-taking options and repo selectors consume it, a known boolean
/// flag does not, and any unrecognized separate-token option is treated
/// conservatively as consuming it so a `--`-hidden destination or later selector
/// is not skipped.
fn idx_prev_consumes_clone_value(args: &[String], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let prev = args[idx - 1].as_str();
    if !prev.starts_with('-') || prev.contains('=') {
        return false;
    }
    if GH_REPO_CLONE_VALUE_OPTIONS.contains(&prev) {
        return true;
    }
    separate_token_option_may_consume_dashdash(prev)
}

/// Locate the index just past the `clone` token in `gh repo …` argv (caller
/// passes `&args[1..]`, i.e. after the top-level `repo`). Returns `None` if no
/// `clone` token is reached before an unknown flag or unexpected positional
/// (fail closed for destination detection).
fn gh_repo_clone_token_end(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "clone" {
            return Some(i + 1);
        }
        if !a.starts_with('-') {
            // Unexpected positional before clone.
            return None;
        }
        if let Some((_, consume_next)) = gh_repo_flag(a, args.get(i + 1).map(String::as_str)) {
            i += if consume_next { 2 } else { 1 };
            continue;
        }
        if a == "--help" || a == "-h" {
            i += 1;
            continue;
        }
        // Unknown flag before clone: stop (fail closed for dest detection).
        return None;
    }
    None
}

/// Collect the positional arguments of `gh repo clone` starting at `start`,
/// skipping value-taking options and honouring a real `--` terminator (but not a
/// `--` that is a preceding value-taking option's value).
fn gh_repo_clone_positionals(args: &[String], start: usize) -> Vec<&str> {
    let mut positionals: Vec<&str> = Vec::new();
    let mut i = start;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" && !idx_prev_consumes_clone_value(args, i) {
            positionals.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        if a == "--" {
            // `--` consumed as the value of a preceding value-taking option is
            // not the terminator; skip it and keep scanning for the destination.
            i += 1;
            continue;
        }
        if a.starts_with('-') {
            // Skip known value-taking options for gh repo clone.
            if matches!(a, "-u" | "--upstream-remote-name") {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        positionals.push(a);
        i += 1;
    }
    positionals
}

/// Destination directory of `gh repo clone <repository> [<directory>]`, if present.
fn gh_repo_clone_destination(args: &[String]) -> Option<&str> {
    // args begin after top-level `repo` (caller passes &args[1..]).
    let start = gh_repo_clone_token_end(args)?;
    let positionals = gh_repo_clone_positionals(args, start);
    // positionals: <repository> [<directory>]
    positionals.get(1).copied()
}
pub fn gh_repo_env_target() -> Option<String> {
    gh_repo_env_selector()
}

/// The repo selector `gh` will actually resolve for `args`: an explicit
/// `-R`/`--repo` if present, otherwise the implicit `GH_REPO` selector.
///
/// The explicit selector always wins, matching `gh`'s precedence. Returns `None`
/// when neither is present so the caller falls back to the working directory.
#[must_use]
pub fn effective_gh_repo_selector(args: &[String], env_selector: Option<&str>) -> Option<String> {
    if let Some(explicit) = gh_repo_selector(args) {
        return Some(explicit.to_owned());
    }
    env_selector
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Reject a mutating `gh` command whose effective repo selector (explicit
/// `-R`/`--repo` or implicit `GH_REPO`) does not match the verified local
/// `origin` slug.
///
/// This is the origin binding the supervisor applies once the branch gate is
/// satisfied. Extracting it keeps the explicit-`-R` and implicit-`GH_REPO` paths
/// identical and unit-testable without mutating process environment.
pub fn bind_gh_repo_selector_to_origin(
    args: &[String],
    env_selector: Option<&str>,
    origin_slug: &str,
) -> Result<()> {
    if let Some(selector) = effective_gh_repo_selector(args, env_selector)
        && !github_repo_slugs_match(&selector, origin_slug)
    {
        return Err(Error::PolicyViolation {
            code: PolicyCode::PathNotAllowed,
            message: format!(
                "gh repo selector `{selector}` does not match verified origin `{origin_slug}`"
            ),
        });
    }
    Ok(())
}

/// Extract the last `-R` / `--repo` selector from a `gh` argv (including `pr` etc.).
///
/// Accepts the pflag spellings `gh` 2.x actually parses: `-R value`, `-R=value`,
/// `-Rvalue`, `--repo value`, `--repo=value`, and clustered shorts whose last
/// letter is `R` (`-wR value`, `-wR=value`, `-wRvalue`). Last flag wins, matching
/// `gh`.
#[must_use]
pub fn gh_repo_selector(args: &[String]) -> Option<&str> {
    gh_repo_selectors(args).last().copied()
}

fn gh_repo_selectors(args: &[String]) -> Vec<&str> {
    let mut selectors = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match gh_selector_scan_step(args, i) {
            GhSelectorStep::Done => break,
            GhSelectorStep::Advance(next) => i = next,
            GhSelectorStep::Found { selector, next } => {
                selectors.push(selector);
                i = next;
            }
        }
    }
    selectors
}

enum GhSelectorStep<'a> {
    Done,
    Advance(usize),
    Found { selector: &'a str, next: usize },
}

fn gh_selector_scan_step(args: &[String], i: usize) -> GhSelectorStep<'_> {
    let a = args[i].as_str();
    if a == "--" {
        // A `--` that a preceding value-taking option consumes as its value
        // is not the options terminator; gh keeps parsing, so a later
        // `-R other/repo` must still be detected. Fail closed by continuing.
        if dashdash_is_option_value(args, i) {
            return GhSelectorStep::Advance(i + 1);
        }
        return GhSelectorStep::Done;
    }
    if let Some((selector, consume_next)) = gh_repo_flag(a, args.get(i + 1).map(String::as_str)) {
        return GhSelectorStep::Found {
            selector,
            next: if consume_next { i + 2 } else { i + 1 },
        };
    }
    // Skip a non-selector value-taking option together with its value so a
    // `--` value cannot terminate scanning prematurely.
    if a.starts_with('-') && !a.contains('=') && GH_VALUE_TAKING_OPTIONS.contains(&a) {
        return GhSelectorStep::Advance(i + 2);
    }
    GhSelectorStep::Advance(i + 1)
}

/// Parse one argv token as a `gh` repo selector flag.
///
/// Returns `(selector, consume_next)` when `arg` is `-R` / `--repo` in any
/// accepted spelling. A missing required value is an empty selector so callers
/// can fail closed.
fn gh_repo_flag<'a>(arg: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool)> {
    if arg == "--repo" || arg == "-R" {
        return Some((next.unwrap_or(""), true));
    }
    if let Some(value) = arg.strip_prefix("--repo=") {
        return Some((value, false));
    }
    if let Some(value) = arg.strip_prefix("-R=") {
        return Some((value, false));
    }
    if let Some(value) = arg.strip_prefix("-R")
        && !value.is_empty()
    {
        return Some((value, false));
    }
    clustered_short_repo_flag(arg, next)
}

fn clustered_short_repo_flag<'a>(arg: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool)> {
    if !arg.starts_with('-') || arg.starts_with("--") {
        return None;
    }
    let body = &arg[1..];
    let (letters, eq_value) = match body.split_once('=') {
        Some((letters, value)) => (letters, Some(value)),
        None => (body, None),
    };
    let r_idx = letters.find('R')?;
    if !letters[..r_idx].chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let after = &letters[r_idx + 1..];
    if !after.is_empty() {
        return Some((after, false));
    }
    if let Some(value) = eq_value {
        return Some((value, false));
    }
    Some((next.unwrap_or(""), true))
}

fn gh_repo_env_selector() -> Option<String> {
    std::env::var("GH_REPO")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Whether a parsed `gh` argv is a command that operates on a repository, so the
/// `GH_REPO` environment selector applies to it.
///
/// Per `gh help environment`, `GH_REPO` only affects commands that otherwise
/// operate on a local repository. Repository-independent commands such as
/// `gh auth status`, `gh ssh-key ...`, `gh gist ...`, and global `secret` /
/// `variable` / `label` forms ignore it. Applying the implicit check to those
/// wrongly rejects them (e.g. `GH_REPO=other/repo writ gh-safe auth status`).
///
/// This is deliberately conservative and fail-closed: `pr`, `issue`, and
/// non-clone `repo` subcommands are always treated as repository-context, and an
/// unrecognized subcommand is also treated as repository-context so a new
/// repo-affecting command is not silently exempted.
fn gh_command_uses_repo_context(args: &[String]) -> bool {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return false;
    };
    match subcommand {
        // Never operate on a local repository; GH_REPO is irrelevant.
        "auth" | "ssh-key" | "gist" => false,
        // `repo clone` targets an explicit repository argument, not GH_REPO; the
        // destination is validated separately. Other `repo` subcommands
        // (view/edit/...) resolve against the current/selected repository.
        "repo" => !matches!(first_positional_after(&args[1..]), Some("clone")),
        // `pr` and `issue` always resolve a repository.
        "pr" | "issue" => true,
        // These have both repo-scoped and account/global forms. Fail closed and
        // treat them as repo-context so GH_REPO is still bound rather than
        // silently ignored for a repo-scoped invocation.
        "secret" | "variable" | "label" | "release" | "workflow" | "browse" => true,
        // Unknown/other allowlisted subcommands: fail closed.
        _ => true,
    }
}

pub fn enforce_gh_repo_targets(
    args: &[String],
    allowlist: &OwnerAllowlist,
    implicit_repo: Option<&str>,
) -> Result<()> {
    let selectors = gh_repo_selectors(args);
    for selector in &selectors {
        allowlist.enforce_repo_selector(selector)?;
    }
    let positionals = gh_positional_repo_targets(args);
    for selector in &positionals {
        allowlist.enforce_repo_selector(selector)?;
    }
    if !selectors.is_empty() || !positionals.is_empty() {
        return Ok(());
    }
    // The implicit GH_REPO selector only applies to commands that actually
    // consume repository context. Repository-independent commands like
    // `gh auth status` must not be rejected just because GH_REPO is set.
    if gh_command_uses_repo_context(args)
        && let Some(selector) = implicit_repo.filter(|value| !value.trim().is_empty())
    {
        allowlist.enforce_repo_selector(selector)?;
    }
    Ok(())
}

/// Owner-bearing positionals that `-R` / `GH_REPO` do not cover: `gh repo`
/// operands and GitHub issue/PR URLs or `owner/repo#n` tokens.
fn gh_positional_repo_targets(args: &[String]) -> Vec<String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Vec::new();
    };
    match subcommand {
        "repo" => gh_repo_positional_targets(&args[1..]),
        "pr" | "issue" => gh_issue_or_pr_positional_targets(&args[1..]),
        _ => Vec::new(),
    }
}

fn gh_repo_positional_targets(args: &[String]) -> Vec<String> {
    let Some(repo_sub) = first_positional_after(args) else {
        return Vec::new();
    };
    if !matches!(
        repo_sub,
        "delete"
            | "edit"
            | "view"
            | "archive"
            | "unarchive"
            | "rename"
            | "sync"
            | "create"
            | "fork"
    ) {
        return Vec::new();
    }
    first_operand_after(args, repo_sub)
        .map(str::to_owned)
        .into_iter()
        .collect()
}

fn first_operand_after<'a>(args: &'a [String], token: &str) -> Option<&'a str> {
    let idx = args.iter().position(|arg| arg == token)?;
    first_positional_after(&args[idx + 1..])
}

fn gh_issue_or_pr_positional_targets(args: &[String]) -> Vec<String> {
    args.iter()
        .filter_map(|token| positional_github_repo_token(token))
        .collect()
}

fn positional_github_repo_token(token: &str) -> Option<String> {
    if token.starts_with('-') {
        return None;
    }
    let bare = token.split('#').next().unwrap_or(token);
    if normalize_github_repo_identity(bare).is_some() {
        return Some(bare.to_owned());
    }
    parse_github_issue_or_pr_url(bare)
}

fn parse_github_issue_or_pr_url(token: &str) -> Option<String> {
    let rest = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))?;
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [host, owner, repo, kind, ..]
            if matches!(*kind, "pull" | "issues" | "issue")
                && (host.contains('.') || *host == "github.com") =>
        {
            Some(format!("{owner}/{repo}"))
        }
        _ => None,
    }
}

/// Pin a validated origin slug as an explicit `--repo` so later `gh` spawn
/// cannot re-resolve a mutated local origin after a supervisor permit wait.
#[must_use]
pub fn pin_gh_repo_selector(mut args: Vec<String>, origin_slug: &str) -> Vec<String> {
    if gh_repo_selector(&args).is_some() {
        return args;
    }
    args.push("--repo".to_owned());
    args.push(origin_slug.to_owned());
    args
}
