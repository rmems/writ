//! `writ watchlist` CLI: collaboration view over `leases.db`.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Subcommand;
use writ_core::contract::{ErrorData, Response, SCHEMA_VERSION};
use writ_core::lease::LeaseStore;
use writ_core::owners::OwnerAllowlist;
use writ_core::paths::lease_store_path;
use writ_core::watchlist::{GhPrProbe, WatchQuery, WatchlistData, load_view};

#[derive(Debug, Subcommand)]
pub enum WatchlistAction {
    /// Show registered jobs from the lease store (no GitHub probes).
    List {
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        job: Option<String>,
        #[arg(long)]
        include_released: bool,
    },
    /// Refresh GitHub PR/check overlay for matching jobs. Does not persist.
    Check {
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        job: Option<String>,
        #[arg(long)]
        include_released: bool,
    },
    /// Same as `check` over the full filtered set.
    CheckAll {
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        include_released: bool,
    },
    /// Watchlist does not persist a second store. Register a checkout instead.
    Add,
    /// Watchlist does not persist a second store. Unregister a checkout instead.
    Remove,
}

pub fn run(
    action: WatchlistAction,
    allowlist: &OwnerAllowlist,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    match action {
        WatchlistAction::Add => persist_hint("add", json, stdout),
        WatchlistAction::Remove => persist_hint("remove", json, stdout),
        WatchlistAction::List {
            owner,
            repo,
            job,
            include_released,
        } => render_view(
            WatchQuery {
                owner,
                repo,
                job_id: job,
                include_released,
                probe_github: false,
            },
            allowlist,
            json,
            "cli.watchlist.list",
            stdout,
        ),
        WatchlistAction::Check {
            owner,
            repo,
            job,
            include_released,
        } => render_view(
            WatchQuery {
                owner,
                repo,
                job_id: job,
                include_released,
                probe_github: true,
            },
            allowlist,
            json,
            "cli.watchlist.check",
            stdout,
        ),
        WatchlistAction::CheckAll {
            owner,
            repo,
            include_released,
        } => render_view(
            WatchQuery {
                owner,
                repo,
                job_id: None,
                include_released,
                probe_github: true,
            },
            allowlist,
            json,
            "cli.watchlist.check_all",
            stdout,
        ),
    }
}

fn persist_hint(
    verb: &'static str,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let command = match verb {
        "add" => "cli.watchlist.add",
        _ => "cli.watchlist.remove",
    };
    let hint = "Watchlist is a view over leases.db (RM-825), not a second store. \
                Use `writ worktree register` / `unregister` to change ownership records.";
    if json {
        let response = Response {
            ok: true,
            schema_version: SCHEMA_VERSION,
            command,
            data: serde_json::json!({
                "persisted": false,
                "hint": hint,
            }),
            error: None::<ErrorData>,
        };
        serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
    } else {
        writeln!(stdout, "{hint}")?;
    }
    Ok(ExitCode::SUCCESS)
}

fn render_view(
    query: WatchQuery,
    allowlist: &OwnerAllowlist,
    json: bool,
    command: &'static str,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let store = LeaseStore::open(lease_store_path())?;
    let probe = GhPrProbe::new(allowlist.clone());
    let github = query.probe_github.then_some(&probe as _);
    let data = load_view(&store, &query, github)?;
    write_output(json, command, &data, stdout)?;
    Ok(ExitCode::SUCCESS)
}

fn write_output(
    json: bool,
    command: &'static str,
    data: &WatchlistData,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if json {
        let response = Response::success(command, data);
        serde_json::to_writer(&mut *stdout, &response)?;
        stdout.write_all(b"\n")?;
        return Ok(());
    }
    if data.entries.is_empty() {
        writeln!(
            stdout,
            "No registered jobs. Harnesses own checkouts; register with `writ worktree register`."
        )?;
        return Ok(());
    }
    writeln!(
        stdout,
        "job\towner/repo\tbranch\tcollab\trecovery\tgithub\tblockers"
    )?;
    for entry in &data.entries {
        writeln!(
            stdout,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.job_id,
            entry.repo,
            entry.branch,
            entry.collab_status,
            entry.recovery_status,
            github_cell(entry.github.as_ref()),
            blockers_cell(&entry.residual_blockers)
        )?;
    }
    Ok(())
}

fn github_cell(github: Option<&writ_core::watchlist::GithubState>) -> String {
    match github {
        None => "-".to_owned(),
        Some(gh) if gh.number == 0 => gh.check_status.clone(),
        Some(gh) => format!("#{}:{}", gh.number, gh.check_status),
    }
}

fn blockers_cell(blockers: &[String]) -> String {
    if blockers.is_empty() {
        "-".to_owned()
    } else {
        blockers.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use writ_core::owners::OwnerAllowlist;

    #[test]
    fn add_does_not_persist() {
        let mut out = Vec::new();
        let code = persist_hint("add", true, &mut out).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let body = String::from_utf8(out).unwrap();
        assert!(body.contains("\"persisted\":false"));
        assert!(body.contains("cli.watchlist.add"));
    }

    #[test]
    fn human_empty_mentions_register() {
        let mut out = Vec::new();
        write_output(
            false,
            "cli.watchlist.list",
            &WatchlistData::empty(false, false),
            &mut out,
        )
        .unwrap();
        let body = String::from_utf8(out).unwrap();
        assert!(body.contains("writ worktree register"));
    }

    #[test]
    fn allowlist_type_is_available_for_check() {
        let list = OwnerAllowlist::parse("acme");
        assert!(!list.is_empty());
    }
}
