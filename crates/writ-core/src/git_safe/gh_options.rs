//! Static option arity relevant to GitHub target discovery.
//!
//! Audited against installed gh 2.96 help for the supported command families.
//! This is a finite arity table, not a command allowlist or a complete grammar.
//! New ambiguous flags need command-specific metadata plus selector/value tests.

#[derive(Clone, Copy, Default)]
pub(super) struct CommandOptions {
    /// Flags that never consume a separate value, including browse's optional
    /// --commit[=SHA] / -c[=SHA], whose bare form defaults to the last commit.
    pub non_values: &'static [&'static str],
    /// Required values missing from, or overloaded against, the common table.
    pub values: &'static [&'static str],
}

pub(super) fn for_command(family: &str, verb: Option<&str>) -> CommandOptions {
    let verb = verb.map(|verb| canonical_verb(family, verb));
    CommandOptions {
        non_values: non_value_flags(family, verb),
        values: value_flags(family, verb),
    }
}

/// Built-in aliases documented by gh 2.96 help; configured user aliases are opaque.
pub(super) fn canonical_verb<'a>(family: &str, verb: &'a str) -> &'a str {
    match (family, verb) {
        ("pr", "co") => "checkout",
        ("pr" | "issue" | "gist" | "release" | "repo" | "repo autolink", "new") => "create",
        ("secret" | "variable", "remove") => "delete",
        (
            "pr" | "issue" | "gist" | "label" | "release" | "repo" | "secret" | "ssh-key"
            | "variable" | "workflow" | "repo autolink" | "repo deploy-key" | "repo gitignore"
            | "repo license",
            "ls",
        ) => "list",
        _ => verb,
    }
}

/// Before the verb is known, these audited required-value aliases must not be
/// mistaken for the boolean meanings they have in other leaf commands.
pub(super) const PREVERB_VALUE_ALIASES: &[&str] =
    &["-a", "-B", "-l", "-m", "-r", "-s", "-i", "-e", "-n"];

fn non_value_flags(family: &str, verb: Option<&str>) -> &'static [&'static str] {
    match (family, verb) {
        ("auth", Some("login" | "refresh")) => &["-c", "--clipboard"],
        ("auth", Some("setup-git")) => &["-f", "--force"],
        ("auth", Some("status")) => &["-t", "--show-token"],
        ("browse", _) => &["-p", "--projects", "-c", "--commit"],
        ("gist", Some("create")) => &["-p", "--public"],
        ("issue", Some("develop")) => &["-c", "--checkout", "-l", "--list"],
        ("label", Some("clone" | "create")) | ("pr", Some("checkout")) => &["-f", "--force"],
        ("pr", Some("create")) => &["-f", "--fill", "-e", "--editor"],
        ("pr" | "issue", Some("comment")) => &["-e", "--editor"],
        ("pr", Some("review")) => &[
            "-c",
            "--comment",
            "-a",
            "--approve",
            "-r",
            "--request-changes",
        ],
        ("pr", Some("status")) => &["-c", "--conflict-status"],
        ("pr" | "issue", Some("view")) => &["-c", "--comments"],
        ("release", Some("create")) => &["-p", "--prerelease"],
        ("repo", Some("create")) => &["-c", "--clone"],
        ("repo", Some("edit")) => &["--template"],
        ("repo", Some("sync")) => &["--force"],
        ("workflow", Some("run")) => &["--json"],
        _ => &[],
    }
}

fn value_flags(family: &str, verb: Option<&str>) -> &'static [&'static str] {
    match (family, verb) {
        ("pr", Some("create")) => &["-a", "-B", "-l", "-m", "-r", "--reviewer", "--recover"],
        ("pr", Some("edit")) => &[
            "--add-assignee",
            "--add-label",
            "--add-project",
            "--add-reviewer",
            "-B",
            "-m",
            "--remove-assignee",
            "--remove-label",
            "--remove-project",
            "--remove-reviewer",
        ],
        ("issue", Some("create")) => &[
            "-a",
            "--blocked-by",
            "--blocking",
            "-l",
            "-m",
            "--parent",
            "--recover",
            "--type",
        ],
        ("issue", Some("edit")) => &[
            "--add-assignee",
            "--add-blocked-by",
            "--add-blocking",
            "--add-label",
            "--add-project",
            "--add-sub-issue",
            "-m",
            "--parent",
            "--remove-assignee",
            "--remove-blocked-by",
            "--remove-blocking",
            "--remove-label",
            "--remove-project",
            "--remove-sub-issue",
            "--type",
        ],
        ("pr", Some("list")) => &["--app", "-a", "-B", "-l", "-s"],
        ("issue", Some("list")) => &["--app", "-a", "-l", "--mention", "-m", "-s", "--type"],
        ("issue", Some("develop")) => &["--branch-repo", "-n", "--name"],
        ("issue", Some("close")) => &["--duplicate-of", "-r", "--reason"],
        ("pr" | "issue", Some("lock")) => &["-r", "--reason"],
        ("pr", Some("checkout")) => &["--branch"],
        ("pr", Some("checks")) => &["-i", "--interval"],
        ("pr", Some("diff")) => &["--color", "-e", "--exclude"],
        ("repo", Some("sync")) => &["-s", "--source", "--branch"],
        ("repo", Some("view")) => &["--branch"],
        ("repo", Some("create")) => &[
            "-d",
            "--description",
            "-g",
            "--gitignore",
            "-h",
            "--homepage",
            "-l",
            "--license",
            "-r",
            "--remote",
            "-s",
            "--source",
            "--team",
        ],
        ("repo", Some("edit")) => &[
            "--add-topic",
            "--default-branch",
            "-d",
            "--description",
            "-h",
            "--homepage",
            "--remove-topic",
            "--squash-merge-commit-message",
            "--visibility",
        ],
        ("repo", Some("fork")) => &["--fork-name", "--org", "--remote-name"],
        _ => &[],
    }
}

/// Known value-taking options used to distinguish operands from option values.
pub(super) const GH_VALUE_TAKING_OPTIONS: &[&str] = &[
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
    "-c",
    "--subject",
    "--author-email",
    "-A",
    "--match-head-commit",
    "--method",
    "-X",
    "--header",
    "-H",
    "--field",
    "--raw-field",
    "-f",
    "--hostname",
    "--cache",
    "--input",
    "--preview",
    "-p",
];

/// `gh` boolean (non-value-taking) flags whose separate-token form does NOT
/// consume the following argv token. When one of these immediately precedes a
/// literal `--`, that `--` is genuinely the end-of-options terminator.
///
/// This list underpins the fail-closed arity decision in
/// [`super::separate_token_option_may_consume_dashdash`]: only when the preceding option
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
pub(super) const GH_KNOWN_BOOLEAN_FLAGS: &[&str] = &[
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
    "--allow-escape-sequences",
    "--include",
    "--paginate",
    "--silent",
    "--slurp",
    "--verbose",
];
