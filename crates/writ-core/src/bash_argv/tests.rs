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
    assert_eq!(
        tokenize_shell(ShellText(r#"git push --for"ce""#)),
        ["git", "push", "--force"]
    );
    assert_eq!(
        tokenize_shell(ShellText(r#"git push "--force""#)),
        ["git", "push", "--force"]
    );
    assert_eq!(
        tokenize_shell(ShellText(r#"git push -"f""#)),
        ["git", "push", "-f"]
    );
}

#[test]
fn extracts_dollar_and_backtick_substitutions() {
    assert_eq!(
        extract_substitutions(ShellText("echo $(git rev-parse HEAD) `gh pr view`")),
        ["git rev-parse HEAD", "gh pr view"]
    );
}

#[track_caller]
fn stripped_git_args(tokens: &[&str]) -> Vec<String> {
    let argv: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
    skip_git_globals(Argv(&argv), &mut None, &mut false)
}

#[test]
fn skip_git_globals_uses_checked_slices_after_end_of_options() {
    assert_eq!(stripped_git_args(&["--"]), [] as [String; 0]);
    assert_eq!(stripped_git_args(&["--", "status"]), ["status"]);
    assert_eq!(
        stripped_git_args(&["-C", "/tmp/repo", "status"]),
        ["status"]
    );
    assert_eq!(
        stripped_git_args(&["-c", "alias.status=!git push --force", "status"]),
        ["-c", "alias.status=!git push --force", "status"]
    );
    assert_eq!(
        stripped_git_args(&["--config-env=alias.status=FOO", "status"]),
        ["--config-env=alias.status=FOO", "status"]
    );
}

#[test]
fn skip_git_globals_tracks_dash_c_target() {
    let argv: Vec<String> = ["-C", "/tmp/repo", "merge", "feature"]
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
    let mut git_dir = None;
    let mut other = false;
    assert_eq!(
        skip_git_globals(Argv(&argv), &mut git_dir, &mut other),
        ["merge", "feature"]
    );
    assert_eq!(git_dir.as_deref(), Some(std::path::Path::new("/tmp/repo")));
    assert!(!other);

    // Chained relative -C operands fold into the first absolute base.
    let argv: Vec<String> = ["-C", "/tmp", "-C", "repo", "status"]
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
    let mut git_dir = None;
    let mut other = false;
    skip_git_globals(Argv(&argv), &mut git_dir, &mut other);
    assert_eq!(git_dir.as_deref(), Some(std::path::Path::new("/tmp/repo")));

    let argv: Vec<String> = ["--git-dir=/x/.git", "status"]
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
    let mut git_dir = None;
    let mut other = false;
    skip_git_globals(Argv(&argv), &mut git_dir, &mut other);
    assert!(git_dir.is_none());
    assert!(other);
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
    assert!(git_gh_invocations(ShellText("g${x-}it push --force")).is_err());
    assert!(git_gh_invocations(ShellText("git status")).is_ok());
    assert!(git_gh_invocations(ShellText("git commit -m '$msg'")).is_ok());
}
