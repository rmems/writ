//! Optional read-only open of the existing lease store.
//!
//! Read paths must not create `leases.db`. A missing file is `Ok(None)`; a
//! path that exists but is not a regular file is an error.

use writ_core::error::Result;
use writ_core::lease::LeaseStore;
use writ_core::paths::lease_store_path;

pub(crate) fn open_existing_read_only_store() -> Result<Option<LeaseStore>> {
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
