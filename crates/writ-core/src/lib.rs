//! Safety-focused core primitives for `writ`.
//!
//! The module boundaries are established in the R1 scaffold. Their behavior is
//! implemented by the linked foundation issues.

pub mod attribution;
mod bash_argv;
pub mod checkout;
pub mod ci_taxonomy;
pub mod contract;
pub mod error;
mod git_cmd;
pub mod git_safe;
pub mod hook;
pub mod identity;
pub mod install;
pub mod lease;
pub mod owners;
pub mod paths;
pub mod porcelain;
mod pr_import;
pub mod state;
pub mod status;
pub mod supervisor;
pub mod timeout_policy;
pub mod watchlist;
pub mod worktree;

/// Version shared by the core library and CLI workspace packages.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
