//! `writ coord` CLI: same-host claims, overlap, and handoff.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Subcommand;
use serde::Serialize;
use writ_core::contract::Response;
use writ_core::coord::{
    AckRequest, AnnounceRequest, HandoffRequest, MessageKind, PauseRequest, SendRequest,
};
use writ_core::lease::{JobKey, LeaseStore};
use writ_core::owners::OwnerAllowlist;
use writ_core::paths::lease_store_path;

#[derive(Debug, Subcommand)]
pub enum CoordAction {
    /// Announce task/agent identity and declared paths; overlaps are advisory.
    Announce {
        /// GitHub-style owner segment.
        owner: String,
        /// Repository name segment.
        repo_name: String,
        /// Job id segment (e.g. gh-42).
        job_id: String,
        /// Agent identity that owns this claim.
        #[arg(long)]
        agent: String,
        /// Optional session identity for stale-message distinction.
        #[arg(long)]
        session: Option<String>,
        /// Optional declared intent string.
        #[arg(long)]
        intent: Option<String>,
        /// Repo-relative paths this job intends to touch.
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// Show one job's coordination claim.
    Show {
        owner: String,
        repo_name: String,
        job_id: String,
    },
    /// List live coordination claims in the shared store.
    List,
    /// Read overlap/help/handoff/ack messages for a job.
    Inbox {
        owner: String,
        repo_name: String,
        job_id: String,
    },
    /// Send a help, overlap, intent, dependency, or ack message.
    Send {
        owner: String,
        repo_name: String,
        job_id: String,
        /// Sending agent identity.
        #[arg(long)]
        agent: String,
        /// Message kind: intent, overlap, help, ack, dependency, or blocker.
        #[arg(long)]
        kind: String,
        /// Message body.
        #[arg(long)]
        body: String,
        #[arg(long)]
        to_agent: Option<String>,
        #[arg(long)]
        to_owner: Option<String>,
        #[arg(long)]
        to_repo: Option<String>,
        #[arg(long)]
        to_job: Option<String>,
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(long)]
        ack_of: Option<i64>,
    },
    /// Acknowledge a message; a matching handoff ACK transfers ownership.
    Ack {
        owner: String,
        repo_name: String,
        job_id: String,
        /// Message id to acknowledge.
        #[arg(long)]
        id: i64,
        /// Acknowledging agent identity.
        #[arg(long)]
        agent: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Pause the current owner and emit help without deleting WIP.
    Pause {
        owner: String,
        repo_name: String,
        job_id: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Offer a generation-bound handoff. Ownership moves only after ACK.
    Handoff {
        owner: String,
        repo_name: String,
        job_id: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        to_agent: String,
        #[arg(long)]
        to_job: Option<String>,
        #[arg(long)]
        generation: Option<i64>,
        #[arg(long)]
        body: String,
    },
}

impl CoordAction {
    pub(crate) fn envelope_command(&self) -> &'static str {
        match self {
            Self::Announce { .. } => "coord.announce",
            Self::Show { .. } => "coord.show",
            Self::List => "coord.list",
            Self::Inbox { .. } => "coord.inbox",
            Self::Send { .. } => "coord.send",
            Self::Ack { .. } => "coord.ack",
            Self::Pause { .. } => "coord.pause",
            Self::Handoff { .. } => "coord.handoff",
        }
    }
}

pub(crate) fn run(action: CoordAction, ctx: CoordCli<'_>) -> writ_core::error::Result<ExitCode> {
    let response = dispatch(action, ctx.allowlist)?;
    if ctx.json {
        writeln!(
            ctx.stdout,
            "{}",
            serde_json::to_string(&response).map_err(io::Error::other)?
        )?;
    } else {
        writeln!(
            ctx.stdout,
            "ok={} command={}",
            response.ok, response.command
        )?;
        writeln!(
            ctx.stdout,
            "{}",
            serde_json::to_string_pretty(&response.data).map_err(io::Error::other)?
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) struct CoordCli<'a> {
    pub allowlist: &'a OwnerAllowlist,
    pub json: bool,
    pub stdout: &'a mut dyn Write,
}

fn dispatch(
    action: CoordAction,
    allowlist: &OwnerAllowlist,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    enforce_coord_owners(&action, allowlist)?;
    if coord_is_read(&action) {
        return execute_coord_read(action, allowlist);
    }
    execute(Execute {
        store: LeaseStore::open(lease_store_path())?,
        action,
        allowlist,
    })
}

fn coord_is_read(action: &CoordAction) -> bool {
    matches!(
        action,
        CoordAction::Show { .. } | CoordAction::List | CoordAction::Inbox { .. }
    )
}

fn execute_coord_read(
    action: CoordAction,
    allowlist: &OwnerAllowlist,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    match open_coord_read_store()? {
        None => absent_coord_read(&action),
        Some(store) => execute_read(ReadCmd {
            store: &store,
            action,
            allowlist,
        }),
    }
}

fn open_coord_read_store() -> writ_core::error::Result<Option<LeaseStore>> {
    let path = lease_store_path();
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(Some(LeaseStore::open_read_only(path)?)),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("lease store path is not a regular file: {}", path.display()),
        )
        .into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn absent_coord_read(
    action: &CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    match action {
        CoordAction::List => ok("coord.list", serde_json::json!({ "claims": [] })),
        CoordAction::Show { .. } => ok("coord.show", serde_json::json!({ "claim": null })),
        CoordAction::Inbox { .. } => ok("coord.inbox", serde_json::json!({ "messages": [] })),
        _ => unreachable!("absent_coord_read only handles show/list/inbox"),
    }
}

fn enforce_coord_owners(
    action: &CoordAction,
    allowlist: &OwnerAllowlist,
) -> writ_core::error::Result<()> {
    match action {
        CoordAction::List => allowlist.enforce_discovery(),
        CoordAction::Send {
            owner, to_owner, ..
        } => {
            allowlist.enforce_owner(owner)?;
            to_owner
                .as_deref()
                .map(|owner| allowlist.enforce_owner(owner))
                .transpose()?;
            Ok(())
        }
        CoordAction::Announce { owner, .. }
        | CoordAction::Show { owner, .. }
        | CoordAction::Inbox { owner, .. }
        | CoordAction::Ack { owner, .. }
        | CoordAction::Pause { owner, .. }
        | CoordAction::Handoff { owner, .. } => allowlist.enforce_owner(owner),
    }
}

struct Execute<'a> {
    store: LeaseStore,
    action: CoordAction,
    allowlist: &'a OwnerAllowlist,
}

fn execute(ctx: Execute<'_>) -> writ_core::error::Result<Response<serde_json::Value>> {
    match ctx.action {
        CoordAction::Announce { .. }
        | CoordAction::Send { .. }
        | CoordAction::Ack { .. }
        | CoordAction::Pause { .. }
        | CoordAction::Handoff { .. } => execute_write(&ctx.store, ctx.action),
        other => execute_read(ReadCmd {
            store: &ctx.store,
            action: other,
            allowlist: ctx.allowlist,
        }),
    }
}

struct ReadCmd<'a> {
    store: &'a LeaseStore,
    action: CoordAction,
    allowlist: &'a OwnerAllowlist,
}

