//! Watched-job state: **read path only**.
//!
//! There is no writer. This module exposes `load_jobs` and nothing that
//! persists, so nothing in this workspace ever creates `watched.json`. With the
//! file absent -- the normal case -- `writ status` and `writ jobs` return an empty
//! array.
//!
//! That is not the same as "always empty". `load_jobs_from` returns an empty vec
//! only on `NotFound`; a file that does exist at the resolved path is parsed and
//! returned as-is. So populated output is reachable when something outside this
//! workspace writes the file, or when `WRIT_STATE_PATH` points at one. The gap is
//! the missing writer, not a guarantee about the value.
//!
//! GitHub #26 ("R3: Job/state store") is closed as completed and an earlier
//! version of this comment claimed it implemented the store. It delivered the
//! reader and the serde types; the persistence path never landed. The claim is
//! corrected here rather than left implying a writer exists somewhere.
//!
//! This module is superseded rather than completed: the SQLite lease store in
//! GitHub #124 replaces it, with crash consistency tracked in #136. Do not add a
//! `watched.json` writer without checking whether the lease store should own the
//! state instead.

use std::fs;
use std::path::Path;

use crate::paths::state_path;
use crate::status::JobStatus;

/// Return all currently watched jobs from the persisted store.
///
/// Returns `Ok(vec![])` only when the state file does not exist, which is the
/// normal case because nothing in this workspace creates it. An existing,
/// well-formed file is parsed and its jobs returned unchanged.
/// Returns `Err` when the file exists but cannot be read or parsed,
/// so callers can surface the failure instead of silently masking it.
///
/// Path resolution honours `WRIT_STATE_PATH` via [`crate::paths::state_path`].
pub fn load_jobs() -> Result<Vec<JobStatus>, String> {
    load_jobs_from(&state_path())
}

/// Load jobs from an explicit state file path (used by tests and future callers).
///
/// This is the implementation behind [`load_jobs`] and is the seam used to test
/// success and malformed-JSON behaviour for the `WRIT_STATE_PATH` store without
/// mutating process environment (workspace forbids `unsafe-code`).
pub fn load_jobs_from(path: &Path) -> Result<Vec<JobStatus>, String> {
    let data = match fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read {}: {}", path.display(), e)),
    };
    serde_json::from_str(&data).map_err(|e| format!("failed to parse {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::load_jobs_from;
    use crate::paths::resolve_state_path;
    use crate::status::{CiClass, JobStatus, ProcessState};

    fn sample_job() -> JobStatus {
        JobStatus {
            job_id: "writ-1".to_owned(),
            owner: "acme".to_owned(),
            repo: "example-org".to_owned(),
            issue_number: Some(1),
            pr_number: None,
            worktree_path: "/tmp/worktrees/acme/example-org/writ-1".to_owned(),
            branch: "feature/status".to_owned(),
            process_state: ProcessState::Running,
            last_error: None,
            ci_class: CiClass::Pending,
        }
    }

    fn unique_path(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn writ_state_path_load_success() {
        // Simulate WRIT_STATE_PATH pointing at a valid watched.json.
        let path = resolve_state_path(Some(unique_path("writ-state-ok").as_os_str()));
        let jobs = vec![sample_job()];
        fs::write(&path, serde_json::to_string(&jobs).unwrap()).unwrap();

        let loaded = load_jobs_from(&path).expect("load success");
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].job_id, "writ-1");
        assert_eq!(loaded[0].owner, "acme");
        assert_eq!(loaded[0].repo, "example-org");
    }

    #[test]
    fn writ_state_path_load_malformed_json() {
        // Simulate WRIT_STATE_PATH pointing at a corrupt watched.json.
        let path = resolve_state_path(Some(unique_path("writ-state-bad").as_os_str()));
        fs::write(&path, "{not valid json").unwrap();

        let err = load_jobs_from(&path).expect_err("malformed JSON must fail");
        let _ = fs::remove_file(&path);

        assert!(
            err.contains("failed to parse"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn load_jobs_from_missing_file_returns_empty() {
        let path = unique_path("writ-state-missing");
        let _ = fs::remove_file(&path);
        let loaded = load_jobs_from(&path).expect("missing is empty");
        assert!(loaded.is_empty());
    }
}
