//! Allowlisted GitHub CLI (`gh`) policy.

use std::borrow::Cow;
use std::collections::HashSet;
use std::process::Command;

use crate::error::{Error, PolicyCode, Result};
use crate::owners::OwnerAllowlist;

use super::identity::{github_repo_slugs_match, normalize_github_repo_identity};
use super::{GitOutput, reject_external_path};

#[path = "gh_options.rs"]
mod options;
use options::{CommandOptions, GH_KNOWN_BOOLEAN_FLAGS, GH_VALUE_TAKING_OPTIONS};

/// GitHub CLI command families supported by the helper. GitHub owns remote
/// authorization; the helper checks explicit repository owners and local effects.
/// `run` is allowlisted so official Actions log views and flake reruns can go
/// through this boundary; nested `run` verbs are restricted separately.
const ALLOWED_GH_SUBCOMMANDS: &[&str] = &[
    "api", "auth", "browse", "gist", "issue", "label", "pr", "release", "repo", "run", "secret",
    "ssh-key", "variable", "workflow",
];

/// `gh run` verbs that fetch logs, download artifacts, or perform an official rerun.
///
/// `delete` and `cancel` stay blocked: they are destructive and are not the
/// Class A flake path (`gh run rerun` / `gh run view --log-failed`).
/// `download` stays allowlisted, but [`gh_reject_external_download_destination`]
/// refuses a `--dir` / `-D` that leaves the worktree.
const ALLOWED_GH_RUN_SUBSUBCOMMANDS: &[&str] = &["download", "list", "rerun", "view", "watch"];

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
    /// that check. Local checkout-changing commands are rejected before the
    /// owner check; all validation completes before [`Self::run`].
    pub fn with_allowlist(args: &[String], allowlist: &OwnerAllowlist) -> Result<Self> {
        let subcommand = gh_subcommand(args)?;
        gh_reject_local_checkout_changes(args)?;
        gh_reject_disallowed_run_verb(subcommand, args)?;
        gh_reject_external_download_destination(subcommand, args)?;
        gh_reject_external_clone_destination(subcommand, args)?;
        gh_enforce_clone_target_owner(subcommand, args, allowlist)?;
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

/// Remote PR changes belong to GitHub. Checkout switching and local branch
/// deletion must instead use the assigned-checkout Git path.
fn gh_reject_local_checkout_changes(args: &[String]) -> Result<()> {
    if gh_requires_branch_check(args) {
        return Err(Error::PolicyViolation {
            code: PolicyCode::MergeBlocked,
            message: "gh command changes the local checkout; use assigned-branch git operations"
                .to_owned(),
        });
    }
    Ok(())
}

/// Restrict `gh run` to log fetch and official rerun verbs.
fn gh_reject_disallowed_run_verb(subcommand: &str, args: &[String]) -> Result<()> {
    if subcommand != "run" {
        return Ok(());
    }
    match first_positional_after(&args[1..]) {
        Some(run_sub) => {
            let allowed_run: HashSet<&str> =
                ALLOWED_GH_RUN_SUBSUBCOMMANDS.iter().copied().collect();
            if !allowed_run.contains(run_sub) {
                return Err(Error::PolicyViolation {
                    code: PolicyCode::GhSubcommandNotAllowed,
                    message: format!("`gh run {run_sub}` is not allowed"),
                });
            }
        }
        None => {
            return Err(Error::PolicyViolation {
                code: PolicyCode::GhSubcommandNotAllowed,
                message: "`gh run` requires an allowed verb (view, list, watch, rerun, download)"
                    .to_owned(),
            });
        }
    }
    Ok(())
}

/// `gh run download` extracts artifacts into `--dir` / `-D` (default `.`).
///
/// Reject destinations that leave the worktree. `--name` and `--pattern` select
/// artifacts; they are not output paths. Unknown download flags are rejected so
/// a clustered short cannot hide an outside destination.
///
/// # Validation split (PR #183 review follow-up)
///
/// This runs only the cwd-independent **string** gate ([`reject_external_path`]:
/// absolute / `..` / drive / UNC). It is reached from
/// [`SafeGhCommand::with_allowlist`], which has no worktree context:
/// [`SafeGhCommand::new`] is a direct-call path with no supervised worktree, and
/// the child `gh` is actually spawned with its cwd set to the resolved worktree
/// (`prepared.cwd`), not the supervisor process cwd. Resolving a symlink here
/// against `std::env::current_dir()` would therefore inspect the wrong directory
/// in the normal supervised case. The symlink-resolution gate is applied
/// separately against the real worktree root in
/// [`reject_external_gh_download_destination_in`], called from
/// `supervisor_cmd::prepare_gh_command` where the worktree root is known.
/// Direct `SafeGhCommand::new` callers still get the string gate here.
fn gh_reject_external_download_destination(subcommand: &str, args: &[String]) -> Result<()> {
    if subcommand != "run" || first_positional_after(&args[1..]) != Some("download") {
        return Ok(());
    }
    for dest in gh_run_download_dirs(args)? {
        // Cwd-independent string gate: rejects absolute / `..` / drive / UNC
        // spellings. The symlink-resolution gate is rooted at the worktree in
        // `reject_external_gh_download_destination_in` (supervised path).
        reject_external_path(Some(dest), "gh run download destination")?;
    }
    Ok(())
}

/// Validate a `gh run download` command's `--dir` / `-D` destinations against an
/// explicit `worktree_root` (the resolved repo dir that becomes the child `gh`
/// process cwd), running **both** gates fail-closed:
///
/// 1. the pure-string [`reject_external_path`] check (absolute / `..` / drive /
///    UNC), unchanged; and
/// 2. the symlink-resolution check, which joins/resolves each destination
///    against the canonicalized `worktree_root` and confirms the resolved
///    nearest-existing-ancestor stays within it, rejecting symlink components.
///
/// This is the worktree-root-aware entry point used by the supervisor so the
/// defense inspects the directory the child actually extracts into, regardless
/// of the supervisor's own process cwd. It is a pure function of
/// `(args, worktree_root)` and does not read `std::env::current_dir()`.
///
/// A no-op unless `args` is a `gh run download` invocation.
pub fn reject_external_gh_download_destination_in(
    args: &[String],
    worktree_root: &std::path::Path,
) -> Result<()> {
    if args.first().map(String::as_str) != Some("run")
        || first_positional_after(&args[1..]) != Some("download")
    {
        return Ok(());
    }
    for dest in gh_run_download_dirs(args)? {
        // First gate: the pure-string check rejects absolute / `..` / drive /
        // UNC spellings.
        reject_external_path(Some(dest), "gh run download destination")?;
        // Second gate: a plain relative name can still be (or traverse) a
        // symlink that escapes the worktree. Resolve it against the real
        // worktree root fail-closed.
        reject_symlinked_download_destination_in(dest, worktree_root)?;
    }
    Ok(())
}

/// Reject a `gh run download --dir` destination that escapes `worktree_root` via
/// a symlink, which the pure-string [`reject_external_path`] gate cannot see.
///
/// The destination may not exist yet, so this resolves the nearest existing
/// ancestor (walking up component by component), canonicalizes it, and confirms
/// it stays within the canonicalized `worktree_root` (the directory the child
/// `gh` process runs in). It also rejects when any existing leading component is
/// itself a symlink. Fails closed on any IO error that prevents validating an
/// existing component.
///
/// `worktree_root` is passed explicitly (not read from `std::env::current_dir()`)
/// so the gate inspects the directory the child actually extracts into even when
/// the supervisor's process cwd differs from the worktree.
fn reject_symlinked_download_destination_in(
    dest: &str,
    worktree_root: &std::path::Path,
) -> Result<()> {
    use std::path::Path;

    let root = worktree_root.canonicalize().map_err(|e| Error::Io {
        context: "canonicalize worktree root for gh run download destination",
        source: e,
    })?;

    let joined = root.join(Path::new(dest));

    let Some((existing, metadata)) = nearest_existing_download_ancestor(&joined)? else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(symlinked_download_escape(dest));
    }
    let canonical = existing.canonicalize().map_err(|e| Error::Io {
        context: "canonicalize gh run download destination ancestor",
        source: e,
    })?;
    if !canonical.starts_with(&root) {
        return Err(symlinked_download_escape(dest));
    }
    Ok(())
}