fn execute_read(cmd: ReadCmd<'_>) -> writ_core::error::Result<Response<serde_json::Value>> {
    match cmd.action {
        CoordAction::List => list_allowed(cmd.store, cmd.allowlist),
        other => load_coord_read(cmd.store, other),
    }
}

fn load_coord_read(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let (owner, repo_name, job_id, field) = match action {
        CoordAction::Show {
            owner,
            repo_name,
            job_id,
        } => (
            owner,
            repo_name,
            job_id,
            JobField {
                command: "coord.show",
                name: "claim",
            },
        ),
        CoordAction::Inbox {
            owner,
            repo_name,
            job_id,
        } => (
            owner,
            repo_name,
            job_id,
            JobField {
                command: "coord.inbox",
                name: "messages",
            },
        ),
        _ => unreachable!("load_coord_read only handles show/inbox"),
    };
    let key = JobKey {
        owner: &owner,
        repo_name: &repo_name,
        job_id: &job_id,
    };
    if field.name == "claim" {
        job_field(key, field, |key| store.find_claim(key))
    } else {
        job_field(key, field, |key| store.inbox(key))
    }
}

fn execute_write(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    match action {
        CoordAction::Send { .. } => send(store, action),
        CoordAction::Announce { .. } | CoordAction::Handoff { .. } => execute_offer(store, action),
        CoordAction::Ack { .. } | CoordAction::Pause { .. } => execute_control(store, action),
        _ => unreachable!("execute_write only handles mutating coord commands"),
    }
}

