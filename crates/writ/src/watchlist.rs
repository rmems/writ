//! CLI handlers for `writ watchlist`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use writ_core::contract::{ErrorData, Response, SCHEMA_VERSION};
use writ_core::watchlist::{
    AddReport, CheckReport, GhPrProbe, WatchEntry, WatchKind, WatchlistError, add_prs_at,
    check_prs_at, default_pr_babysit_path, import_pr_babysit_at, list_prs_at, owner_of_repo,
    remove_pr_at,
};

use super::{WatchlistAction, WatchlistKindArg};

fn state_path(explicit: Option<&PathBuf>) -> PathBuf {
    explicit
        .cloned()
        .unwrap_or_else(writ_core::paths::watchlist_path)
}

/// Run a `writ watchlist` subcommand.
pub(crate) fn run(
    action: WatchlistAction,
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    let path = state_path(action.state());
    match action {
        WatchlistAction::Add {
            repo,
            reset,
            kind,
            targets,
            ..
        } => run_add(
            path.as_path(),
            repo.as_deref(),
            reset,
            kind,
            &targets,
            json,
            stdout,
        ),
        WatchlistAction::Remove {
            repo,
            number,
            targets,
            ..
        } => run_remove(
            path.as_path(),
            repo.as_deref(),
            number,
            &targets,
            json,
            stdout,
        ),
        WatchlistAction::List { repo, owner, .. } => run_list(
            path.as_path(),
            owner.as_deref(),
            repo.as_deref(),
            json,
            stdout,
        ),
        WatchlistAction::Check { repo, number, .. } => {
            run_check(path.as_path(), repo.as_deref(), number, json, stdout)
        }
        WatchlistAction::CheckAll { repo, owner, .. } => run_check_all(
            path.as_path(),
            owner.as_deref(),
            repo.as_deref(),
            json,
            stdout,
        ),
        WatchlistAction::ImportPrBabysit { path: source, .. } => {
            run_import(path.as_path(), source, json, stdout)
        }
    }
}

trait WatchlistStatePath {
    fn state(&self) -> Option<&PathBuf>;
}

impl WatchlistStatePath for WatchlistAction {
    fn state(&self) -> Option<&PathBuf> {
        match self {
            Self::Add { state, .. }
            | Self::Remove { state, .. }
            | Self::List { state, .. }
            | Self::Check { state, .. }
            | Self::CheckAll { state, .. }
            | Self::ImportPrBabysit { state, .. } => state.as_ref(),
        }
    }
}

fn run_remove(
    path: &Path,
    repo: Option<&str>,
    number: Option<u64>,
    targets: &[String],
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    match remove_command(path, repo, number, targets) {
        Ok(entry) => write_remove(json, &entry, stdout).map(|()| ExitCode::SUCCESS),
        Err(err) => write_error("watchlist.remove", json, err, stdout),
    }
}

fn run_list(
    path: &Path,
    owner: Option<&str>,
    repo: Option<&str>,
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    match list_prs_at(path, owner, repo) {
        Ok(entries) => write_list(json, &entries, stdout).map(|()| ExitCode::SUCCESS),
        Err(err) => write_error("watchlist.list", json, err, stdout),
    }
}

fn run_check(
    path: &Path,
    repo: Option<&str>,
    number: u64,
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    match check_one(path, repo, number) {
        Ok(report) => {
            write_check(json, "watchlist.check", &report, stdout).map(|()| ExitCode::SUCCESS)
        }
        Err(err) => write_error("watchlist.check", json, err, stdout),
    }
}

fn run_check_all(
    path: &Path,
    owner: Option<&str>,
    repo: Option<&str>,
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    match check_prs_at(path, &GhPrProbe, owner, repo, None, None) {
        Ok(report) => {
            write_check(json, "watchlist.check_all", &report, stdout).map(|()| ExitCode::SUCCESS)
        }
        Err(err) => write_error("watchlist.check_all", json, err, stdout),
    }
}

