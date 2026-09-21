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
        other => render_view(
            ViewRequest {
                action: other,
                allowlist,
                json,
            },
            stdout,
        ),
    }
}

struct ViewRequest<'a> {
    action: WatchlistAction,
    allowlist: &'a OwnerAllowlist,
    json: bool,
}

impl ViewRequest<'_> {
    fn command(&self) -> &'static str {
        match self.action {
            WatchlistAction::List { .. } => "cli.watchlist.list",
            WatchlistAction::Check { .. } => "cli.watchlist.check",
            WatchlistAction::CheckAll { .. } => "cli.watchlist.check_all",
            WatchlistAction::Add | WatchlistAction::Remove => unreachable!(),
        }
    }

    fn query(&self) -> WatchQuery {
        match &self.action {
            WatchlistAction::List {
                owner,
                repo,
                job,
                include_released,
            } => filtered_query(owner, repo, job.clone(), *include_released, false),
            WatchlistAction::Check {
                owner,
                repo,
                job,
                include_released,
            } => filtered_query(owner, repo, job.clone(), *include_released, true),
            WatchlistAction::CheckAll {
                owner,
                repo,
                include_released,
            } => filtered_query(owner, repo, None, *include_released, true),
            WatchlistAction::Add | WatchlistAction::Remove => unreachable!(),
        }
    }
}

fn filtered_query(
    owner: &Option<String>,
    repo: &Option<String>,
    job_id: Option<String>,
    include_released: bool,
    probe_github: bool,
) -> WatchQuery {
    WatchQuery {
        owner: owner.clone(),
        repo: repo.clone(),
        job_id,
        include_released,
        probe_github,
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
    request: ViewRequest<'_>,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let store = LeaseStore::open(lease_store_path())?;
    let probe = GhPrProbe::new(request.allowlist.clone());
    let query = request.query();
    let github = query.probe_github.then_some(&probe as _);
    let data = load_view(&store, &query, github)?;
    write_output(request.json, request.command(), &data, stdout)?;
    Ok(ExitCode::SUCCESS)
}

fn write_output(
    json: bool,
    command: &'static str,
    data: &WatchlistData,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if json {
        return write_json(command, data, stdout);
    }
    if data.entries.is_empty() {
        writeln!(
            stdout,
            "No registered jobs. Harnesses own checkouts; register with `writ worktree register`."
        )?;
        return Ok(());
    }
    write_table(data, stdout)
}

fn write_json(
    command: &'static str,
    data: &WatchlistData,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let response = Response::success(command, data);
    serde_json::to_writer(&mut *stdout, &response)?;
    stdout.write_all(b"\n")
}

fn write_table(data: &WatchlistData, stdout: &mut impl Write) -> io::Result<()> {
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
    use writ_core::watchlist::{
        CollabStatus, CoordOverlay, RecoveryStatus, WatchEntry, WatchlistData,
    };

    fn sample_entry() -> WatchEntry {
        WatchEntry {
            job_id: "job-1".to_owned(),
            owner: "acme".to_owned(),
            repo: "acme/sample".to_owned(),
            branch: "hive/job-1".to_owned(),
            worktree_path: "/tmp/wt".to_owned(),
            lease_mode: "WRITER_LOCKED".to_owned(),
            collab_status: CollabStatus::Running,
            recovery_status: RecoveryStatus::Live,
            coord: CoordOverlay {
                agent_id: None,
                session_id: None,
                intent: None,
                owner_generation: None,
                paused: false,
                declared_paths: Vec::new(),
                overlaps: Vec::new(),
                waiting_on: None,
            },
            github: None,
            residual_blockers: vec!["recovery:stale_heartbeat".to_owned()],
        }
    }

    fn inspect(action: WatchlistAction) -> (&'static str, WatchQuery) {
        let allowlist = OwnerAllowlist::parse("acme");
        let request = ViewRequest {
            action,
            allowlist: &allowlist,
            json: false,
        };
        (request.command(), request.query())
    }

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
    fn query_shapes() {
        let (cmd, query) = inspect(WatchlistAction::List {
            owner: Some("acme".to_owned()),
            repo: Some("sample".to_owned()),
            job: Some("job-1".to_owned()),
            include_released: true,
        });
        assert_eq!(cmd, "cli.watchlist.list");
        assert!(!query.probe_github);
        assert!(query.include_released);

        let (cmd, query) = inspect(WatchlistAction::Check {
            owner: None,
            repo: None,
            job: Some("job-1".to_owned()),
            include_released: false,
        });
        assert_eq!(cmd, "cli.watchlist.check");
        assert!(query.probe_github);

        let (cmd, query) = inspect(WatchlistAction::CheckAll {
            owner: Some("acme".to_owned()),
            repo: None,
            include_released: false,
        });
        assert_eq!(cmd, "cli.watchlist.check_all");
        assert!(query.job_id.is_none());
    }

    #[test]
    fn write_output_table_includes_job_and_blockers() {
        let mut out = Vec::new();
        let data = WatchlistData {
            entries: vec![sample_entry()],
            coord_available: false,
            github_probed: false,
        };
        write_output(false, "cli.watchlist.list", &data, &mut out).unwrap();
        let body = String::from_utf8(out).unwrap();
        assert!(body.contains("job-1"));
        assert!(body.contains("running"));
        assert!(body.contains("recovery:stale_heartbeat"));
        assert!(body.contains("\t-\t"));
    }

    #[test]
    fn github_cell_formats_numbered_and_unknown() {
        assert_eq!(github_cell(None), "-");
        let mut gh = writ_core::watchlist::GithubState {
            number: 0,
            title: String::new(),
            url: String::new(),
            branch: "b".to_owned(),
            base: String::new(),
            state: "UNKNOWN".to_owned(),
            check_status: "unknown".to_owned(),
            mergeable: None,
            is_draft: false,
            residual_blockers: Vec::new(),
        };
        assert_eq!(github_cell(Some(&gh)), "unknown");
        gh.number = 12;
        gh.check_status = "healthy".to_owned();
        assert_eq!(github_cell(Some(&gh)), "#12:healthy");
    }

    #[test]
    fn persist_hint_remove_human() {
        let mut out = Vec::new();
        persist_hint("remove", false, &mut out).unwrap();
        let body = String::from_utf8(out).unwrap();
        assert!(body.contains("leases.db"));
    }

    #[test]
    fn write_json_envelope() {
        let mut out = Vec::new();
        write_output(
            true,
            "cli.watchlist.check_all",
            &WatchlistData::empty(true, false),
            &mut out,
        )
        .unwrap();
        let body = String::from_utf8(out).unwrap();
        assert!(body.contains("cli.watchlist.check_all"));
        assert!(body.contains("\"github_probed\":true"));
    }
}