fn nearest_existing_download_ancestor(
    path: &std::path::Path,
) -> Result<Option<(std::path::PathBuf, std::fs::Metadata)>> {
    for candidate in path.ancestors() {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) => return Ok(Some((candidate.to_path_buf(), metadata))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    context: "inspect gh run download destination",
                    source,
                });
            }
        }
    }
    Ok(None)
}

fn symlinked_download_escape(dest: &str) -> Error {
    Error::PolicyViolation {
        code: PolicyCode::PathNotAllowed,
        message: format!(
            "`gh run download destination` `{dest}` resolves outside the worktree via a symlink"
        ),
    }
}

enum DownloadStep<'a> {
    Done,
    Advance(usize),
    Dir(&'a str, usize),
}

/// Collect `--dir` / `-D` values. One step per token keeps the scan flat.
fn gh_run_download_dirs(args: &[String]) -> Result<Vec<&str>> {
    let mut dirs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match download_argv_step(args, i)? {
            DownloadStep::Done => break,
            DownloadStep::Advance(next) => i = next,
            DownloadStep::Dir(value, next) => {
                dirs.push(value);
                i = next;
            }
        }
    }
    Ok(dirs)
}

fn download_argv_step(args: &[String], i: usize) -> Result<DownloadStep<'_>> {
    let arg = args[i].as_str();
    if arg == "--" {
        return Ok(DownloadStep::Done);
    }
    let next = args.get(i + 1).map(String::as_str);
    if let Some(step) = download_dir_step(arg, next, i)? {
        return Ok(step);
    }
    if let Some(next_i) = download_known_skip(arg, i) {
        return Ok(DownloadStep::Advance(next_i));
    }
    if arg.starts_with('-') {
        return Err(Error::PolicyViolation {
            code: PolicyCode::GhFlagNotAllowed,
            message: format!("`gh run download` flag `{arg}` is not allowed"),
        });
    }
    Ok(DownloadStep::Advance(i + 1))
}

