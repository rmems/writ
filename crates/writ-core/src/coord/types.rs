//! Public coordination types and request/result structs.

use serde::Serialize;

use crate::error::{Error, Result};
use crate::lease::JobKey;

/// Shared coordination event kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Intent,
    Overlap,
    Help,
    Handoff,
    Ack,
    Dependency,
    Blocker,
}

impl MessageKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Overlap => "overlap",
            Self::Help => "help",
            Self::Handoff => "handoff",
            Self::Ack => "ack",
            Self::Dependency => "dependency",
            Self::Blocker => "blocker",
        }
    }

    /// Parse a kind string. Unknown values fail closed.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "intent" => Ok(Self::Intent),
            "overlap" => Ok(Self::Overlap),
            "help" => Ok(Self::Help),
            "handoff" => Ok(Self::Handoff),
            "ack" => Ok(Self::Ack),
            "dependency" => Ok(Self::Dependency),
            "blocker" => Ok(Self::Blocker),
            other => Err(Error::LeaseStore {
                context: "parse coord message kind",
                message: format!("unknown message kind `{other}`"),
            }),
        }
    }
}

/// Declared owner of a leased job assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoordClaim {
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub branch: String,
    pub worktree_path: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub intent: Option<String>,
    pub declared_paths: Vec<String>,
    pub owner_generation: i64,
    pub paused_at: Option<i64>,
    pub allocation_state: String,
}

/// One inbox row. `ack_of` points at the message being acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoordMessage {
    pub id: i64,
    pub created_at: i64,
    pub kind: MessageKind,
    pub from_agent_id: String,
    pub from_owner: String,
    pub from_repo_name: String,
    pub from_job_id: String,
    pub to_agent_id: Option<String>,
    pub to_owner: Option<String>,
    pub to_repo_name: Option<String>,
    pub to_job_id: Option<String>,
    pub owner_generation: i64,
    pub body: String,
    pub paths: Vec<String>,
    pub ack_of: Option<i64>,
    pub acked_at: Option<i64>,
}

/// Advisory overlap against another live or paused claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathOverlap {
    pub owner: String,
    pub repo_name: String,
    pub job_id: String,
    pub agent_id: String,
    pub branch: String,
    pub paths: Vec<String>,
    /// Always true: overlap does not veto writes on distinct branches.
    pub advisory: bool,
}

/// Identity + declared paths for `announce`.
#[derive(Debug, Clone, Copy)]
pub struct AnnounceRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub session_id: Option<&'a str>,
    pub agent_type: &'a str,
    pub intent: Option<&'a str>,
    pub paths: &'a [String],
}

/// Result of announcing identity on a live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnnounceResult {
    pub claim: CoordClaim,
    pub intent: CoordMessage,
    pub overlaps: Vec<PathOverlap>,
}

/// Inputs for a generation-stamped handoff offer.
#[derive(Debug, Clone, Copy)]
pub struct HandoffRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub from_agent_id: &'a str,
    pub to_agent_id: &'a str,
    pub to_job_id: Option<&'a str>,
    pub expected_generation: Option<i64>,
    pub body: &'a str,
}

/// Inputs for acknowledging a message, including a handoff transfer.
#[derive(Debug, Clone, Copy)]
pub struct AckRequest<'a> {
    pub message_id: i64,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub session_id: Option<&'a str>,
}

/// Generic mailbox send (help/overlap/intent/dependency; not a seize).
#[derive(Debug, Clone, Copy)]
pub struct SendRequest<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub job_id: &'a str,
    pub agent_id: &'a str,
    pub kind: MessageKind,
    pub body: &'a str,
    pub to_agent_id: Option<&'a str>,
    pub to_owner: Option<&'a str>,
    pub to_repo_name: Option<&'a str>,
    pub to_job_id: Option<&'a str>,
    pub paths: &'a [String],
    pub ack_of: Option<i64>,
}

/// Pause the current owner and emit help without deleting WIP.
#[derive(Debug, Clone, Copy)]
pub struct PauseRequest<'a> {
    pub key: JobKey<'a>,
    pub agent_id: &'a str,
    pub body: Option<&'a str>,
}
