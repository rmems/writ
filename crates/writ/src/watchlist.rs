//! `writ watchlist` CLI: collaboration view over `leases.db`.

use std::io::{self, Write};
use std::path::Path;
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

/// Execute a watchlist action and write its human-readable or JSON response.
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
            } => WatchQuery {
                owner: owner.clone(),
                repo: repo.clone(),
                job_id: job.clone(),
                include_released: *include_released,
                probe_github: false,
            },
            WatchlistAction::Check {
                owner,
                repo,
                job,
                include_released,
            } => WatchQuery {
                owner: owner.clone(),
                repo: repo.clone(),
                job_id: job.clone(),
                include_released: *include_released,
                probe_github: true,
            },
            WatchlistAction::CheckAll {
                owner,
                repo,
                include_released,
            } => WatchQuery {
                owner: owner.clone(),
                repo: repo.clone(),
                job_id: None,
                include_released: *include_released,
                probe_github: true,
            },
            WatchlistAction::Add | WatchlistAction::Remove => unreachable!(),
        }
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
    let path = lease_store_path();
    let query = request.query();
    // A view never creates the store: a missing leases.db is an empty view.
    let data = if store_exists(&path)? {
        let store = LeaseStore::open_read_only(&path)?;
        let probe = GhPrProbe::new(request.allowlist.clone());
        let github = query.probe_github.then_some(&probe as _);
        load_view(&store, &query, github, request.allowlist)?
    } else {
        WatchlistData::empty(query.probe_github, false)
    };
    write_output(request.json, request.command(), &data, stdout)?;
    Ok(ExitCode::SUCCESS)
}

fn store_exists(path: &Path) -> io::Result<bool> {
    if path.try_exists()? {
        return Ok(true);
    }
    if path.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("lease store path is a dangling symlink: {}", path.display()),
        ));
    }
    validate_store_ancestors(path)?;
    Ok(false)
}

fn validate_store_ancestors(path: &Path) -> io::Result<()> {
    // Windows can report NotFound when an ancestor is a regular file. Only
    // treat the store as absent after reaching an existing directory. Inspect
    // links first so a dangling ancestor cannot masquerade as a missing path.
    for ancestor in path.ancestors().skip(1) {
        match ancestor.symlink_metadata() {
            Ok(metadata) => {
                let metadata = if metadata.is_symlink() {
                    ancestor.metadata()?
                } else {
                    metadata
                };
                if metadata.is_dir() {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!(
                        "lease store ancestor is not a directory: {}",
                        ancestor.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use writ_core::owners::OwnerAllowlist;
    use writ_core::watchlist::{
        CollabStatus, CoordOverlay, RecoveryStatus, WatchEntry, WatchlistData,
    };

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "writ-watchlist-ancestors-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_store_ancestor_validation_rejects_regular_files() {
        let root = TestDir::new();
        let file = root.0.join("file");
        std::fs::write(&file, b"preserve me").unwrap();
        // Exercise the ancestor contract directly: Unix can reject these paths
        // in the initial stat, while Windows may first report them as absent.
        for path in [file.join("leases.db"), file.join("nested/leases.db")] {
            assert_eq!(
                validate_store_ancestors(&path).unwrap_err().kind(),
                io::ErrorKind::NotADirectory
            );
        }
        assert_eq!(std::fs::read(file).unwrap(), b"preserve me");
    }

    #[test]
    fn missing_store_ancestor_validation_accepts_absent_paths_without_creating_them() {
        let root = TestDir::new();
        let relative = format!("writ-absent-store-{}.db", std::process::id());
        assert!(!Path::new(&relative).exists());
        for path in [
            root.0.join("leases.db"),
            root.0.join("missing/nested/leases.db"),
            std::path::PathBuf::from(&relative),
        ] {
            validate_store_ancestors(&path).unwrap();
            assert!(!path.exists());
        }
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }

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