fn run_import(
    path: &Path,
    source: Option<PathBuf>,
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    let source = source.unwrap_or_else(default_pr_babysit_path);
    match import_pr_babysit_at(path, &source) {
        Ok(report) => write_add("watchlist.import_pr_babysit", json, &report, stdout)
            .map(|()| ExitCode::SUCCESS),
        Err(err) => write_error("watchlist.import_pr_babysit", json, err, stdout),
    }
}

/// Handle the `watchlist add` arm: resolve state path, add PRs, render output.
fn run_add(
    path: &Path,
    repo_flag: Option<&str>,
    reset: bool,
    kind: WatchlistKindArg,
    targets: &[String],
    json: bool,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    match add_command(path, repo_flag, reset, kind, targets) {
        Ok(report) => write_add("watchlist.add", json, &report, stdout).map(|()| ExitCode::SUCCESS),
        Err(err) => write_error("watchlist.add", json, err, stdout),
    }
}

fn add_command(
    path: &Path,
    repo_flag: Option<&str>,
    reset: bool,
    kind: WatchlistKindArg,
    targets: &[String],
) -> Result<AddReport, WatchlistError> {
    let (repo, numbers) = parse_repo_and_numbers(repo_flag, targets)?;
    if numbers.is_empty() {
        return Err(WatchlistError::InvalidInput(
            "add requires at least one pull-request number".to_owned(),
        ));
    }
    add_prs_at(
        path,
        &GhPrProbe,
        &repo,
        &numbers,
        kind.into_kind(),
        reset,
        None,
    )
}

fn remove_command(
    path: &Path,
    repo_flag: Option<&str>,
    number_flag: Option<u64>,
    targets: &[String],
) -> Result<WatchEntry, WatchlistError> {
    let (repo, numbers) = parse_repo_and_numbers(repo_flag, targets)?;
    let number = match (number_flag, numbers.as_slice()) {
        (Some(number), []) => number,
        (None, [number]) => *number,
        (Some(number), [same]) if *same == number => number,
        _ => {
            return Err(WatchlistError::InvalidInput(
                "remove requires exactly one pull-request number".to_owned(),
            ));
        }
    };
    remove_pr_at(path, &repo, number)
}

fn check_one(
    path: &Path,
    repo_flag: Option<&str>,
    number: u64,
) -> Result<CheckReport, WatchlistError> {
    let repo = match repo_flag {
        Some(repo) => repo.to_owned(),
        None => infer_repo_for_number(path, number)?,
    };
    check_prs_at(
        path,
        &GhPrProbe,
        None,
        Some(repo.as_str()),
        Some(&[number]),
        None,
    )
}

fn infer_repo_for_number(path: &Path, number: u64) -> Result<String, WatchlistError> {
    let entries = list_prs_at(path, None, None)?;
    let matches: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.number == number)
        .collect();
    match matches.as_slice() {
        [entry] => Ok(entry.repo.clone()),
        [] => Err(WatchlistError::NotFound {
            repo: "<unknown>".to_owned(),
            number,
        }),
        _ => Err(WatchlistError::InvalidInput(format!(
            "PR #{number} exists in multiple repos; pass --repo owner/name"
        ))),
    }
}

fn parse_repo_and_numbers(
    repo_flag: Option<&str>,
    targets: &[String],
) -> Result<(String, Vec<u64>), WatchlistError> {
    let mut repo = repo_flag
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let mut numbers = Vec::new();
    for target in targets {
        if target.contains('/') {
            if repo
                .as_deref()
                .is_some_and(|have| !have.eq_ignore_ascii_case(target))
            {
                return Err(WatchlistError::InvalidInput(
                    "conflicting repository arguments".to_owned(),
                ));
            }
            repo = Some(target.clone());
            continue;
        }
        let number = target.parse::<u64>().map_err(|_| {
            WatchlistError::InvalidInput(format!("not a pull-request number: `{target}`"))
        })?;
        numbers.push(number);
    }
    let repo = repo.ok_or_else(|| {
        WatchlistError::InvalidInput(
            "repository required: pass --repo owner/name or owner/name before the numbers"
                .to_owned(),
        )
    })?;
    if owner_of_repo(&repo).is_none() {
        return Err(WatchlistError::InvalidInput(format!(
            "repo must be owner/name, got `{repo}`"
        )));
    }
    Ok((repo, numbers))
}

