//! Best-effort Bash statement and argv extraction for PreToolUse admission.
//!
//! This is not a shell. It only splits compounds and tokens far enough to
//! feed `SafeGitCommand` / `SafeGhCommand`. Quote-toggle rules are kept
//! identical to the original scanner, including tokenize's double-quote
//! check, so admission behavior does not change.

use std::iter::Peekable;
use std::str::Chars;

use crate::supervisor::normalize_program_name;

/// Borrowed shell text (command, statement, or substitution body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellText<'a>(pub &'a str);

/// argv slice after environment assignments have been stripped.
#[derive(Clone, Copy)]
struct Argv<'a>(&'a [String]);

#[derive(Clone, Copy)]
struct ArgToken<'a>(&'a str);

#[derive(Clone, Copy)]
enum ScanKind {
    Statements,
    Tokens,
}

#[derive(Default)]
struct QuoteState {
    in_single: bool,
    in_double: bool,
}

impl QuoteState {
    fn quoted(&self) -> bool {
        self.in_single || self.in_double
    }

    fn toggle(&mut self, ch: char, kind: ScanKind) -> bool {
        match (ch, kind) {
            ('\'', _) if !self.in_double => {
                self.in_single = !self.in_single;
                true
            }
            ('"', ScanKind::Statements) if !self.in_single => {
                self.in_double = !self.in_double;
                true
            }
            ('"', ScanKind::Tokens) if !self.in_double => {
                self.in_double = !self.in_double;
                true
            }
            _ => false,
        }
    }
}

#[derive(Default)]
struct ScanBuf {
    quotes: QuoteState,
    current: String,
    out: Vec<String>,
}

impl ScanBuf {
    fn step(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>, kind: ScanKind) {
        if self.try_escape(ch, chars, kind) {
            return;
        }
        if self.quotes.toggle(ch, kind) {
            self.keep_quote_char(ch, kind);
            return;
        }
        self.step_body(ch, chars, kind);
    }

    fn keep_quote_char(&mut self, ch: char, kind: ScanKind) {
        if matches!(kind, ScanKind::Statements) {
            self.current.push(ch);
        }
    }

    fn try_escape(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>, kind: ScanKind) -> bool {
        if ch != '\\' {
            return false;
        }
        if self.quotes.in_single {
            return false;
        }
        self.take_escaped(chars.next(), kind);
        true
    }

    fn take_escaped(&mut self, next: Option<char>, kind: ScanKind) {
        let Some(next) = next else {
            return;
        };
        if matches!(kind, ScanKind::Statements) {
            self.current.push('\\');
        }
        self.current.push(next);
    }

    fn step_body(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>, kind: ScanKind) {
        match kind {
            ScanKind::Statements => self.step_statement(ch, chars),
            ScanKind::Tokens => self.step_token(ch, chars),
        }
    }

    fn step_statement(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>) {
        if self.quotes.quoted() {
            self.current.push(ch);
            return;
        }
        if consume_statement_break(ch, chars) {
            self.emit_trimmed();
            return;
        }
        self.current.push(ch);
    }

    fn step_token(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>) {
        if self.quotes.quoted() {
            self.current.push(ch);
            return;
        }
        if ch.is_whitespace() {
            self.emit_raw();
            return;
        }
        if is_redir_start(ch) {
            self.take_redir(ch, chars);
            return;
        }
        self.current.push(ch);
    }

    fn take_redir(&mut self, ch: char, chars: &mut Peekable<Chars<'_>>) {
        self.emit_raw();
        let mut op = String::from(ch);
        if chars.peek() == Some(&ch) {
            op.push(ch);
            chars.next();
        }
        self.out.push(op);
    }

    fn emit_trimmed(&mut self) {
        let trimmed = self.current.trim();
        if !trimmed.is_empty() {
            self.out.push(trimmed.to_owned());
        }
        self.current.clear();
    }

    fn emit_raw(&mut self) {
        if !self.current.is_empty() {
            self.out.push(std::mem::take(&mut self.current));
        }
    }

    fn finish(mut self, kind: ScanKind) -> Vec<String> {
        match kind {
            ScanKind::Statements => self.emit_trimmed(),
            ScanKind::Tokens => self.emit_raw(),
        }
        self.out
    }
}

pub(crate) fn split_shell_statements(command: ShellText<'_>) -> Vec<String> {
    scan(command, ScanKind::Statements)
}

pub(crate) fn tokenize_shell(statement: ShellText<'_>) -> Vec<String> {
    scan(statement, ScanKind::Tokens)
}

fn scan(text: ShellText<'_>, kind: ScanKind) -> Vec<String> {
    let mut buf = ScanBuf::default();
    let mut chars = text.0.chars().peekable();
    while let Some(ch) = chars.next() {
        buf.step(ch, &mut chars, kind);
    }
    buf.finish(kind)
}

