//! `writ coord` CLI: same-host claims, overlap, and handoff.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Subcommand;
use serde::Serialize;
use writ_core::contract::Response;
use writ_core::coord::{AckRequest, AnnounceRequest, HandoffRequest, MessageKind, SendRequest};
use writ_core::lease::{JobKey, LeaseStore};
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
        /// Message kind: intent, overlap, help, ack, or dependency.
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

pub(crate) fn run(
    action: CoordAction,
    json: bool,
    stdout: &mut impl Write,
) -> writ_core::error::Result<ExitCode> {
    let response = dispatch(action)?;
    if json {
        writeln!(
            stdout,
            "{}",
            serde_json::to_string(&response).map_err(io::Error::other)?
        )?;
    } else {
        writeln!(stdout, "ok={} command={}", response.ok, response.command)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch(action: CoordAction) -> writ_core::error::Result<Response<serde_json::Value>> {
    let store = LeaseStore::open(lease_store_path())?;
    match action {
        CoordAction::Announce { .. } => announce(&store, action),
        CoordAction::Show {
            owner,
            repo_name,
            job_id,
        } => job_field(&owner, &repo_name, &job_id, "coord.show", "claim", |key| {
            store.find_claim(key)
        }),
        CoordAction::List => {
            let claims = store.list_claims()?;
            ok("coord.list", serde_json::json!({ "claims": claims }))
        }
        CoordAction::Inbox {
            owner,
            repo_name,
            job_id,
        } => job_field(
            &owner,
            &repo_name,
            &job_id,
            "coord.inbox",
            "messages",
            |key| store.inbox(key),
        ),
        CoordAction::Send { .. } => send(&store, action),
        CoordAction::Ack { .. } => ack(&store, action),
        CoordAction::Pause { .. } => pause(&store, action),
        CoordAction::Handoff { .. } => handoff(&store, action),
    }
}

fn job_key<'a>(owner: &'a str, repo_name: &'a str, job_id: &'a str) -> JobKey<'a> {
    JobKey {
        owner,
        repo_name,
        job_id,
    }
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

fn job_field<T: Serialize>(
    owner: &str,
    repo_name: &str,
    job_id: &str,
    command: &'static str,
    field: &'static str,
    load: impl FnOnce(JobKey<'_>) -> writ_core::error::Result<T>,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let payload = load(job_key(owner, repo_name, job_id))?;
    ok(command, serde_json::json!({ field: payload }))
}

fn announce(
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
        unreachable!("dispatch only forwards Announce");
    };
    let result = store.announce(AnnounceRequest {
        owner: &owner,
        repo_name: &repo_name,
        job_id: &job_id,
        agent_id: &agent,
        session_id: session.as_deref(),
        agent_type: "worker",
        intent: intent.as_deref(),
        paths: &paths,
    })?;
    value("coord.announce", &result)
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

fn ack(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let CoordAction::Ack {
        owner,
        repo_name,
        job_id,
        id,
        agent,
        session,
    } = action
    else {
        unreachable!("dispatch only forwards Ack");
    };
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
        serde_json::json!({
            "ack": ack,
            "claim": claim,
        }),
    )
}

fn pause(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let CoordAction::Pause {
        owner,
        repo_name,
        job_id,
        agent,
        body,
    } = action
    else {
        unreachable!("dispatch only forwards Pause");
    };
    let (claim, help) = store.pause_claim(
        job_key(&owner, &repo_name, &job_id),
        &agent,
        body.as_deref(),
    )?;
    ok(
        "coord.pause",
        serde_json::json!({
            "claim": claim,
            "help": help,
        }),
    )
}

fn handoff(
    store: &LeaseStore,
    action: CoordAction,
) -> writ_core::error::Result<Response<serde_json::Value>> {
    let CoordAction::Handoff {
        owner,
        repo_name,
        job_id,
        agent,
        to_agent,
        to_job,
        generation,
        body,
    } = action
    else {
        unreachable!("dispatch only forwards Handoff");
    };
    let message = store.propose_handoff(HandoffRequest {
        owner: &owner,
        repo_name: &repo_name,
        job_id: &job_id,
        from_agent_id: &agent,
        to_agent_id: &to_agent,
        to_job_id: to_job.as_deref(),
        expected_generation: generation,
        body: &body,
    })?;
    value("coord.handoff", &message)
}
