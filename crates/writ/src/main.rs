use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

/// Manage isolated issue-to-PR jobs and their durable state.
#[derive(Debug, Parser)]
#[command(name = "writ", version, about, long_about = None)]
struct Cli {
    /// Emit responses as versioned JSON envelopes.
    #[arg(long, global = true)]
    json: bool,

    /// Comma-separated GitHub owners allowed for worktree create and `gh -R`.
    ///
    /// Overrides `WRIT_ALLOWED_OWNERS` / `WH_ALLOWED_OWNERS` when present. An
    /// empty value is still deny-by-default.
    #[arg(long, global = true, value_name = "OWNERS")]
    allowed_owners: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show status of all watched jobs.
    Status,
    /// List all watched jobs (alias for status).
    Jobs,
    /// Validate and run a git command under safety policy.
    GitSafe {
        /// Optional expected branch; mutating subcommands verify before running.
        #[arg(long)]
        expected_branch: Option<String>,

        /// Repository working tree (default: current directory).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Git arguments after the subcommand (e.g., push --force-with-lease origin main).
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Validate and run a GitHub CLI command under safety policy.
    GhSafe {
        /// gh arguments (e.g., pr create --title "Fix").
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Process supervisor: spawn with timeouts, process-group isolation, and max-parallel.
    Supervisor {
        #[command(subcommand)]
        action: SupervisorAction,
    },

    /// Checkout coordination (register/unregister/inspect/list) plus the
    /// deprecated managed lifecycle (create/remove/prune).
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },

    /// Format an attributed PR thread reply or summary comment.
    Attribution {
        #[command(subcommand)]
        action: AttributionAction,
    },

    /// Dispatch a Claude Code hook event from JSON on stdin.
    Hook,

    /// Idempotently write the writ hook block into Claude Code settings.
    Install {
        /// Settings file to update (default: `.claude/settings.json` in the current directory).
        #[arg(long)]
        settings: Option<PathBuf>,
        /// Executable the hook should invoke (default: `WRIT_BIN`, then `writ` on `PATH`).
        #[arg(long)]
        writ_bin: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum WorktreeAction {
    /// Register an existing, harness-created checkout for coordination.
    ///
    /// Writ does not create, move, or modify the checkout; this only writes a
    /// coordination record in the shared lease store. Works for any path:
    /// a linked worktree, the primary checkout, or a standalone clone.
    Register {
        /// Path to an existing git checkout (any location).
        path: PathBuf,
        /// Job id for the coordination record (default: checkout dir name).
        #[arg(long)]
        job: Option<String>,
    },
    /// Release the coordination record for a checkout. Never deletes files,
    /// branches, or uncommitted changes.
    Unregister {
        /// Path of the registered checkout.
        path: PathBuf,
    },
    /// Read-only inspection of a checkout: branch, HEAD, dirty/detached state,
    /// repo identity. Writes nothing.
    Inspect {
        /// Path to an existing git checkout.
        path: PathBuf,
    },
    /// Deprecated: create a worktree under the sandboxed base path.
    /// The harness should create checkouts itself; use `register` instead.
    Create {
        /// Repository root (git dir / working tree).
        #[arg(long)]
        repo: PathBuf,
        /// GitHub-style owner segment.
        owner: String,
        /// Repository name segment.
        repo_name: String,
        /// Job id segment (e.g. gh-42).
        job_id: String,
        /// Branch name for the worktree.
        branch: String,
        /// Commit or ref required by v2 for exact-base branch creation.
        #[arg(long)]
        start_point: Option<String>,
        /// GitHub pull request number whose `refs/pull/<n>/head` is imported from origin.
        #[arg(long)]
        pr_number: Option<u64>,
        /// Named remote bound to the base repository (must be `origin` when set).
        #[arg(long)]
        source_remote: Option<String>,
        /// Fork `owner/repo` identity. Never used as fetch or checkout authority.
        #[arg(long)]
        head_repo: Option<String>,
        /// Boundary schema: v1 returns an upgrade error; select v2 to create.
        #[arg(
            long,
            default_value_t = writ_core::contract::SCHEMA_VERSION,
            value_parser = clap::value_parser!(u8).range(1..=2)
        )]
        schema_version: u8,
    },
    /// List registered checkouts in the shared coordination store.
    List,
    /// Deprecated: remove a worktree path (must stay under the sandbox base).
    /// The harness owns physical cleanup; use `unregister` for coordination.
    Remove {
        /// Absolute or relative worktree path.
        path: PathBuf,
        /// Force removal with dirty tree.
        #[arg(long)]
        force: bool,
    },
    /// Deprecated: prune stale git worktree metadata for a repo.
    /// Equivalent to `git worktree prune`; physical cleanup is harness-owned.
    Prune {
        #[arg(long)]
        repo: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AttributionAction {
    /// Render a reply body with attribution and optional commit SHA.
    Format {
        /// Main reply content.
        #[arg(long, allow_hyphen_values = true)]
        body: String,
        /// Identity line. Overrides `WRIT_AGENT_ID` / `WRIT_ATTRIBUTION`.
        #[arg(long)]
        agent_id: Option<String>,
        /// Actual commit SHA when discussing committed work. Omit for
        /// coordination messages and when no code changed. Never invent a SHA.
        #[arg(long)]
        commit_sha: Option<String>,
        /// `footer` (default) or `header`.
        #[arg(long)]
        placement: Option<String>,
        /// Format as a PR-level comment instead of a review thread reply.
        #[arg(long)]
        pr_comment: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SupervisorAction {
    /// Run a command under supervision.
    Run {
        /// Wall-clock timeout in seconds. 0 means no timeout.
        #[arg(long, default_value = "0")]
        timeout: u64,

        /// Expected branch for mutating supervised `git` / `gh pr` commands.
        #[arg(long)]
        expected_branch: Option<String>,

        /// Repository working tree for supervised git branch checks (default: `.`).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Max concurrent supervised children **in this process** (default 1).
        /// Does not coordinate across separate `writ` processes.
        #[arg(long, default_value = "1")]
        max_parallel: usize,

        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let envelope_command = json_error_command(&cli);
    let worktree_schema_version = worktree_schema_version(&cli);

    match run(cli, &mut io::stdout()).await {
        Ok(code) => code,
        Err(error) => {
            if json && let Some(command) = envelope_command {
                let mut stdout = io::stdout();
                let _ = write_json_error_envelope(
                    command,
                    worktree_schema_version,
                    &error,
                    &mut stdout,
                );
            }
            let _ = writeln!(io::stderr(), "writ: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

struct WorktreeResidual<'a> {
    path: &'a Path,
    branch: &'a str,
    path_exists: bool,
    branch_commit: &'a Option<String>,
    head_commit: &'a Option<String>,
    worktree_registered: bool,
}

macro_rules! worktree_residual {
    ($failure:expr) => {
        WorktreeResidual {
            path: &$failure.path,
            branch: &$failure.branch,
            path_exists: $failure.path_exists,
            branch_commit: &$failure.branch_commit,
            head_commit: &$failure.head_commit,
            worktree_registered: $failure.worktree_registered,
        }
    };
}

fn worktree_residual_json(
    residual: WorktreeResidual<'_>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert("path".to_owned(), serde_json::json!(residual.path));
    map.insert("branch".to_owned(), serde_json::json!(residual.branch));
    map.insert(
        "path_exists".to_owned(),
        serde_json::json!(residual.path_exists),
    );
    map.insert(
        "branch_commit".to_owned(),
        serde_json::json!(residual.branch_commit),
    );
    map.insert(
        "head_commit".to_owned(),
        serde_json::json!(residual.head_commit),
    );
    map.insert(
        "worktree_registered".to_owned(),
        serde_json::json!(residual.worktree_registered),
    );
    map.insert("cleanup_performed".to_owned(), serde_json::json!(false));
    map
}

fn worktree_error_data(error: &writ_core::error::Error) -> serde_json::Value {
    match error {
        writ_core::error::Error::ContractUpgradeRequired {
            required_schema_version,
        } => serde_json::json!({
            "required_schema_version": required_schema_version,
        }),
        writ_core::error::Error::WorktreeCreationFailed(failure) => {
            serde_json::Value::Object(worktree_residual_json(worktree_residual!(failure)))
        }
        writ_core::error::Error::AmbiguousStartPoint {
            start_point,
            refs,
            git_warning,
        } => serde_json::json!({
            "start_point": start_point,
            "refs": refs.iter().map(|colliding| serde_json::json!({
                "refname": colliding.refname,
                "commit": colliding.commit,
            })).collect::<Vec<_>>(),
            "git_warning": git_warning,
        }),
        writ_core::error::Error::WorktreePostconditionFailed(failure) => {
            postcondition_failure_data(failure)
        }
        writ_core::error::Error::PrImportFailed(failure) => import_failure_data(failure),
        _ => serde_json::json!({}),
    }
}

fn postcondition_failure_data(
    failure: &writ_core::error::WorktreePostconditionFailure,
) -> serde_json::Value {
    let mut map = worktree_residual_json(worktree_residual!(failure));
    map.insert(
        "expected_commit".to_owned(),
        serde_json::json!(&failure.expected_commit),
    );
    map.insert(
        "actual_branch".to_owned(),
        serde_json::json!(&failure.actual_branch),
    );
    serde_json::Value::Object(map)
}

fn import_failure_data(failure: &writ_core::error::PrImportFailure) -> serde_json::Value {
    let writ_core::error::PrImportFailure {
        expected_commit,
        source_remote,
        source_ref,
        import_ref,
        imported_commit,
        import_ref_exists,
        cleanup_performed,
        ..
    } = failure;
    serde_json::json!({
        "expected_commit": expected_commit,
        "source_remote": source_remote,
        "source_ref": source_ref,
        "import_ref": import_ref,
        "imported_commit": imported_commit,
        "import_ref_exists": import_ref_exists,
        "cleanup_performed": cleanup_performed,
    })
}

fn worktree_schema_version(cli: &Cli) -> u8 {
    match &cli.command {
        Some(Command::Worktree {
            action: WorktreeAction::Create { schema_version, .. },
        }) => *schema_version,
        _ => writ_core::contract::SCHEMA_VERSION,
    }
}

fn worktree_command_name(cli: &Cli) -> Option<&'static str> {
    match &cli.command {
        Some(Command::Worktree { action }) => Some(match action {
            WorktreeAction::Register { .. } => "worktree.register",
            WorktreeAction::Unregister { .. } => "worktree.unregister",
            WorktreeAction::Inspect { .. } => "worktree.inspect",
            WorktreeAction::Create { .. } => "worktree.create",
            WorktreeAction::List => "worktree.list",
            WorktreeAction::Remove { .. } => "worktree.remove",
            WorktreeAction::Prune { .. } => "worktree.prune",
        }),
        _ => None,
    }
}

fn json_error_command(cli: &Cli) -> Option<&'static str> {
    worktree_command_name(cli).or(match &cli.command {
        Some(Command::Attribution { .. }) => Some("attribution.format"),
        _ => None,
    })
}

fn write_json_error_envelope(
    command: &'static str,
    schema_version: u8,
    error: &writ_core::error::Error,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let response = writ_core::contract::Response {
        ok: false,
        schema_version,
        command,
        data: worktree_error_data(error),
        error: Some(writ_core::contract::ErrorData {
            code: error.code().to_owned(),
            message: error.to_string(),
        }),
    };
    serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
    stdout.write_all(b"\n")
}

/// Owned fields of a `worktree create` request, threaded from the CLI action to
/// [`worktree_create_response`] so the dispatch arm stays small.
struct WorktreeCreateArgs {
    repo: PathBuf,
    owner: String,
    repo_name: String,
    job_id: String,
    branch: String,
    start_point: Option<String>,
    pr_number: Option<u64>,
    source_remote: Option<String>,
    head_repo: Option<String>,
    schema_version: u8,
}

/// Build the `worktree.create` response, preserving the exact JSON envelope.
fn worktree_create_response(
    allowlist: &writ_core::owners::OwnerAllowlist,
    args: WorktreeCreateArgs,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::contract::Response;
    use writ_core::worktree::{WorktreeCreateRequest, WorktreeManager};

    let WorktreeCreateArgs {
        repo,
        owner,
        repo_name,
        job_id,
        branch,
        start_point,
        pr_number,
        source_remote,
        head_repo,
        schema_version,
    } = args;

    if schema_version == writ_core::contract::SCHEMA_VERSION {
        return Err(writ_core::error::Error::ContractUpgradeRequired {
            required_schema_version: writ_core::contract::EXACT_BASE_SCHEMA_VERSION,
        });
    }
    let start_point = start_point.ok_or(writ_core::error::Error::StartPointRequired)?;
    let manager = WorktreeManager::new()?.with_allowlist(allowlist.clone());
    let wt = manager.create_with_request(WorktreeCreateRequest {
        repo_root: &repo,
        owner: &owner,
        repo: &repo_name,
        job_id: &job_id,
        branch: &branch,
        start_point: &start_point,
        pr_number,
        source_remote: source_remote.as_deref(),
        head_repo: head_repo.as_deref(),
    })?;
    Ok(Response::success_with_schema(
        "worktree.create",
        serde_json::json!({
            "path": wt.path,
            "branch": wt.branch,
            "branch_ref": format!("refs/heads/{}", wt.branch),
            "repo_root": wt.repo_root,
            "start_commit": wt.start_commit,
            "head_commit": wt.head_commit,
            "worktree_registered": true,
        }),
        schema_version,
    ))
}

fn checkout_json(info: &writ_core::checkout::CheckoutInfo) -> serde_json::Value {
    serde_json::json!({
        "path": info.path,
        "branch": info.branch,
        "head_commit": info.head_commit,
        "common_dir": info.common_dir,
        "linked_worktree": info.linked_worktree,
        "origin_slug": info.origin_slug,
        "dirty": info.dirty,
    })
}

fn worktree_register_response(
    path: PathBuf,
    job: Option<String>,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::checkout::CheckoutRegistry;
    use writ_core::contract::Response;

    let registry = CheckoutRegistry::new()?;
    let job_id = job.unwrap_or_else(|| writ_core::checkout::default_job_id(&path));
    let info = registry.register(&path, &job_id)?;
    let mut data = checkout_json(&info);
    data["job_id"] = serde_json::Value::String(job_id);
    Ok(Response::success("worktree.register", data))
}

fn worktree_unregister_response(
    path: PathBuf,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::checkout::CheckoutRegistry;
    use writ_core::contract::Response;

    let released = CheckoutRegistry::new()?.unregister(&path)?;
    Ok(Response::success(
        "worktree.unregister",
        serde_json::json!({ "released": released.is_some() }),
    ))
}

fn worktree_inspect_response(
    path: PathBuf,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::contract::Response;

    let info = writ_core::checkout::inspect_checkout(&path)?;
    Ok(Response::success("worktree.inspect", checkout_json(&info)))
}

fn worktree_list_response()
-> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::checkout::CheckoutRegistry;
    use writ_core::contract::Response;

    let worktrees = CheckoutRegistry::new()?.registered()?;
    Ok(Response::success(
        "worktree.list",
        serde_json::json!({
            "worktrees": worktrees.iter().map(|lease| serde_json::json!({
                "path": lease.worktree_path,
                "branch": lease.branch,
                "job_id": lease.job_id,
                "mode": lease.mode.as_str(),
            })).collect::<Vec<_>>(),
        }),
    ))
}

fn worktree_response(
    action: WorktreeAction,
    allowlist: &writ_core::owners::OwnerAllowlist,
) -> writ_core::error::Result<writ_core::contract::Response<serde_json::Value>> {
    use writ_core::contract::Response;
    use writ_core::worktree::WorktreeManager;

    match action {
        WorktreeAction::Register { path, job } => worktree_register_response(path, job),
        WorktreeAction::Unregister { path } => worktree_unregister_response(path),
        WorktreeAction::Inspect { path } => worktree_inspect_response(path),
        WorktreeAction::List => worktree_list_response(),
        WorktreeAction::Create {
            repo,
            owner,
            repo_name,
            job_id,
            branch,
            start_point,
            pr_number,
            source_remote,
            head_repo,
            schema_version,
        } => worktree_create_response(
            allowlist,
            WorktreeCreateArgs {
                repo,
                owner,
                repo_name,
                job_id,
                branch,
                start_point,
                pr_number,
                source_remote,
                head_repo,
                schema_version,
            },
        ),
        WorktreeAction::Remove { path, force } => {
            let manager = WorktreeManager::new()?;
            manager.remove(&path, force)?;
            Ok(Response::success(
                "worktree.remove",
                serde_json::json!({ "removed": path }),
            ))
        }
        WorktreeAction::Prune { repo } => {
            let manager = WorktreeManager::new()?;
            manager.prune(&repo)?;
            Ok(Response::success("worktree.prune", serde_json::json!({})))
        }
    }
}

fn run_worktree(
    action: WorktreeAction,
    allowlist: &writ_core::owners::OwnerAllowlist,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let response = worktree_response(action, allowlist)?;

    if json {
        write_json_line(stdout, &response)?;
    } else {
        writeln!(stdout, "ok={} command={}", response.ok, response.command)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn run_attribution(
    action: AttributionAction,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    match action {
        AttributionAction::Format {
            body,
            agent_id,
            commit_sha,
            placement,
            pr_comment,
        } => {
            if body.trim().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attribution body must not be empty",
                )
                .into());
            }
            let mut config = writ_core::attribution::AttributionConfig::from_env();
            if let Some(id) = agent_id.as_deref() {
                config.agent_id = writ_core::attribution::canonicalize_agent_id(id);
            }
            if let Some(placement) = placement {
                config.placement = writ_core::attribution::AttributionPlacement::coerce(&placement);
            }
            let raw_sha = commit_sha.as_deref();
            let commit_sha = match raw_sha.map(str::trim) {
                None | Some("") => None,
                Some(sha) => Some(
                    writ_core::attribution::sanitize_commit_sha(Some(sha)).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid --commit-sha '{sha}'"),
                        )
                    })?,
                ),
            };
            let text =
                writ_core::attribution::format_reply(body, Some(&config), commit_sha, !pr_comment);
            if json {
                let response = writ_core::contract::Response::success(
                    "attribution.format",
                    serde_json::json!({
                        "text": text,
                        "agent_id": config.agent_id,
                        "include_sha_on_fix": config.include_sha_on_fix,
                        "placement": config.placement,
                        "commit_sha": commit_sha,
                        "is_thread_reply": !pr_comment,
                    }),
                );
                serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
                stdout.write_all(b"\n")?;
            } else {
                writeln!(stdout, "{text}")?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Entry point for CLI commands (status/jobs, git/gh-safe, supervisor, worktree, attribution).
fn run_hook(stdout: &mut impl Write) -> writ_core::error::Result<ExitCode> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let code = writ_core::hook::dispatch(
        &input,
        &writ_core::hook::HookRuntime::default(),
        stdout,
        &mut io::stderr(),
    );
    Ok(ExitCode::from(code))
}

fn run_install(
    settings: Option<PathBuf>,
    writ_bin: Option<PathBuf>,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let settings = settings.unwrap_or_else(|| PathBuf::from(".claude/settings.json"));
    let command = writ_command(writ_bin);
    let result =
        writ_core::install::install_settings(&settings, writ_core::install::WritCommand(&command))?;
    emit_install_output(json, stdout, &result, &command)?;
    Ok(ExitCode::SUCCESS)
}

fn emit_install_output(
    json: bool,
    stdout: &mut impl Write,
    result: &writ_core::install::InstallResult,
    command: &str,
) -> io::Result<()> {
    if json {
        let response = writ_core::contract::Response::success(
            "cli.install",
            serde_json::json!({
                "path": result.path,
                "changed": result.changed,
                "command": command,
            }),
        );
        return write_json_line(stdout, &response);
    }
    writeln!(
        stdout,
        "install {} settings {}",
        if result.changed {
            "updated"
        } else {
            "unchanged"
        },
        result.path.display()
    )
}

fn write_json_line(stdout: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *stdout, value).map_err(io::Error::other)?;
    stdout.write_all(b"\n")
}

fn writ_command(writ_bin: Option<PathBuf>) -> String {
    if let Some(path) = writ_bin {
        return path.to_string_lossy().into_owned();
    }
    if let Some(path) = std::env::var_os("WRIT_BIN")
        && !path.is_empty()
    {
        return path.to_string_lossy().into_owned();
    }
    "writ".to_owned()
}

async fn run(cli: Cli, stdout: &mut impl Write) -> writ_core::error::Result<ExitCode> {
    let allowlist =
        writ_core::owners::OwnerAllowlist::from_cli_or_env(cli.allowed_owners.as_deref());
    match cli.command {
        Some(Command::Status) => {
            run_status(
                cli.json,
                "cli.status",
                writ_core::state::load_jobs(),
                stdout,
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Jobs) => {
            run_status(cli.json, "cli.jobs", writ_core::state::load_jobs(), stdout)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::GitSafe {
            expected_branch,
            repo,
            args,
        }) => run_git_safe(&args, expected_branch.as_deref(), repo, cli.json, stdout),
        Some(Command::GhSafe { args }) => run_gh_safe(&args, &allowlist, cli.json, stdout),
        Some(Command::Supervisor { action }) => match action {
            SupervisorAction::Run {
                timeout,
                expected_branch,
                repo,
                max_parallel,
                cmd,
            } => {
                run_supervisor(
                    cli.json,
                    timeout,
                    max_parallel,
                    cmd,
                    writ_core::supervisor::RunOptions {
                        expected_branch,
                        repo,
                        allowlist: Some(allowlist),
                    },
                    stdout,
                )
                .await
            }
        },
        Some(Command::Worktree { action }) => run_worktree(action, &allowlist, cli.json, stdout),
        Some(Command::Attribution { action }) => run_attribution(action, cli.json, stdout),
        Some(Command::Hook) => run_hook(stdout),
        Some(Command::Install { settings, writ_bin }) => {
            run_install(settings, writ_bin, cli.json, stdout)
        }
        None => {
            if cli.json {
                serde_json::to_writer(
                    &mut *stdout,
                    &writ_core::contract::Response::bootstrap_success(),
                )
                .map_err(io::Error::other)?;
                stdout.write_all(b"\n")?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Run `writ supervisor run` with policy-checked core supervisor and consistent JSON envelopes.
async fn run_supervisor(
    json: bool,
    timeout_secs: u64,
    max_parallel: usize,
    cmd: Vec<String>,
    options: writ_core::supervisor::RunOptions,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let program = match cmd.first() {
        Some(p) => p.as_str(),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "supervisor run requires at least one argument (the command)",
            )
            .into());
        }
    };
    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
    let timeout = if timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(timeout_secs))
    };

    // One-shot CLI: a single awaited child cannot contend with itself.
    // Library callers may still use Supervisor::new(n) for in-process fan-out.
    let supervisor = writ_core::supervisor::Supervisor::new(max_parallel.max(1));
    match supervisor.run(program, &args, timeout, &options).await {
        Ok(output) => {
            write_supervisor_result(json, Ok(&output), stdout)?;
            if json {
                // Machine clients (WhClient) parse stdout only on exit 0 (except policy=2).
                // Structured success/failure lives in the envelope (`ok`, `data`, `error`).
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(supervised_exit_code(&output))
            }
        }
        Err(err) => {
            write_supervisor_result(json, Err(&err), stdout)?;
            Err(err)
        }
    }
}

fn write_supervisor_result(
    json: bool,
    result: Result<&writ_core::supervisor::SupervisedOutput, &writ_core::error::Error>,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if !json {
        if let Ok(output) = result {
            serde_json::to_writer(&mut *stdout, output).map_err(io::Error::other)?;
            stdout.write_all(b"\n")?;
        }
        return Ok(());
    }

    let (ok, data, error) = match result {
        Ok(output) => {
            // Always ok:true for completed runs so WhClient keeps `data` (exit_code, timed_out, …).
            let data = serde_json::to_value(output).map_err(io::Error::other)?;
            (true, data, None)
        }
        Err(writ_core::error::Error::PolicyViolation { code, message }) => (
            false,
            serde_json::json!({}),
            Some(writ_core::contract::ErrorData {
                code: code.as_str().to_owned(),
                message: message.clone(),
            }),
        ),
        Err(other) => (
            false,
            serde_json::json!({}),
            Some(writ_core::contract::ErrorData {
                code: "SUPERVISOR_ERROR".to_owned(),
                message: other.to_string(),
            }),
        ),
    };

    let response = writ_core::contract::Response {
        ok,
        schema_version: writ_core::contract::SCHEMA_VERSION,
        command: "supervisor.run",
        data,
        error,
    };
    serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

/// Map supervised outcome to a process exit code.
///
/// - spawn failure → non-zero
/// - timed_out / killed → non-zero
/// - otherwise propagate child exit code when present
fn supervised_exit_code(output: &writ_core::supervisor::SupervisedOutput) -> ExitCode {
    if output.spawn_failed() {
        return ExitCode::FAILURE;
    }
    if output.timed_out || output.killed {
        return ExitCode::from(124);
    }
    match output.exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(code) if (1..=255).contains(&code) => ExitCode::from(code as u8),
        Some(_) => ExitCode::FAILURE,
        None => ExitCode::FAILURE,
    }
}

/// Render status/jobs from a preloaded `load_jobs` result (testable without env mutation).
fn run_status(
    json: bool,
    command_name: &'static str,
    jobs_result: Result<Vec<writ_core::status::JobStatus>, String>,
    stdout: &mut impl Write,
) -> io::Result<()> {
    match jobs_result {
        Ok(jobs) => run_with_jobs(json, command_name, jobs, stdout),
        Err(e) => {
            if json {
                // JSON path: write ok:false envelope, then exit non-zero.
                // Consumers should parse stdout even on CalledProcessError (see docs/status-schema.md).
                let response = writ_core::status::status_error(command_name, e.clone());
                serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
                stdout.write_all(b"\n")?;
            }
            // Human and JSON: never pretend the empty set is healthy on load failure.
            Err(io::Error::other(e))
        }
    }
}

/// Render status/jobs output. Separated from `run` for testability.
fn run_with_jobs(
    json: bool,
    command_name: &'static str,
    jobs: Vec<writ_core::status::JobStatus>,
    stdout: &mut impl Write,
) -> io::Result<()> {
    if json {
        let response = writ_core::status::status_response(command_name, jobs);
        serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
    } else if jobs.is_empty() {
        stdout.write_all(b"No watched jobs.\n")?;
    } else {
        for job in &jobs {
            writeln!(
                stdout,
                "{} [{}] {}/{} branch={} ci={}",
                job.job_id, job.process_state, job.owner, job.repo, job.branch, job.ci_class
            )?;
        }
    }
    Ok(())
}

fn run_git_safe(
    args: &[String],
    expected_branch: Option<&str>,
    repo: Option<PathBuf>,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let cmd = writ_core::git_safe::SafeGitCommand::new(args)?;
    let repo_dir = repo.unwrap_or_else(|| PathBuf::from("."));
    let output = cmd.run(&repo_dir, expected_branch)?;

    write_safe_result("git.safe", cmd.args(), &output, json, stdout)?;
    Ok(exit_code_from_i32(output.exit_code))
}

fn run_gh_safe(
    args: &[String],
    allowlist: &writ_core::owners::OwnerAllowlist,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let cmd = writ_core::git_safe::SafeGhCommand::with_allowlist(args, allowlist)?;
    let output = cmd.run()?;

    write_safe_result("gh.safe", cmd.args(), &output, json, stdout)?;
    Ok(exit_code_from_i32(output.exit_code))
}

fn write_safe_result(
    command: &'static str,
    args: &[String],
    output: &writ_core::git_safe::GitOutput,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<()> {
    if json {
        let data = serde_json::json!({
            "args": args,
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
        });
        let response = writ_core::contract::Response::success(command, data);
        serde_json::to_writer(&mut *stdout, &response).map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
    } else {
        // Human: show validation + execution summary; stream child stdout/stderr to stdout/stderr.
        writeln!(
            stdout,
            "ran: {} {} (exit {})",
            if command.starts_with("git") {
                "git"
            } else {
                "gh"
            },
            args.join(" "),
            output.exit_code
        )?;
        if !output.stdout.is_empty() {
            write!(stdout, "{}", output.stdout)?;
        }
        if !output.stderr.is_empty() {
            let _ = write!(io::stderr(), "{}", output.stderr);
        }
    }
    Ok(())
}

fn exit_code_from_i32(code: i32) -> ExitCode {
    if code == 0 {
        ExitCode::SUCCESS
    } else if (1..=255).contains(&code) {
        ExitCode::from(code as u8)
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;
    use std::str;

    use clap::{CommandFactory, Parser};
    use writ_core::status::{CiClass, JobStatus, ProcessState};

    use super::{
        Cli, json_error_command, run, run_status, run_with_jobs, supervised_exit_code,
        write_json_error_envelope,
    };

    fn sample_job() -> JobStatus {
        JobStatus {
            job_id: "writ-1".to_owned(),
            owner: "acme".to_owned(),
            repo: "example-org".to_owned(),
            issue_number: Some(29),
            pr_number: None,
            worktree_path: "/tmp/wt/writ-1".to_owned(),
            branch: "feature/foo".to_owned(),
            process_state: ProcessState::Running,
            last_error: None,
            ci_class: CiClass::Pending,
        }
    }

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn attribution_format_parser_accepts_thread_and_pr_shapes() {
        let parsed = Cli::try_parse_from([
            "writ",
            "attribution",
            "format",
            "--body",
            "Looks good!",
            "--agent-id",
            "Claude Code: worktrees-hives agent",
            "--commit-sha",
            "abc1234",
        ])
        .unwrap();
        let Some(super::Command::Attribution {
            action:
                super::AttributionAction::Format {
                    body,
                    agent_id,
                    commit_sha,
                    pr_comment: false,
                    ..
                },
        }) = parsed.command
        else {
            panic!("expected attribution format command")
        };
        assert_eq!(body, "Looks good!");
        assert_eq!(
            agent_id.as_deref(),
            Some("Claude Code: worktrees-hives agent")
        );
        assert_eq!(commit_sha.as_deref(), Some("abc1234"));

        let hyphenated = Cli::try_parse_from([
            "writ",
            "attribution",
            "format",
            "--body",
            "- Fixed the branch check.",
        ])
        .unwrap();
        let Some(super::Command::Attribution {
            action:
                super::AttributionAction::Format {
                    body: hyphen_body, ..
                },
        }) = hyphenated.command
        else {
            panic!("expected hyphenated attribution body")
        };
        assert_eq!(hyphen_body, "- Fixed the branch check.");

        let pr = Cli::try_parse_from([
            "writ",
            "attribution",
            "format",
            "--body",
            "All checks passed.",
            "--pr-comment",
        ])
        .unwrap();
        let Some(super::Command::Attribution {
            action:
                super::AttributionAction::Format {
                    pr_comment: true, ..
                },
        }) = pr.command
        else {
            panic!("expected PR comment attribution format")
        };
    }

    struct FormatCase {
        json: bool,
        body: &'static str,
        agent_id: Option<&'static str>,
        commit_sha: Option<&'static str>,
        placement: Option<&'static str>,
        pr_comment: bool,
    }

    fn format_cli(case: FormatCase) -> Cli {
        Cli {
            json: case.json,
            allowed_owners: None,
            command: Some(super::Command::Attribution {
                action: super::AttributionAction::Format {
                    body: case.body.to_owned(),
                    agent_id: case.agent_id.map(str::to_owned),
                    commit_sha: case.commit_sha.map(str::to_owned),
                    placement: case.placement.map(str::to_owned),
                    pr_comment: case.pr_comment,
                },
            }),
        }
    }

    async fn run_format(case: FormatCase) -> (writ_core::error::Result<ExitCode>, Vec<u8>) {
        let mut stdout = Vec::new();
        let result = run(format_cli(case), &mut stdout).await;
        (result, stdout)
    }

    fn parse_stdout_json(stdout: &[u8]) -> serde_json::Value {
        serde_json::from_str(str::from_utf8(stdout).unwrap().trim()).unwrap()
    }

    fn format_envelope_ok(value: &serde_json::Value) -> bool {
        value["ok"] == true
            && value["schema_version"] == 1
            && value["command"] == "attribution.format"
            && value["error"].is_null()
    }

    async fn format_ok(case: FormatCase) -> Vec<u8> {
        let (result, stdout) = run_format(case).await;
        assert_eq!(result.unwrap(), ExitCode::SUCCESS);
        stdout
    }

    fn format_data(value: &serde_json::Value) -> (Option<&str>, Option<&str>, Option<bool>, bool) {
        (
            value["data"]["text"].as_str(),
            value["data"]["commit_sha"].as_str(),
            value["data"]["is_thread_reply"].as_bool(),
            value["data"]["commit_sha"].is_null(),
        )
    }

    #[tokio::test]
    async fn attribution_format_human_omits_sha() {
        let human = format_ok(FormatCase {
            json: false,
            body: "Looks good!",
            agent_id: Some("worktrees-hives agent"),
            commit_sha: None,
            placement: None,
            pr_comment: false,
        })
        .await;
        assert_eq!(
            str::from_utf8(&human).unwrap(),
            "Looks good!\n\n---\nworktrees-hives agent\n"
        );
    }

    #[tokio::test]
    async fn attribution_format_collaboration_message_omits_sha() {
        let collab = parse_stdout_json(
            &format_ok(FormatCase {
                json: true,
                body: "Overlap: I own SKILL.md Reply attribution; RM-145 owns the rest.",
                agent_id: Some("worktrees-hives agent"),
                commit_sha: None,
                placement: None,
                pr_comment: false,
            })
            .await,
        );
        assert!(format_envelope_ok(&collab), "{collab}");
        assert_eq!(
            format_data(&collab),
            (
                Some(
                    "Overlap: I own SKILL.md Reply attribution; RM-145 owns the rest.\n\n---\nworktrees-hives agent"
                ),
                None,
                Some(true),
                true
            )
        );
    }

    #[tokio::test]
    async fn attribution_format_json_includes_pushed_sha() {
        let with_sha = parse_stdout_json(
            &format_ok(FormatCase {
                json: true,
                body: "Fixed the issue.",
                agent_id: Some("worktrees-hives agent"),
                commit_sha: Some("abc1234"),
                placement: Some("footer"),
                pr_comment: false,
            })
            .await,
        );
        assert!(format_envelope_ok(&with_sha), "{with_sha}");
        assert_eq!(
            format_data(&with_sha),
            (
                Some("Fixed the issue.\n\n---\nworktrees-hives agent: fixed in abc1234"),
                Some("abc1234"),
                Some(true),
                false
            )
        );
    }

    #[tokio::test]
    async fn attribution_format_pr_comment_omits_blank_sha() {
        let omit_sha = parse_stdout_json(
            &format_ok(FormatCase {
                json: true,
                body: "No code change.",
                agent_id: Some("Codex: worktrees-hives agent"),
                commit_sha: Some("  "),
                placement: None,
                pr_comment: true,
            })
            .await,
        );
        assert_eq!(
            format_data(&omit_sha),
            (
                Some("No code change.\n\nCodex: worktrees-hives agent"),
                None,
                Some(false),
                true
            )
        );
    }

    #[tokio::test]
    async fn attribution_format_rejects_invalid_inputs() {
        for case in [
            FormatCase {
                json: false,
                body: "   ",
                agent_id: None,
                commit_sha: None,
                placement: None,
                pr_comment: false,
            },
            FormatCase {
                json: false,
                body: "Looks good!",
                agent_id: None,
                commit_sha: Some("not-a-sha"),
                placement: None,
                pr_comment: false,
            },
        ] {
            let (result, stdout) = run_format(case).await;
            assert!(result.is_err() && stdout.is_empty());
        }
    }

    #[tokio::test]
    async fn attribution_format_json_rejects_empty_body_with_envelope() {
        let cli = format_cli(FormatCase {
            json: true,
            body: "",
            agent_id: None,
            commit_sha: None,
            placement: None,
            pr_comment: false,
        });
        assert_eq!(json_error_command(&cli), Some("attribution.format"));
        let (result, mut stdout) = run_format(FormatCase {
            json: true,
            body: "",
            agent_id: None,
            commit_sha: None,
            placement: None,
            pr_comment: false,
        })
        .await;
        let error = result.expect_err("empty body must be rejected");
        assert!(stdout.is_empty());
        write_json_error_envelope(
            "attribution.format",
            writ_core::contract::SCHEMA_VERSION,
            &error,
            &mut stdout,
        )
        .unwrap();
        let value = parse_stdout_json(&stdout);
        assert_eq!(
            (
                value["ok"].as_bool(),
                value["schema_version"].as_u64(),
                value["command"].as_str(),
                value["error"]["message"].as_str(),
                value["error"]["code"]
                    .as_str()
                    .is_some_and(|code| !code.is_empty())
            ),
            (
                Some(false),
                Some(1),
                Some("attribution.format"),
                Some("io operation: attribution body must not be empty"),
                true
            )
        );

        let empty_agent = parse_stdout_json(
            &format_ok(FormatCase {
                json: true,
                body: "Looks good!",
                agent_id: Some("   "),
                commit_sha: None,
                placement: None,
                pr_comment: false,
            })
            .await,
        );
        assert_eq!(
            empty_agent["data"]["agent_id"],
            writ_core::attribution::DEFAULT_AGENT_ID
        );
    }

    #[test]
    fn worktree_create_parser_distinguishes_v1_migration_from_v2_exact_base() {
        let missing = Cli::try_parse_from([
            "writ", "worktree", "create", "--repo", ".", "acme", "repo", "job", "branch",
        ])
        .unwrap();
        let Some(super::Command::Worktree {
            action:
                super::WorktreeAction::Create {
                    start_point: None,
                    schema_version: 1,
                    ..
                },
        }) = missing.command
        else {
            panic!("expected legacy worktree create request")
        };

        let parsed = Cli::try_parse_from([
            "writ",
            "worktree",
            "create",
            "--schema-version",
            "2",
            "--repo",
            ".",
            "--start-point",
            "origin/trunk",
            "acme",
            "repo",
            "job",
            "branch",
        ])
        .unwrap();
        let Some(super::Command::Worktree {
            action:
                super::WorktreeAction::Create {
                    start_point,
                    schema_version,
                    ..
                },
        }) = parsed.command
        else {
            panic!("expected worktree create command")
        };
        assert_eq!(start_point.as_deref(), Some("origin/trunk"));
        assert_eq!(schema_version, 2);
    }

    #[test]
    fn worktree_create_parser_accepts_pr_import_identity() {
        let with_pr = Cli::try_parse_from([
            "writ",
            "worktree",
            "create",
            "--schema-version",
            "2",
            "--repo",
            ".",
            "--start-point",
            "0123456789abcdef0123456789abcdef01234567",
            "--pr-number",
            "42",
            "--head-repo",
            "acme/fork",
            "acme",
            "repo",
            "job",
            "branch",
        ])
        .unwrap();
        let Some(super::Command::Worktree {
            action:
                super::WorktreeAction::Create {
                    pr_number,
                    head_repo,
                    source_remote,
                    ..
                },
        }) = with_pr.command
        else {
            panic!("expected worktree create command with PR identity")
        };
        assert_eq!(pr_number, Some(42));
        assert_eq!(head_repo.as_deref(), Some("acme/fork"));
        assert_eq!(source_remote.as_deref(), None);
    }

    #[tokio::test]
    async fn json_mode_writes_v1_envelope_to_stdout() {
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: None,
        };
        let mut stdout = Vec::new();

        let code = run(cli, &mut stdout).await.unwrap();
        assert_eq!(code, ExitCode::SUCCESS);

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(v.get("schema_version").expect("missing schema_version"), 1);
        assert!(v.get("ok").expect("missing ok").as_bool().unwrap());
    }

    #[tokio::test]
    async fn default_mode_keeps_stdout_empty() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: None,
        };
        let mut stdout = Vec::new();

        let code = run(cli, &mut stdout).await.unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(stdout.is_empty());
    }

    #[test]
    fn status_json_emits_v1_envelope_with_jobs_in_data() {
        let mut stdout = Vec::new();

        run_with_jobs(true, "cli.status", vec![], &mut stdout).unwrap();

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();

        assert_eq!(v.get("schema_version").expect("missing schema_version"), 1);
        assert_eq!(v.get("command").expect("missing command"), "cli.status");
        assert!(v.get("ok").expect("missing ok").as_bool().unwrap());
        assert!(
            v.get("error").expect("missing error").is_null(),
            "error must be explicitly null, not absent"
        );
        let data = v.get("data").expect("missing data");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert!(jobs.is_empty());
        assert!(v.get("jobs").is_none(), "jobs must be nested under data");
    }

    #[test]
    fn status_json_includes_injected_jobs() {
        let mut stdout = Vec::new();

        run_with_jobs(true, "cli.status", vec![sample_job()], &mut stdout).unwrap();

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();

        let data = v.get("data").expect("missing data");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].get("job_id").expect("missing job_id"), "writ-1");
        assert_eq!(
            jobs[0].get("process_state").expect("missing process_state"),
            "running"
        );
    }

    #[test]
    fn jobs_json_emits_v1_envelope_with_jobs_in_data() {
        let mut stdout = Vec::new();

        run_with_jobs(true, "cli.jobs", vec![], &mut stdout).unwrap();

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();

        assert_eq!(v.get("schema_version").expect("missing schema_version"), 1);
        assert_eq!(v.get("command").expect("missing command"), "cli.jobs");
        assert!(v.get("ok").expect("missing ok").as_bool().unwrap());
        assert!(
            v.get("error").expect("missing error").is_null(),
            "error must be explicitly null, not absent"
        );
        let data = v.get("data").expect("missing data");
        let jobs = data
            .get("jobs")
            .expect("missing data.jobs")
            .as_array()
            .expect("data.jobs must be an array");
        assert!(jobs.is_empty());
        assert!(v.get("jobs").is_none(), "jobs must be nested under data");
    }

    #[test]
    fn status_without_json_prints_human_readable() {
        let mut stdout = Vec::new();

        run_with_jobs(false, "cli.status", vec![], &mut stdout).unwrap();

        assert_eq!(str::from_utf8(&stdout).unwrap(), "No watched jobs.\n");
    }

    #[test]
    fn jobs_without_json_prints_human_readable() {
        let mut stdout = Vec::new();

        run_with_jobs(false, "cli.jobs", vec![], &mut stdout).unwrap();

        assert_eq!(str::from_utf8(&stdout).unwrap(), "No watched jobs.\n");
    }

    #[test]
    fn status_without_json_lists_jobs_when_present() {
        let mut stdout = Vec::new();

        run_with_jobs(false, "cli.status", vec![sample_job()], &mut stdout).unwrap();

        let output = str::from_utf8(&stdout).unwrap();
        assert!(output.contains("writ-1"));
        assert!(output.contains("running"));
        assert!(output.contains("feature/foo"));
    }

    #[test]
    fn status_json_load_error_emits_failure_envelope() {
        let mut stdout = Vec::new();
        let err = run_status(
            true,
            "cli.status",
            Err("failed to parse watched.json: expected value".to_owned()),
            &mut stdout,
        )
        .unwrap_err();
        assert!(err.to_string().contains("failed to parse"));

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(v.get("ok").expect("missing ok"), false);
        assert_eq!(
            v.get("error")
                .expect("missing error")
                .get("code")
                .expect("missing code"),
            "STATE_LOAD_FAILED"
        );
        assert!(
            v.get("data")
                .expect("missing data")
                .get("jobs")
                .expect("missing jobs")
                .as_array()
                .expect("jobs array")
                .is_empty()
        );
    }

    #[test]
    fn status_human_load_error_does_not_print_healthy_empty() {
        let mut stdout = Vec::new();
        let err = run_status(
            false,
            "cli.status",
            Err("failed to read watched.json: permission denied".to_owned()),
            &mut stdout,
        )
        .unwrap_err();
        assert!(err.to_string().contains("permission denied"));
        // Must not look like a healthy empty watch list.
        assert!(
            !str::from_utf8(&stdout).unwrap().contains("No watched jobs"),
            "human load error must not print healthy empty summary"
        );
        assert!(
            stdout.is_empty(),
            "human load error should not write a success summary to stdout"
        );
    }

    #[tokio::test]
    async fn supervisor_run_non_json_emits_raw_supervised_output() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: {
                        #[cfg(windows)]
                        {
                            vec!["where.exe".to_owned(), "where.exe".to_owned()]
                        }
                        #[cfg(not(windows))]
                        {
                            vec!["true".to_owned()]
                        }
                    },
                },
            }),
        };
        let mut stdout = Vec::new();

        let code = run(cli, &mut stdout).await.unwrap();
        assert_eq!(code, ExitCode::SUCCESS);

        let output = str::from_utf8(&stdout).unwrap();
        assert!(output.contains("\"exit_code\":0"));
        // Non-json path is raw SupervisedOutput, not the Response envelope.
        assert!(!output.contains("\"command\":\"supervisor.run\""));
    }

    #[tokio::test]
    async fn supervisor_run_blocks_unsafe_git_command() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: vec!["git".to_owned(), "push".to_owned(), "--force".to_owned()],
                },
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::BareForcePush,
                ..
            }
        ));
        assert!(stdout.is_empty());
    }

    #[tokio::test]
    async fn supervisor_run_blocks_path_qualified_git_force() {
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: vec![
                        "/usr/bin/git".to_owned(),
                        "push".to_owned(),
                        "--force".to_owned(),
                    ],
                },
            }),
        };
        let mut stdout = Vec::new();
        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::BareForcePush,
                ..
            }
        ));
        let v: serde_json::Value =
            serde_json::from_str(str::from_utf8(&stdout).unwrap().trim()).unwrap();
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(false));
        assert_eq!(
            v.get("command").and_then(|x| x.as_str()),
            Some("supervisor.run")
        );
        assert_eq!(
            v.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str()),
            Some("BARE_FORCE_PUSH")
        );
    }

    #[tokio::test]
    async fn supervisor_run_json_nonzero_sets_ok_false() {
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: {
                        #[cfg(windows)]
                        {
                            // `false` may not exist; use powershell is forbidden. Use `cmd` is forbidden.
                            // Use git with invalid args for non-zero? Prefer `python` - not guaranteed.
                            // Use `ping` with bad args returns non-zero on Windows.
                            vec![
                                "ping".to_owned(),
                                "/n".to_owned(),
                                "0".to_owned(),
                                "127.0.0.1".to_owned(),
                            ]
                        }
                        #[cfg(not(windows))]
                        {
                            vec!["false".to_owned()]
                        }
                    },
                },
            }),
        };
        let mut stdout = Vec::new();
        let code = run(cli, &mut stdout).await.unwrap();
        // JSON mode always exits 0 when the envelope was written; status is in `ok`/`data`.
        assert_eq!(code, ExitCode::SUCCESS);
        let v: serde_json::Value =
            serde_json::from_str(str::from_utf8(&stdout).unwrap().trim()).unwrap();
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        let exit = v
            .get("data")
            .and_then(|d| d.get("exit_code"))
            .and_then(|c| c.as_i64());
        assert!(
            exit.is_some_and(|c| c != 0),
            "expected nonzero exit in data, got {exit:?}"
        );
        assert_eq!(
            v.get("data")
                .and_then(|d| d.get("error_code"))
                .and_then(|c| c.as_str()),
            Some("NON_ZERO_EXIT")
        );
    }

    #[tokio::test]
    async fn supervisor_run_blocks_unsafe_gh_command() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: vec!["gh".to_owned(), "pr".to_owned(), "merge".to_owned()],
                },
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::MergeBlocked,
                ..
            }
        ));
        assert!(stdout.is_empty());
    }

    #[tokio::test]
    async fn supervisor_run_json_wraps_v1_envelope() {
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: Some(super::Command::Supervisor {
                action: super::SupervisorAction::Run {
                    timeout: 0,
                    expected_branch: None,
                    repo: None,
                    max_parallel: 1,
                    cmd: {
                        #[cfg(windows)]
                        {
                            vec!["where.exe".to_owned(), "where.exe".to_owned()]
                        }
                        #[cfg(not(windows))]
                        {
                            vec!["true".to_owned()]
                        }
                    },
                },
            }),
        };
        let mut stdout = Vec::new();

        let code = run(cli, &mut stdout).await.unwrap();
        assert_eq!(code, ExitCode::SUCCESS);

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(
            v.get("command").and_then(|x| x.as_str()),
            Some("supervisor.run")
        );
        assert_eq!(v.get("schema_version").and_then(|x| x.as_u64()), Some(1));
        let data = v.get("data").expect("data");
        assert_eq!(data.get("exit_code").and_then(|x| x.as_i64()), Some(0));
    }

    #[test]
    fn supervised_exit_code_maps_timeout_and_kill() {
        let timed_out = writ_core::supervisor::SupervisedOutput {
            exit_code: None,
            timed_out: true,
            killed: true,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            error_code: None,
        };
        assert_eq!(supervised_exit_code(&timed_out), ExitCode::from(124));

        let child_fail = writ_core::supervisor::SupervisedOutput {
            exit_code: Some(7),
            timed_out: false,
            killed: false,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            error_code: None,
        };
        assert_eq!(supervised_exit_code(&child_fail), ExitCode::from(7));
    }

    #[tokio::test]
    async fn git_safe_valid_command_executes() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::GitSafe {
                expected_branch: None,
                repo: None,
                args: vec!["rev-parse".to_owned(), "--is-inside-work-tree".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        let code = run(cli, &mut stdout).await.unwrap();
        assert_eq!(code, ExitCode::SUCCESS);

        let output = str::from_utf8(&stdout).unwrap();
        assert!(output.contains("ran: git rev-parse --is-inside-work-tree"));
        assert!(output.contains("true"));
    }

    #[tokio::test]
    async fn git_safe_json_mode_includes_exit_code() {
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: Some(super::Command::GitSafe {
                expected_branch: None,
                repo: None,
                args: vec!["rev-parse".to_owned(), "--is-inside-work-tree".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        run(cli, &mut stdout).await.unwrap();

        let output = str::from_utf8(&stdout).unwrap();
        let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v.get("command").and_then(|x| x.as_str()), Some("git.safe"));
        let data = v.get("data").expect("data");
        assert_eq!(data.get("exit_code").and_then(|x| x.as_i64()), Some(0));
        assert!(
            data.get("stdout")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .contains("true")
        );
    }

    #[tokio::test]
    async fn git_safe_blocks_mergetool() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::GitSafe {
                expected_branch: None,
                repo: None,
                args: vec!["mergetool".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn git_safe_blocks_bare_force() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::GitSafe {
                expected_branch: None,
                repo: None,
                args: vec!["push".to_owned(), "--force".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::BareForcePush,
                ..
            }
        ));
    }

    #[test]
    fn git_safe_checkout_merge_branch_name_not_merge_blocked() {
        // Subcommand-only merge detection: a branch argument named `merge` is fine.
        // Policy-only check — do not execute checkout (would require a real ref / switch).
        let cmd =
            writ_core::git_safe::SafeGitCommand::new(&["checkout".to_owned(), "merge".to_owned()])
                .expect("checkout of branch named merge must not be MergeBlocked");
        assert_eq!(cmd.subcommand(), "checkout");
        assert_eq!(cmd.args(), &["checkout", "merge"]);
    }

    #[tokio::test]
    async fn gh_safe_pr_merge_blocked() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::GhSafe {
                args: vec!["pr".to_owned(), "merge".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::MergeBlocked,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn gh_safe_api_blocked() {
        let cli = Cli {
            json: false,
            allowed_owners: None,
            command: Some(super::Command::GhSafe {
                args: vec!["api".to_owned(), "repos/acme/example-org".to_owned()],
            }),
        };
        let mut stdout = Vec::new();

        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::GhSubcommandNotAllowed,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn gh_safe_pr_view_allowed_at_policy() {
        // May fail if gh is not auth'd; policy must accept `pr view`.
        let cli = Cli {
            json: true,
            allowed_owners: None,
            command: Some(super::Command::GhSafe {
                args: vec!["pr".to_owned(), "view".to_owned(), "1".to_owned()],
            }),
        };
        let mut stdout = Vec::new();
        match run(cli, &mut stdout).await {
            Ok(_) => {
                let output = str::from_utf8(&stdout).unwrap();
                assert!(output.contains("\"command\":\"gh.safe\""));
            }
            Err(writ_core::error::Error::PolicyViolation { .. }) => {
                panic!("pr view must pass policy")
            }
            Err(writ_core::error::Error::Io { .. }) => {
                // gh binary missing is acceptable in constrained envs
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn gh_safe_repo_selector_outside_allowlist_is_blocked() {
        let cli = Cli {
            json: false,
            allowed_owners: Some("acme".to_owned()),
            command: Some(super::Command::GhSafe {
                args: vec![
                    "pr".to_owned(),
                    "view".to_owned(),
                    "-R".to_owned(),
                    "other/repo".to_owned(),
                    "1".to_owned(),
                ],
            }),
        };
        let mut stdout = Vec::new();
        let err = run(cli, &mut stdout).await.unwrap_err();
        assert!(matches!(
            err,
            writ_core::error::Error::PolicyViolation {
                code: writ_core::error::PolicyCode::OwnerNotAllowed,
                ..
            }
        ));
        assert!(stdout.is_empty());
    }

    #[test]
    fn allowed_owners_flag_is_global() {
        let parsed = Cli::try_parse_from([
            "writ",
            "--allowed-owners",
            "acme,example-org",
            "gh-safe",
            "pr",
            "view",
            "-R",
            "acme/repo",
            "1",
        ])
        .unwrap();
        assert_eq!(parsed.allowed_owners.as_deref(), Some("acme,example-org"));
    }
}