fn consume_statement_break(ch: char, chars: &mut Peekable<Chars<'_>>) -> bool {
    match ch {
        ';' | '\n' => true,
        '&' => {
            consume_repeat(chars, '&');
            true
        }
        '|' => {
            consume_repeat(chars, '|');
            true
        }
        _ => false,
    }
}

fn consume_repeat(chars: &mut Peekable<Chars<'_>>, expected: char) {
    if chars.peek() == Some(&expected) {
        chars.next();
    }
}

fn is_redir_start(ch: char) -> bool {
    ch == '<' || ch == '>'
}

fn is_redir_token(token: ArgToken<'_>) -> bool {
    token.0.starts_with('<') || token.0.starts_with('>')
}

fn tokens_define_git_or_gh(tokens: Argv<'_>) -> bool {
    tokens.0.iter().any(|token| {
        let stem = token.trim_end_matches('{');
        stem.starts_with("git()") || stem.starts_with("gh()")
    })
}

fn opaque_git_gh_shell(tokens: Argv<'_>) -> bool {
    if tokens_define_git_or_gh(tokens) {
        return true;
    }
    let has_redir = tokens.0.iter().any(|token| is_redir_token(ArgToken(token)));
    has_redir && invocation_from_tokens(tokens.0).is_some()
}

#[derive(Clone, Copy)]
struct NestDelims {
    open: char,
    close: char,
}

impl NestDelims {
    const PAREN: Self = Self {
        open: '(',
        close: ')',
    };

    fn delta(self, ch: char) -> i32 {
        i32::from(ch == self.open) - i32::from(ch == self.close)
    }
}

fn extract_substitutions(statement: ShellText<'_>) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = SubCursor {
        source: statement,
        index: 0,
    };
    while cursor.index < statement.0.len() {
        match cursor.take_substitution() {
            Some(inner) => found.push(inner),
            None => cursor.index += 1,
        }
    }
    found
}

struct SubCursor<'a> {
    source: ShellText<'a>,
    index: usize,
}

impl<'a> SubCursor<'a> {
    fn take_substitution(&mut self) -> Option<String> {
        self.take_dollar_paren()
            .or_else(|| self.take_backtick())
            .map(str::to_owned)
    }

    fn take_dollar_paren(&mut self) -> Option<&'a str> {
        let rest = self.source.0.get(self.index..)?;
        let bytes = rest.as_bytes();
        if bytes.first() != Some(&b'$') || bytes.get(1) != Some(&b'(') {
            return None;
        }
        let (inner, end) = take_balanced(ShellText(&rest[2..]), NestDelims::PAREN)?;
        self.index += 2 + end;
        Some(inner)
    }

    fn take_backtick(&mut self) -> Option<&'a str> {
        let rest = self.source.0.get(self.index..)?;
        if rest.as_bytes().first() != Some(&b'`') {
            return None;
        }
        let end = rest.get(1..)?.find('`')?;
        self.index += 2 + end;
        Some(&rest[1..1 + end])
    }
}

fn take_balanced(input: ShellText<'_>, delims: NestDelims) -> Option<(&str, usize)> {
    let mut depth = 1;
    for (idx, ch) in input.0.char_indices() {
        depth += delims.delta(ch);
        if depth == 0 {
            return Some((&input.0[..idx], idx + ch.len_utf8()));
        }
    }
    None
}

fn mentions_git_or_gh(command: ShellText<'_>) -> bool {
    command
        .0
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .any(|token| matches!(token, "git" | "gh"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitGhTool {
    Git,
    Gh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitGhInvocation {
    pub tool: GitGhTool,
    pub args: Vec<String>,
}

pub(crate) struct UnparsedCommand<'a>(pub ShellText<'a>);

pub(crate) fn git_gh_invocations(
    command: ShellText<'_>,
) -> Result<Vec<GitGhInvocation>, UnparsedCommand<'_>> {
    let mut found = Vec::new();
    for statement in split_shell_statements(command) {
        collect_from_statement(command, ShellText(&statement), &mut found)?;
    }
    if found.is_empty() && mentions_git_or_gh(command) {
        return Err(UnparsedCommand(command));
    }
    Ok(found)
}

fn collect_from_statement<'c>(
    command: ShellText<'c>,
    statement: ShellText<'_>,
    found: &mut Vec<GitGhInvocation>,
) -> Result<(), UnparsedCommand<'c>> {
    for inner in extract_substitutions(statement) {
        collect_from_statement(command, ShellText(&inner), found)?;
    }
    let tokens = tokenize_shell(statement);
    if opaque_git_gh_shell(Argv(&tokens)) {
        return Err(UnparsedCommand(command));
    }
    if let Some(invocation) = invocation_from_tokens(&tokens) {
        found.push(invocation);
    }
    Ok(())
}

