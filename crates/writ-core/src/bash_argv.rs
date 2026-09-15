//! Best-effort Bash statement and argv extraction for PreToolUse admission.
//!
//! This is not a shell. It only splits compounds and tokens far enough to
//! feed `SafeGitCommand` / `SafeGhCommand`. Quote-toggle rules are kept
//! identical to the original scanner, including tokenize's double-quote
//! check, so admission behavior does not change.

use std::iter::Peekable;
use std::str::Chars;

#[derive(Default)]
struct QuoteState {
    in_single: bool,
    in_double: bool,
}

impl QuoteState {
    fn quoted(&self) -> bool {
        self.in_single || self.in_double
    }

    fn toggle_keep_quotes(&mut self, ch: char) -> bool {
        if ch == '\'' && !self.in_double {
            self.in_single = !self.in_single;
            true
        } else if ch == '"' && !self.in_single {
            self.in_double = !self.in_double;
            true
        } else {
            false
        }
    }

    fn toggle_strip_quotes(&mut self, ch: char) -> bool {
        if ch == '\'' && !self.in_double {
            self.in_single = !self.in_single;
            true
        } else if ch == '"' && !self.in_double {
            self.in_double = !self.in_double;
            true
        } else {
            false
        }
    }
}

pub(crate) fn split_shell_statements(command: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quotes = QuoteState::default();
    while let Some(ch) = chars.next() {
        if consume_escaped(&mut current, &mut chars, ch, quotes.in_single, true) {
            continue;
        }
        if quotes.toggle_keep_quotes(ch) {
            current.push(ch);
            continue;
        }
        if !quotes.quoted() && consume_statement_break(ch, &mut chars) {
            push_statement(&mut statements, &mut current);
            continue;
        }
        current.push(ch);
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn consume_escaped(
    current: &mut String,
    chars: &mut Peekable<Chars<'_>>,
    ch: char,
    in_single: bool,
    keep_backslash: bool,
) -> bool {
    if ch != '\\' || in_single {
        return false;
    }
    let Some(next) = chars.next() else {
        return true;
    };
    if keep_backslash {
        current.push(ch);
    }
    current.push(next);
    true
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

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_owned());
    }
    current.clear();
}

pub(crate) fn tokenize_shell(statement: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = statement.chars().peekable();
    let mut quotes = QuoteState::default();
    while let Some(ch) = chars.next() {
        if consume_escaped(&mut current, &mut chars, ch, quotes.in_single, false) {
            continue;
        }
        if quotes.toggle_strip_quotes(ch) {
            continue;
        }
        if ch.is_whitespace() && !quotes.quoted() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(crate) fn extract_substitutions(statement: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < statement.len() {
        match substitution_at(statement, i) {
            Some((inner, consumed)) => {
                found.push(inner.to_owned());
                i += consumed;
            }
            None => i += 1,
        }
    }
    found
}

fn substitution_at(statement: &str, i: usize) -> Option<(&str, usize)> {
    dollar_paren_at(statement, i).or_else(|| backtick_at(statement, i))
}

fn dollar_paren_at(statement: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = statement.as_bytes();
    if bytes.get(i) != Some(&b'$') || bytes.get(i + 1) != Some(&b'(') {
        return None;
    }
    let (inner, end) = take_balanced(&statement[i + 2..], '(', ')')?;
    Some((inner, 2 + end))
}

fn backtick_at(statement: &str, i: usize) -> Option<(&str, usize)> {
    if statement.as_bytes().get(i) != Some(&b'`') {
        return None;
    }
    let end = statement.get(i + 1..)?.find('`')?;
    Some((&statement[i + 1..i + 1 + end], 2 + end))
}

fn take_balanced(input: &str, open: char, close: char) -> Option<(&str, usize)> {
    let mut depth = 1;
    for (idx, ch) in input.char_indices() {
        depth += nest_delta(ch, open, close);
        if depth == 0 {
            return Some((&input[..idx], idx + ch.len_utf8()));
        }
    }
    None
}

fn nest_delta(ch: char, open: char, close: char) -> i32 {
    i32::from(ch == open) - i32::from(ch == close)
}

pub(crate) fn mentions_git_or_gh(command: &str) -> bool {
    command
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .any(|token| matches!(token, "git" | "gh"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_compounds_and_keeps_quoted_operators() {
        assert_eq!(
            split_shell_statements("npm test && git status"),
            ["npm test", "git status"]
        );
        assert_eq!(
            split_shell_statements("echo 'a && b'; git status"),
            ["echo 'a && b'", "git status"]
        );
    }

    #[test]
    fn tokenizes_env_and_path_qualified_git() {
        assert_eq!(
            tokenize_shell("FOO=bar /usr/bin/git status"),
            ["FOO=bar", "/usr/bin/git", "status"]
        );
    }

    #[test]
    fn extracts_dollar_and_backtick_substitutions() {
        assert_eq!(
            extract_substitutions("echo $(git rev-parse HEAD) `gh pr view`"),
            ["git rev-parse HEAD", "gh pr view"]
        );
    }
}