fn download_dir_step<'a>(
    arg: &'a str,
    next: Option<&'a str>,
    i: usize,
) -> Result<Option<DownloadStep<'a>>> {
    let Some((value, consume_next)) = gh_download_dir_flag(arg, next) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(missing_download_destination());
    }
    let next_i = if consume_next { i + 2 } else { i + 1 };
    Ok(Some(DownloadStep::Dir(value, next_i)))
}

fn missing_download_destination() -> Error {
    Error::PolicyViolation {
        code: PolicyCode::PathNotAllowed,
        message: "`gh run download destination` requires a relative path under the worktree"
            .to_owned(),
    }
}

fn gh_download_dir_flag<'a>(arg: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool)> {
    if arg == "--dir" || arg == "-D" {
        return Some((next.unwrap_or(""), true));
    }
    if let Some(value) = arg.strip_prefix("--dir=") {
        return Some((value, false));
    }
    if let Some(value) = arg.strip_prefix("-D=") {
        return Some((value, false));
    }
    attached_short_d(arg).map(|value| (value, false))
}

fn attached_short_d(arg: &str) -> Option<&str> {
    let value = arg.strip_prefix("-D")?;
    (!value.is_empty() && !value.starts_with('-')).then_some(value)
}

/// Advance past a known non-destination download flag and its value, if any.
fn download_known_skip(arg: &str, i: usize) -> Option<usize> {
    if is_download_bool_flag(arg) || is_attached_download_value(arg) {
        return Some(i + 1);
    }
    if is_separate_download_value(arg) {
        return Some(i + 2);
    }
    None
}