fn write_add(
    command: &'static str,
    json: bool,
    report: &AddReport,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if json {
        return write_envelope(
            command,
            serde_json::json!({
                "added": identities(&report.added),
                "refreshed": identities(&report.refreshed),
                "skipped": report.skipped.iter().map(|(repo, number, state)| {
                    serde_json::json!({ "repo": repo, "number": number, "state": state })
                }).collect::<Vec<_>>(),
            }),
            None,
            stdout,
        );
    }
    if add_report_is_empty(report) {
        writeln!(stdout, "No pull requests added.")?;
        return Ok(());
    }
    for (repo, number) in &report.added {
        writeln!(stdout, "added {repo}#{number}")?;
    }
    for (repo, number) in &report.refreshed {
        writeln!(stdout, "refreshed {repo}#{number}")?;
    }
    for (repo, number, state) in &report.skipped {
        writeln!(stdout, "skipped {repo}#{number} ({state})")?;
    }
    Ok(())
}

fn write_remove(json: bool, entry: &WatchEntry, stdout: &mut impl Write) -> io::Result<()> {
    if json {
        return write_envelope(
            "watchlist.remove",
            serde_json::json!({
                "removed": true,
                "repo": entry.repo,
                "number": entry.number,
            }),
            None,
            stdout,
        );
    }
    writeln!(
        stdout,
        "removed {}#{} (stack-mates, if any, stay on the watchlist)",
        entry.repo, entry.number
    )
}

fn write_list(json: bool, entries: &[WatchEntry], stdout: &mut impl Write) -> io::Result<()> {
    if json {
        return write_envelope(
            "watchlist.list",
            serde_json::json!({ "prs": entries.iter().map(entry_json).collect::<Vec<_>>() }),
            None,
            stdout,
        );
    }
    if entries.is_empty() {
        writeln!(stdout, "No watched pull requests.")?;
        return Ok(());
    }
    writeln!(
        stdout,
        "REPO\tNUMBER\tBRANCH\tSTACK\tSTATUS\tLAST_CHECKED\tFIX\tBLOCKERS"
    )?;
    for entry in entries {
        let stack = entry.stack_id.as_deref().unwrap_or("-");
        let blockers = if entry.residual_blockers.is_empty() {
            "-".to_owned()
        } else {
            entry.residual_blockers.join(",")
        };
        writeln!(
            stdout,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.repo,
            entry.number,
            entry.branch,
            stack,
            entry.status,
            entry.last_checked,
            entry.fix_count,
            blockers
        )?;
    }
    Ok(())
}

fn write_check(
    json: bool,
    command: &'static str,
    report: &CheckReport,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if json {
        return write_envelope(
            command,
            serde_json::json!({
                "checked": report.checked.iter().map(entry_json).collect::<Vec<_>>(),
                "pruned": report.pruned.iter().map(|(repo, number, state)| {
                    serde_json::json!({ "repo": repo, "number": number, "state": state })
                }).collect::<Vec<_>>(),
            }),
            None,
            stdout,
        );
    }
    if report.checked.is_empty() && report.pruned.is_empty() {
        writeln!(stdout, "No watched pull requests checked.")?;
        return Ok(());
    }
    for entry in &report.checked {
        writeln!(
            stdout,
            "checked {}#{} status={} blockers={}",
            entry.repo,
            entry.number,
            entry.status,
            if entry.residual_blockers.is_empty() {
                "-".to_owned()
            } else {
                entry.residual_blockers.join(",")
            }
        )?;
    }
    for (repo, number, state) in &report.pruned {
        writeln!(stdout, "pruned {repo}#{number} ({state})")?;
    }
    Ok(())
}

fn write_error(
    command: &'static str,
    json: bool,
    err: WatchlistError,
    stdout: &mut impl Write,
) -> io::Result<ExitCode> {
    let code = err.exit_code();
    if json {
        write_envelope(
            command,
            serde_json::json!({}),
            Some(ErrorData {
                code: err.code().to_owned(),
                message: err.to_string(),
            }),
            stdout,
        )?;
    }
    writeln!(io::stderr(), "writ: {err}")?;
    Ok(ExitCode::from(code))
}

