//! Crash-consistent lease records for worktree allocation.
//!
//! Persist the requested symbolic identity and resolved canonical start commit
//! *before* git mutation. Inspect reports observed identity without adopting
//! anything. Reconcile is deterministic: promote a fully matching interrupted
//! allocation, retry only when no mutation occurred, and keep a needs-attention
//! state for partial or conflicting evidence. Released and tombstoned rows are
//! never resurrected.

mod allocate;
mod fix_cycle;
mod inspect;
mod reconcile;
mod row;
mod schema;
mod tx;
mod types;
mod util;

#[cfg(test)]
mod tests;

pub use types::*;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::{Error, Result};

/// SQLite-backed lease store with a durable allocation journal.
pub struct LeaseStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for LeaseStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LeaseStore {
    /// Filesystem path of this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(in crate::lease) fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| Error::LeaseStore {
            context: "lease store",
            message: "lease store mutex poisoned".to_owned(),
        })
    }
}