fn invocation_from_tokens(tokens: &[String]) -> Option<GitGhInvocation> {
    let stripped = strip_env_assignments(Argv(tokens));
    let (program, rest) = stripped.split_first()?;
    let name = normalize_program_name(program);
    let tool = match name.as_str() {
        "git" => GitGhTool::Git,
        "gh" => GitGhTool::Gh,
        _ => return None,
    };
    let args = match tool {
        GitGhTool::Git => skip_git_globals(Argv(rest)),
        GitGhTool::Gh => rest.to_vec(),
    };
    Some(GitGhInvocation { tool, args })
}

fn strip_env_assignments(tokens: Argv<'_>) -> &[String] {
    let mut i = 0;
    while i < tokens.0.len() && is_env_assignment(ArgToken(&tokens.0[i])) {
        i += 1;
    }
    &tokens.0[i..]
}

fn is_env_assignment(token: ArgToken<'_>) -> bool {
    let Some((key, _)) = token.0.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn skip_git_globals(args: Argv<'_>) -> Vec<String> {
    let mut rest = args.0;
    loop {
        let Some(first) = rest.first() else {
            return Vec::new();
        };
        match GitPrefix::classify(ArgToken(first)) {
            GitPrefix::Operand => return rest.to_vec(),
            GitPrefix::EndOfOptions => return rest.get(1..).unwrap_or(&[]).to_vec(),
            GitPrefix::TakesValue => rest = rest.get(2..).unwrap_or(&[]),
            GitPrefix::EqualsForm => rest = rest.get(1..).unwrap_or(&[]),
        }
    }
}

#[derive(Clone, Copy)]
enum GitPrefix {
    EndOfOptions,
    TakesValue,
    EqualsForm,
    Operand,
}

impl GitPrefix {
    fn classify(arg: ArgToken<'_>) -> Self {
        if arg.0 == "--" {
            return Self::EndOfOptions;
        }
        if takes_value(arg) {
            return Self::TakesValue;
        }
        if equals_form(arg) {
            return Self::EqualsForm;
        }
        Self::Operand
    }
}

fn takes_value(arg: ArgToken<'_>) -> bool {
    matches!(
        arg.0,
        "-C" | "-c"
            | "--git-dir"
            | "--work-tree"
            | "--namespace"
            | "--config-env"
            | "--super-prefix"
    )
}

fn equals_form(arg: ArgToken<'_>) -> bool {
    [
        "--git-dir=",
        "--work-tree=",
        "--namespace=",
        "--config-env=",
        "--super-prefix=",
    ]
    .iter()
    .any(|prefix| arg.0.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_compounds_and_keeps_quoted_operators() {
        assert_eq!(
            split_shell_statements(ShellText("npm test && git status")),
            ["npm test", "git status"]
        );
        assert_eq!(
            split_shell_statements(ShellText("echo 'a && b'; git status")),
            ["echo 'a && b'", "git status"]
        );
    }

    #[test]
    fn tokenizes_env_and_path_qualified_git() {
        assert_eq!(
            tokenize_shell(ShellText("FOO=bar /usr/bin/git status")),
            ["FOO=bar", "/usr/bin/git", "status"]
        );
    }

    #[test]
    fn tokenize_strips_quotes_and_keeps_quoted_whitespace() {
        assert_eq!(tokenize_shell(ShellText("'hello world'")), ["hello world"]);
    }

    #[test]
    fn extracts_dollar_and_backtick_substitutions() {
        assert_eq!(
            extract_substitutions(ShellText("echo $(git rev-parse HEAD) `gh pr view`")),
            ["git rev-parse HEAD", "gh pr view"]
        );
    }

    #[test]
    fn skip_git_globals_uses_checked_slices_after_end_of_options() {
        assert_eq!(
            skip_git_globals(Argv(&["--".to_owned()])),
            [] as [String; 0]
        );
        assert_eq!(
            skip_git_globals(Argv(&["--".to_owned(), "status".to_owned()])),
            ["status"]
        );
    }

    #[test]
    fn tokenize_splits_unquoted_redirection_and_keeps_quoted() {
        assert_eq!(
            tokenize_shell(ShellText("git status > /tmp/out")),
            ["git", "status", ">", "/tmp/out"]
        );
        assert_eq!(
            tokenize_shell(ShellText("git status>/tmp/out")),
            ["git", "status", ">", "/tmp/out"]
        );
        assert_eq!(
            tokenize_shell(ShellText("git commit -m 'a > b'")),
            ["git", "commit", "-m", "a > b"]
        );
    }

    #[test]
    fn git_gh_invocations_fail_closed_on_redir_and_function_def() {
        assert!(git_gh_invocations(ShellText("git status > /tmp/out")).is_err());
        assert!(git_gh_invocations(ShellText("git() { :; }; git status")).is_err());
        assert!(git_gh_invocations(ShellText("git status")).is_ok());
    }
}
