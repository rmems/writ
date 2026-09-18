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
    assert_eq!(
        skip_git_globals(Argv(&[
            "-C".to_owned(),
            "/tmp/repo".to_owned(),
            "status".to_owned()
        ])),
        ["status"]
    );
    assert_eq!(
        skip_git_globals(Argv(&[
            "-c".to_owned(),
            "alias.status=!git push --force".to_owned(),
            "status".to_owned()
        ])),
        ["-c", "alias.status=!git push --force", "status"]
    );
    assert_eq!(
        skip_git_globals(Argv(&[
            "--config-env=alias.status=FOO".to_owned(),
            "status".to_owned()
        ])),
        ["--config-env=alias.status=FOO", "status"]
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
    assert!(git_gh_invocations(ShellText("g${x-}it push --force")).is_err());
    assert!(git_gh_invocations(ShellText("git status")).is_ok());
    assert!(git_gh_invocations(ShellText("git commit -m '$msg'")).is_ok());
}
