//! Collaboration watchlist: a **view** over shared lease/coord state.
//!
//! RM-825 owns `leases.db` (and later `coord_claims` / `coord_messages`). This
//! module never writes those tables and never creates a competing JSON store.
//! GitHub PR/check data is fetched live when requested and is not persisted.

mod classify;
mod coord_read;
mod github;
mod types;
mod view;

#[cfg(test)]
mod view_tests;

pub use github::{GhPrProbe, GithubProbe, ProbeError};
pub use types::{
    CollabStatus, CoordOverlay, GithubState, RecoveryStatus, WatchEntry, WatchlistData,
};
pub use view::{ViewLoad, WatchQuery, load_view};

pub use classify::classify_snapshot;
pub use github::{BranchRef, PrRef, PrSnapshot, parse_pr_view};
