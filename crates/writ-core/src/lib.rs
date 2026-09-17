//! Safety-focused core primitives for `writ`.
//!
//! The module boundaries are established in the R1 scaffold. Their behavior is
//! implemented by the linked foundation issues.

pub mod contract;
pub mod error;
pub mod git_safe;
mod identity;
pub mod paths;
pub mod state;
pub mod status;
pub mod supervisor;
pub mod timeout_policy;
pub mod worktree;

/// Version shared by the core library and CLI workspace packages.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Test-only fixtures shared across the crate's unit tests.
///
/// Centralises the `sample_job()` builder that was previously duplicated in the
/// `status` and `state` test modules (qlty similar-code, mass 73). Call sites
/// override the fields they care about with struct-update syntax
/// (`JobStatus { field: ..., ..sample_job() }`).
#[cfg(test)]
pub(crate) mod test_support {
    use crate::status::{CiClass, JobStatus, ProcessState};

    /// A representative running job used as a base for test fixtures.
    pub(crate) fn sample_job() -> JobStatus {
        JobStatus {
            job_id: "writ-100".to_owned(),
            owner: "acme".to_owned(),
            repo: "example-org".to_owned(),
            issue_number: Some(29),
            pr_number: Some(42),
            worktree_path: "/tmp/worktrees/acme/example-org/writ-100".to_owned(),
            branch: "feature/status-json-cli".to_owned(),
            process_state: ProcessState::Running,
            last_error: None,
            timeout_residual: None,
            ci_class: CiClass::Pending,
        }
    }
}