fn write_envelope(
    command: &'static str,
    data: serde_json::Value,
    error: Option<ErrorData>,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let response = Response {
        ok: error.is_none(),
        schema_version: SCHEMA_VERSION,
        command,
        data,
        error,
    };
    serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

/// True when an add/import produced no added, refreshed, or skipped entries.
fn add_report_is_empty(report: &AddReport) -> bool {
    report.added.is_empty() && report.refreshed.is_empty() && report.skipped.is_empty()
}

fn identities(items: &[(String, u64)]) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|(repo, number)| serde_json::json!({ "repo": repo, "number": number }))
        .collect()
}

fn entry_json(entry: &WatchEntry) -> serde_json::Value {
    serde_json::json!({
        "repo": entry.repo,
        "number": entry.number,
        "branch": entry.branch,
        "status": entry.status,
        "last_checked": entry.last_checked,
        "fix_count": entry.fix_count,
        "residual_blockers": entry.residual_blockers,
        "stack_id": entry.stack_id,
        "stack_type": entry.stack_type,
        "stack_position": entry.stack_position,
        "base": entry.base,
        "title": entry.title,
        "added_at": entry.added_at,
        "check_count": entry.check_count,
        "url": entry.url,
        "kind": entry.kind,
    })
}

impl WatchlistKindArg {
    fn into_kind(self) -> WatchKind {
        match self {
            Self::PrBabysit => WatchKind::PrBabysit,
            Self::IssueToPr => WatchKind::IssueToPr,
        }
    }
}

#[cfg(test)]
mod cli_unit_tests {
    use super::{WatchlistAction, WatchlistKindArg, run};
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!(
                    "writ-watchlist-cli-unit-{}-{id}",
                    std::process::id()
                ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_state() -> (Scratch, PathBuf) {
        let scratch = Scratch::new();
        let path = scratch.0.join("watchlist.json");
        fs::write(
            &path,
            r#"{
              "version": 1,
              "prs": [{
                "repo": "acme/widgets",
                "number": 7,
                "branch": "feat/a",
                "status": "pending",
                "last_checked": "2026-01-01T00:00:00Z",
                "fix_count": 0,
                "residual_blockers": []
              }],
              "groups": {}
            }"#,
        )
        .unwrap();
        (scratch, path)
    }

    #[test]
    fn run_list_human_and_json_use_state_override() {
        let (_scratch, path) = sample_state();
        let mut human = Cursor::new(Vec::new());
        let code = run(
            WatchlistAction::List {
                state: Some(path.clone()),
                repo: None,
                owner: None,
            },
            false,
            &mut human,
        )
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let text = String::from_utf8(human.into_inner()).unwrap();
        assert!(text.contains("acme/widgets"));

        let mut json_out = Cursor::new(Vec::new());
        run(
            WatchlistAction::List {
                state: Some(path),
                repo: None,
                owner: None,
            },
            true,
            &mut json_out,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&json_out.into_inner()).unwrap();
        assert_eq!(payload["command"], "watchlist.list");
    }

    #[test]
    fn run_add_without_repo_is_invalid_input() {
        let (_scratch, path) = sample_state();
        let mut out = Cursor::new(Vec::new());
        let code = run(
            WatchlistAction::Add {
                state: Some(path),
                repo: None,
                reset: false,
                kind: WatchlistKindArg::PrBabysit,
                targets: vec!["7".to_owned()],
            },
            true,
            &mut out,
        )
        .unwrap();
        assert_eq!(code, ExitCode::from(1));
        let payload: serde_json::Value = serde_json::from_slice(&out.into_inner()).unwrap();
        assert_eq!(payload["error"]["code"], "INVALID_INPUT");
    }

    #[test]
    fn run_remove_human_prints_stack_mate_note() {
        let (_scratch, path) = sample_state();
        let mut out = Cursor::new(Vec::new());
        run(
            WatchlistAction::Remove {
                state: Some(path),
                repo: Some("acme/widgets".to_owned()),
                number: None,
                targets: vec!["7".to_owned()],
            },
            false,
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(text.contains("stack-mates"));
    }
}