fn is_download_bool_flag(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

fn is_separate_download_value(arg: &str) -> bool {
    if arg.contains('=') || !arg.starts_with('-') {
        return false;
    }
    matches!(arg, "--name" | "-n" | "--pattern" | "-p")
        || GH_VALUE_TAKING_OPTIONS.contains(&arg)
        || matches!(gh_repo_flag(arg, Some("x")), Some((_, true)))
}

fn is_attached_download_value(arg: &str) -> bool {
    arg.starts_with("--name=")
        || arg.starts_with("--pattern=")
        || attached_nonempty_short(arg, "-n")
        || attached_nonempty_short(arg, "-p")
        || matches!(gh_repo_flag(arg, None), Some((_, false)))
}

fn attached_nonempty_short(arg: &str, flag: &str) -> bool {
    arg.strip_prefix(flag).is_some_and(|rest| !rest.is_empty())
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

/// Arity of one supported pflag short-option cluster. Value-taking options
/// consume the remaining suffix, so its letters are never interpreted as flags.
struct ShortOptions {
    tokens: usize,
    delete_branch: Option<bool>,
}

fn short_options(arg: &str) -> Option<ShortOptions> {
    short_options_with_context(arg, CommandOptions::default())
}

fn short_options_with_context(arg: &str, context: CommandOptions) -> Option<ShortOptions> {
    let body = arg
        .strip_prefix('-')
        .filter(|body| !body.starts_with('-') && !body.is_empty())?;
    let mut delete_branch = None;
    for (index, flag) in body.char_indices() {
        let suffix = &body[index + flag.len_utf8()..];
        if short_value_in_context(flag, context) {
            return Some(ShortOptions {
                tokens: if suffix.is_empty() { 2 } else { 1 },
                delete_branch,
            });
        }
        if !short_boolean_in_context(flag, context) {
            return None;
        }
        if flag == 'd' {
            delete_branch = Some(suffix.strip_prefix('=').is_none_or(gh_boolean_enabled));
        }
        if suffix.starts_with('=') {
            break;
        }
    }
    Some(ShortOptions {
        tokens: 1,
        delete_branch,
    })
}

fn short_boolean_in_context(flag: char, context: CommandOptions) -> bool {
    matches!(flag, 'h' | 'i' | 'w' | 'd' | 'm' | 'r' | 's')
        || has_short_flag(context.non_values, flag)
}

fn short_value_in_context(flag: char, context: CommandOptions) -> bool {
    (short_option_takes_value(flag) || has_short_flag(context.values, flag))
        && !has_short_flag(context.non_values, flag)
}

fn has_short_flag(flags: &[&str], flag: char) -> bool {
    flags
        .iter()
        .any(|option| option.len() == 2 && char::from(option.as_bytes()[1]) == flag)
}

fn short_option_takes_value(flag: char) -> bool {
    if flag == 'R' {
        return true;
    }
    GH_VALUE_TAKING_OPTIONS
        .iter()
        .filter(|option| option.len() == 2)
        .any(|option| char::from(option.as_bytes()[1]) == flag)
}

fn gh_boolean_enabled(value: &str) -> bool {
    !matches!(value, "0" | "f" | "F" | "false" | "False" | "FALSE")
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
    match positional_option_tokens(a, args.get(idx + 1).map(String::as_str)) {
        Some(tokens) => PositionalScan::Skip(tokens),
        None => PositionalScan::Stop(None),
    }
}

/// Number of argv tokens consumed by a supported option and its value.
fn positional_option_tokens(arg: &str, next: Option<&str>) -> Option<usize> {
    if let Some(options) = short_options(arg) {
        return Some(options.tokens);
    }
    if let Some((_, consume_next)) = gh_repo_flag(arg, next) {
        return Some(1 + usize::from(consume_next));
    }
    let (flag, attached) = match arg.split_once('=') {
        Some((flag, _)) => (flag, true),
        None => (arg, false),
    };
    if GH_KNOWN_BOOLEAN_FLAGS.contains(&flag) {
        return Some(1);
    }
    GH_VALUE_TAKING_OPTIONS
        .contains(&flag)
        .then_some(1 + usize::from(!attached))
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
///
/// Only `gh pr` forms that switch or delete the local checkout qualify:
/// `checkout`, and `merge` / `close` with `--delete-branch`. Remote mutations
/// (`merge`, `ready`, `update-branch`, reviews, `gh run` reruns) are GitHub's
/// authorization decision; `gh run download` writes worktree files but is
/// bounded by its own destination gates, so it is not a checkout change.
#[must_use]
pub fn gh_requires_branch_check(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("pr") {
        return false;
    }
    let Some(pr_sub) = command_verb(args) else {
        return false;
    };
    matches!(pr_sub, "checkout")
        || (matches!(pr_sub, "merge" | "close") && gh_deletes_local_branch(args))
}
fn gh_deletes_local_branch(args: &[String]) -> bool {
    let mut i = 1;
    let mut delete_branch = false;
    while let Some(arg) = args.get(i).map(String::as_str) {
        if arg == "--" {
            break;
        }
        delete_branch = delete_branch_setting(arg).unwrap_or(delete_branch);
        i += positional_option_tokens(arg, args.get(i + 1).map(String::as_str)).unwrap_or(1);
    }
    delete_branch
}

fn delete_branch_setting(arg: &str) -> Option<bool> {
    if arg == "--delete-branch" {
        return Some(true);
    }
    arg.strip_prefix("--delete-branch=")
        .map(gh_boolean_enabled)
        .or_else(|| short_options(arg).and_then(|options| options.delete_branch))
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
        if GH_KNOWN_BOOLEAN_FLAGS.contains(&a) || a == "-h" {
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

/// Optional caller-requested binding of a `gh` effective repo selector (explicit
/// `-R`/`--repo` or implicit `GH_REPO`) does not match the verified local
/// `origin` slug.
///
/// Retained for callers that explicitly need a local-origin relationship. Remote
/// supervised GitHub commands do not require this binding.
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
    gh_arguments(args).selectors
}

#[derive(Default)]
struct GhArguments<'a> {
    selectors: Vec<&'a str>,
    operands: Vec<&'a str>,
}

fn gh_arguments(args: &[String]) -> GhArguments<'_> {
    let mut parsed = GhArguments::default();
    let context = command_options(args);
    let mut i = 0;
    while i < args.len() {
        match gh_selector_scan_step(args, i, context) {
            GhSelectorStep::Done => {
                parsed
                    .operands
                    .extend(args[i + 1..].iter().map(String::as_str));
                break;
            }
            GhSelectorStep::Advance(next) => i = next,
            GhSelectorStep::Found { selector, next } => {
                parsed.selectors.push(selector);
                i = next;
            }
            GhSelectorStep::Operand { value, next } => {
                parsed.operands.push(value);
                i = next;
            }
        }
    }
    parsed
}

fn command_options(args: &[String]) -> CommandOptions {
    let family = args.first().map(String::as_str).unwrap_or("");
    options::for_command(family, command_verb(args))
}

/// Discover the verb without searching payload text for command names. The
/// complete argument traversal then uses that verb's exact option metadata.
fn command_verb(args: &[String]) -> Option<&str> {
    let family = args.first()?.as_str();
    let mut i = 1;
    while let Some(arg) = args.get(i).map(String::as_str) {
        if arg == "--" {
            return args
                .get(i + 1)
                .map(|verb| options::canonical_verb(family, verb));
        }
        if !arg.starts_with('-') {
            return Some(options::canonical_verb(family, arg));
        }
        i += preverb_option_tokens(family, arg);
    }
    None
}

fn preverb_option_tokens(family: &str, arg: &str) -> usize {
    // Repo create/edit use -h for homepage, including before the verb. Keep
    // its payload out of command discovery; --help remains unambiguous.
    if family == "repo"
        && let Some(value) = arg.strip_prefix("-h")
    {
        return 1 + usize::from(value.is_empty());
    }
    if matches!(arg, "-h" | "--help") || arg.contains('=') {
        return 1;
    }
    // gh's parent-command discovery consumes separate leaf-option values;
    // bare leaf booleans before a verb are not equivalent to '=true' forms.
    if arg.starts_with("--") || arg.len() == 2 {
        return 2;
    }
    let context = CommandOptions {
        values: options::PREVERB_VALUE_ALIASES,
        ..CommandOptions::default()
    };
    short_options_with_context(arg, context).map_or(1, |options| options.tokens)
}

fn selector_option_tokens(arg: &str, next: Option<&str>, context: CommandOptions) -> Option<usize> {
    let flag = arg.split('=').next().unwrap_or(arg);
    if context.non_values.contains(&flag) {
        return Some(1);
    }
    if context.values.contains(&flag) {
        return Some(1 + usize::from(!arg.contains('=')));
    }
    short_options_with_context(arg, context)
        .map(|options| options.tokens)
        .or_else(|| positional_option_tokens(arg, next))
}

enum GhSelectorStep<'a> {
    Done,
    Advance(usize),
    Found { selector: &'a str, next: usize },
    Operand { value: &'a str, next: usize },
}

fn gh_selector_scan_step<'a>(
    args: &'a [String],
    i: usize,
    context: CommandOptions,
) -> GhSelectorStep<'a> {
    let a = args[i].as_str();
    if a == "--" {
        let previous_tokens = i
            .checked_sub(1)
            .and_then(|previous| selector_option_tokens(&args[previous], Some("--"), context));
        if previous_tokens == Some(1) {
            return GhSelectorStep::Done;
        }
        // A `--` that a preceding value-taking option consumes as its value
        // is not the options terminator; gh keeps parsing, so a later
        // `-R other/repo` must still be detected. Fail closed by continuing.
        if dashdash_is_option_value(args, i) {
            return GhSelectorStep::Advance(i + 1);
        }
        return GhSelectorStep::Done;
    }
    if !a.starts_with('-') {
        return GhSelectorStep::Operand {
            value: a,
            next: i + 1,
        };
    }
    let next = args.get(i + 1).map(String::as_str);
    if let Some((selector, consume_next)) = gh_repo_flag_with_context(a, next, context) {
        return GhSelectorStep::Found {
            selector,
            next: if consume_next { i + 2 } else { i + 1 },
        };
    }
    // Skip a non-selector value-taking option together with its value so a
    // `--` value cannot terminate scanning prematurely.
    GhSelectorStep::Advance(i + selector_option_tokens(a, next, context).unwrap_or(1))
}

/// Parse one argv token as a `gh` repo selector flag.
///
/// Returns `(selector, consume_next)` when `arg` is `-R` / `--repo` in any
/// accepted spelling. A missing required value is an empty selector so callers
/// can fail closed.
fn gh_repo_flag<'a>(arg: &'a str, next: Option<&'a str>) -> Option<(&'a str, bool)> {
    gh_repo_flag_with_context(arg, next, CommandOptions::default())
}