fn execute_offer(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    match action {
        CoordAction::Announce { .. } => execute_announce(store, action),
        CoordAction::Handoff {
            owner,
            repo_name,
            job_id,
            agent,
            to_agent,
            to_job,
            generation,
            body,
        } => value(
            "coord.handoff",
            &store.propose_handoff(HandoffRequest {
                owner: &owner,
                repo_name: &repo_name,
                job_id: &job_id,
                from_agent_id: &agent,
                to_agent_id: &to_agent,
                to_job_id: to_job.as_deref(),
                expected_generation: generation,
                body: &body,
            })?,
        ),
        _ => unreachable!("execute_offer only handles announce/handoff"),
    }
}

fn execute_announce(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let CoordAction::Announce {
        owner,
        repo_name,
        job_id,
        agent,
        session,
        intent,
        paths,
    } = action
    else {
        unreachable!("execute_announce only handles Announce");
    };
    value(
        "coord.announce",
        &store.announce(AnnounceRequest {
            owner: &owner,
            repo_name: &repo_name,
            job_id: &job_id,
            agent_id: &agent,
            session_id: session.as_deref(),
            agent_type: "worker",
            intent: intent.as_deref(),
            paths: &paths,
        })?,
    )
}

fn execute_control(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    match action {
        CoordAction::Ack {
            owner,
            repo_name,
            job_id,
            id,
            agent,
            session,
        } => {
            let (ack, claim) = store.ack_message(AckRequest {
                message_id: id,
                owner: &owner,
                repo_name: &repo_name,
                job_id: &job_id,
                agent_id: &agent,
                session_id: session.as_deref(),
            })?;
            ok(
                "coord.ack",
                serde_json::json!({ "ack": ack, "claim": claim }),
            )
        }
        CoordAction::Pause {
            owner,
            repo_name,
            job_id,
            agent,
            body,
        } => {
            let (claim, help) = store.pause_claim(PauseRequest {
                key: JobKey {
                    owner: &owner,
                    repo_name: &repo_name,
                    job_id: &job_id,
                },
                agent_id: &agent,
                body: body.as_deref(),
            })?;
            ok(
                "coord.pause",
                serde_json::json!({ "claim": claim, "help": help }),
            )
        }
        _ => unreachable!("execute_control only handles ack/pause"),
    }
}

fn list_allowed(
    store: &LeaseStore,
    allowlist: &OwnerAllowlist,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let claims = store
        .list_claims()?
        .into_iter()
        .filter(|claim| allowlist.allows(&claim.owner))
        .collect::<Vec<_>>();
    ok("coord.list", serde_json::json!({ "claims": claims }))
}

fn ok(
    command: &'static str,
    data: serde_json::Value,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    Ok(Response::success(command, data))
}

fn value<T: Serialize>(
    command: &'static str,
    payload: &T,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    ok(
        command,
        serde_json::to_value(payload).map_err(io::Error::other)?,
    )
}

struct JobField {
    command: &'static str,
    name: &'static str,
}

fn job_field<T: Serialize>(
    key: JobKey<'_>,
    field: JobField,
    load: impl FnOnce(JobKey<'_>) -> writ_core::error::Result<T>,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let payload = load(key)?;
    ok(field.command, serde_json::json!({ field.name: payload }))
}

fn send(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let CoordAction::Send {
        owner,
        repo_name,
        job_id,
        agent,
        kind,
        body,
        to_agent,
        to_owner,
        to_repo,
        to_job,
        paths,
        ack_of,
    } = action
    else {
        unreachable!("dispatch only forwards Send");
    };
    let message = store.send_message(SendRequest {
        owner: &owner,
        repo_name: &repo_name,
        job_id: &job_id,
        agent_id: &agent,
        kind: MessageKind::parse(&kind)?,
        body: &body,
        to_agent_id: to_agent.as_deref(),
        to_owner: to_owner.as_deref(),
        to_repo_name: to_repo.as_deref(),
        to_job_id: to_job.as_deref(),
        paths: &paths,
        ack_of,
    })?;
    value("coord.send", &message)
}