fn gh_repo_flag_with_context<'a>(
    arg: &'a str,
    next: Option<&'a str>,
    context: CommandOptions,
) -> Option<(&'a str, bool)> {
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
    clustered_short_repo_flag(arg, next, context)
}

fn clustered_short_repo_flag<'a>(
    arg: &'a str,
    next: Option<&'a str>,
    context: CommandOptions,
) -> Option<(&'a str, bool)> {
    if !arg.starts_with('-') || arg.starts_with("--") {
        return None;
    }
    let body = &arg[1..];
    let (letters, eq_value) = match body.split_once('=') {
        Some((letters, value)) => (letters, Some(value)),
        None => (body, None),
    };
    let r_idx = letters.find('R')?;
    if !letters[..r_idx]
        .chars()
        .all(|c| c.is_ascii_alphabetic() && !short_value_in_context(c, context))
    {
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
    let parsed = gh_arguments(args);
    for selector in &parsed.selectors {
        allowlist.enforce_repo_selector(selector)?;
    }
    let positionals = gh_positional_repo_targets(args, &parsed.operands)?;
    for selector in &positionals {
        allowlist.enforce_repo_selector(selector)?;
    }
    if let Some(owner) = gh_api_owner_target(args, &parsed.operands)? {
        return allowlist.enforce_owner(&owner);
    }
    if !parsed.selectors.is_empty() || !positionals.is_empty() {
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
/// operands, GitHub issue/PR URLs, and an issue-transfer destination repository.
fn gh_positional_repo_targets(args: &[String], operands: &[&str]) -> Result<Vec<String>> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Ok(Vec::new());
    };
    match subcommand {
        "repo" => Ok(gh_repo_positional_targets(args, operands)),
        "pr" | "issue" => gh_issue_or_pr_positional_targets(args, operands),
        _ => Ok(Vec::new()),
    }
}

/// Scope literal repos/OWNER/REPO and orgs/OWNER/repos or users/OWNER/repos
/// endpoints without interpreting opaque bodies or GraphQL node IDs. Owner
/// placeholders use gh's context; a literal owner remains enforceable when
/// only the repository is a placeholder.
fn gh_api_owner_target(args: &[String], operands: &[&str]) -> Result<Option<String>> {
    if args.first().map(String::as_str) != Some("api") {
        return Ok(None);
    }
    let Some(&endpoint) = operands.get(1) else {
        return Ok(None);
    };
    let path = match http_url_parts(endpoint)? {
        Some((host, path)) if host.eq_ignore_ascii_case("api.github.com") => path,
        Some(_) => return Ok(None),
        None => endpoint,
    };
    let path = decode_http_path(path)?;
    Ok(gh_api_path_owner(&path).map(str::to_owned))
}

fn gh_api_path_owner(path: &str) -> Option<&str> {
    let mut parts = path.trim_start_matches('/').split('/');
    let namespace = parts.next()?;
    let owner = parts.next()?;
    let resource = parts.next()?;
    match namespace {
        "repos" => {}
        "orgs" | "users" if resource == "repos" => {}
        _ => return None,
    }
    if owner.contains('{') {
        return None;
    }
    Some(owner)
}

fn gh_repo_positional_targets(args: &[String], operands: &[&str]) -> Vec<String> {
    let Some(repo_sub) = command_verb(args) else {
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
    operands
        .get(2)
        .map(|operand| (*operand).to_owned())
        .into_iter()
        .collect()
}

fn gh_issue_or_pr_positional_targets(args: &[String], operands: &[&str]) -> Result<Vec<String>> {
    let mut targets: Vec<String> = operands
        .iter()
        .filter_map(|token| positional_github_repo_token(token).transpose())
        .collect::<Result<_>>()?;
    if let Some(destination) = issue_transfer_destination(args, operands) {
        targets.push(destination.to_owned());
    }
    Ok(targets)
}

fn issue_transfer_destination<'a>(args: &[String], operands: &[&'a str]) -> Option<&'a str> {
    if args.first().map(String::as_str) != Some("issue") {
        return None;
    }
    if command_verb(args) != Some("transfer") {
        return None;
    }
    // Family, verb, issue identifier, then the destination repository. An
    // unqualified destination name has no explicit owner to check here.
    operands
        .get(3)
        .copied()
        .filter(|destination| normalize_github_repo_identity(destination).is_some())
}

fn positional_github_repo_token(token: &str) -> Result<Option<String>> {
    if token.starts_with('-') {
        return Ok(None);
    }
    let bare = token.split('#').next().unwrap_or(token);
    if let Some((host, path)) = http_url_parts(bare)? {
        let path = decode_http_path(path)?;
        return Ok(github_http_repo_target(host, &path));
    }
    Ok(None)
}

fn github_http_repo_target(host: &str, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [owner, repo] => Some(format!("{owner}/{repo}")),
        [owner, repo, kind, ..]
            if matches!(*kind, "pull" | "issues" | "issue") && host.contains('.') =>
        {
            Some(format!("{owner}/{repo}"))
        }
        _ => None,
    }
}

/// Decode only the path once. Encoded '?' and '#' remain path bytes, while
/// literal query/fragment delimiters are removed before percent decoding.
fn decode_http_path(path: &str) -> Result<Cow<'_, str>> {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if !path.contains('%') {
        return Ok(Cow::Borrowed(path));
    }
    let invalid = || Error::PolicyViolation {
        code: PolicyCode::OwnerNotAllowed,
        message: "malformed percent-encoded HTTP target path".to_owned(),
    };
    let mut bytes = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(url_hex_byte).ok_or_else(invalid)?;
            let low = bytes.next().and_then(url_hex_byte).ok_or_else(invalid)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded)
        .map(Cow::Owned)
        .map_err(|_| invalid())
}

fn url_hex_byte(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse ordinary HTTP(S) DNS authorities without granting host authorization.
/// A malformed HTTP target is an error, never an unchecked fallback selector.
fn http_url_parts(token: &str) -> Result<Option<(&str, &str)>> {
    let Some((scheme, rest)) = token.split_once("://") else {
        return Ok(None);
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Ok(None);
    }
    let invalid = || Error::PolicyViolation {
        code: PolicyCode::OwnerNotAllowed,
        message: "cannot determine owner from malformed HTTP target authority".to_owned(),
    };
    let boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..boundary];
    let path = rest[boundary..].strip_prefix('/').unwrap_or("");
    let host_port = authority.rsplit('@').next().ok_or_else(invalid)?;
    let host = match host_port.split_once(':') {
        Some((host, port)) => {
            port.parse::<u16>().map_err(|_| invalid())?;
            host
        }
        None => host_port,
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    if !valid_http_host(host) {
        return Err(invalid());
    }
    Ok(Some((host, path)))
}

fn valid_http_host(host: &str) -> bool {
    host.split('.').all(|label| {
        if label.is_empty() {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
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
