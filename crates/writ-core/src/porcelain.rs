//! NUL-delimited `git worktree list --porcelain -z` parser.
//!
//! Shared contract for creation postconditions and residual-state inspection:
//!
//! 1. Invoke `git worktree list --porcelain -z`.
//! 2. Keep stdout as raw bytes. Do not UTF-8-lossy-convert or trim before parse.
//! 3. Split attributes on NUL. An empty attribute is the worktree-record boundary.
//! 4. Parse each nonempty attribute as an ASCII label plus optional single-space value.
//! 5. Preserve the `worktree` value as a platform path and match that same record's
//!    exact `HEAD` and `branch` evidence.
//!
//! Git documents `-z` as the machine format that keeps newline-containing worktree
//! paths in one record. Callers must not route the path bytes through a `String`
//! before comparison.

use std::path::{Path, PathBuf};

use crate::identity::{BranchRef, CommitId};

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// One worktree record from `--porcelain -z` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
}

/// Parse NUL-delimited porcelain bytes into worktree records.
#[must_use]
pub fn parse_porcelain_z(bytes: &[u8]) -> Vec<WorktreeRecord> {
    let mut parser = PorcelainParser::default();
    for attr in bytes.split(|b| *b == 0) {
        parser.push(attr);
    }
    parser.finish()
}

#[derive(Default)]
struct PorcelainParser {
    records: Vec<WorktreeRecord>,
    current: PartialRecord,
    open: bool,
}

impl PorcelainParser {
    fn push(&mut self, attr: &[u8]) {
        match attr {
            [] => self.close(),
            field => self.field(field),
        }
    }

    fn field(&mut self, attr: &[u8]) {
        self.open = true;
        self.current.apply(attr);
    }

    fn close(&mut self) {
        if !self.open {
            return;
        }
        self.take_record();
        self.open = false;
    }

    fn take_record(&mut self) {
        if let Some(record) = std::mem::take(&mut self.current).finish() {
            self.records.push(record);
        }
    }

    fn finish(mut self) -> Vec<WorktreeRecord> {
        self.take_record();
        self.records
    }
}

/// True when one porcelain record matches the expected path, full branch ref, and HEAD.
#[must_use]
pub fn registration_matches(
    bytes: &[u8],
    expected_path: &Path,
    expected_branch_ref: BranchRef<'_>,
    expected_head: CommitId<'_>,
) -> bool {
    parse_porcelain_z(bytes).iter().any(|record| {
        record.path.as_path() == expected_path
            && record.branch.as_deref() == Some(expected_branch_ref.as_str())
            && record.head.as_deref() == Some(expected_head.as_str())
    })
}

/// True when any record's worktree path equals `expected_path`.
#[must_use]
pub fn path_is_registered(bytes: &[u8], expected_path: &Path) -> bool {
    parse_porcelain_z(bytes)
        .iter()
        .any(|record| record.path.as_path() == expected_path)
}

#[derive(Default)]
struct PartialRecord {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
}

impl PartialRecord {
    fn apply(&mut self, attr: &[u8]) {
        let (label, value) = split_label_value(attr);
        let Some(value) = value else {
            return;
        };
        match label {
            "worktree" => self.path = Some(path_from_bytes(value)),
            "HEAD" => self.head = Some(String::from_utf8_lossy(value).into_owned()),
            "branch" => self.branch = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }

    fn finish(self) -> Option<WorktreeRecord> {
        Some(WorktreeRecord {
            path: self.path?,
            head: self.head,
            branch: self.branch,
        })
    }
}

fn split_label_value(attr: &[u8]) -> (&str, Option<&[u8]>) {
    let (label_bytes, value) = match attr.iter().position(|b| *b == b' ') {
        Some(idx) => (&attr[..idx], Some(&attr[idx + 1..])),
        None => (attr, None),
    };
    match std::str::from_utf8(label_bytes) {
        Ok(label) if label.is_ascii() && !label.is_empty() => (label, value),
        _ => ("", value),
    }
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        match std::str::from_utf8(bytes) {
            Ok(path) => PathBuf::from(path),
            Err(_) => PathBuf::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn record_bytes(path: &[u8], head: &str, branch: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"worktree ");
        out.extend_from_slice(path);
        out.push(0);
        out.extend_from_slice(b"HEAD ");
        out.extend_from_slice(head.as_bytes());
        out.push(0);
        out.extend_from_slice(b"branch ");
        out.extend_from_slice(branch.as_bytes());
        out.push(0);
        out.push(0);
        out
    }

    fn utf8_record(path: &str, head: &str, branch: &str) -> Vec<u8> {
        record_bytes(path.as_bytes(), head, branch)
    }

    #[test]
    fn parses_ordinary_record() {
        let head = "a".repeat(40);
        let bytes = utf8_record("/tmp/hive/job", &head, "refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from("/tmp/hive/job"));
        assert_eq!(records[0].head.as_deref(), Some(head.as_str()));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/feature/job"));
        assert!(registration_matches(
            &bytes,
            Path::new("/tmp/hive/job"),
            BranchRef("refs/heads/feature/job"),
            CommitId(&head),
        ));
        assert!(path_is_registered(&bytes, Path::new("/tmp/hive/job")));
    }

    #[test]
    fn newline_in_path_stays_in_one_record() {
        let head = "b".repeat(40);
        let path = "/tmp/base\nsegment/job";
        let bytes = utf8_record(path, &head, "refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from(path));
        assert_eq!(records[0].head.as_deref(), Some(head.as_str()));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/feature/job"));
        assert!(registration_matches(
            &bytes,
            Path::new(path),
            BranchRef("refs/heads/feature/job"),
            CommitId(&head),
        ));
        assert!(path_is_registered(&bytes, Path::new(path)));
        assert!(!path_is_registered(&bytes, Path::new("/tmp/base")));
    }

    #[test]
    fn path_bytes_are_not_trimmed() {
        let head = "d".repeat(40);
        let path = b"/tmp/job ";
        let bytes = record_bytes(path, &head, "refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from("/tmp/job "));
        assert!(!path_is_registered(&bytes, Path::new("/tmp/job")));
    }

    #[test]
    fn ignores_unrelated_records_and_rejects_partial_matches() {
        let head = "c".repeat(40);
        let other_head = "e".repeat(40);
        let mut bytes = utf8_record("/tmp/other", &head, "refs/heads/feature/job");
        bytes.extend_from_slice(&utf8_record(
            "/tmp/hive/job",
            &head,
            "refs/heads/feature/other",
        ));
        bytes.extend_from_slice(&utf8_record(
            "/tmp/elsewhere",
            &other_head,
            "refs/heads/feature/elsewhere",
        ));

        let expected_path = Path::new("/tmp/hive/job");
        let expected_branch = BranchRef("refs/heads/feature/job");
        let expected_head = CommitId(&head);

        assert!(!registration_matches(
            &bytes,
            expected_path,
            expected_branch,
            expected_head,
        ));
        assert!(!path_is_registered(&bytes, Path::new("/tmp/hive")));
        assert!(!registration_matches(
            &utf8_record("/tmp/other", &head, "refs/heads/feature/job"),
            expected_path,
            expected_branch,
            expected_head,
        ));
        assert!(!registration_matches(
            &utf8_record("/tmp/hive/job", &other_head, "refs/heads/feature/job"),
            expected_path,
            expected_branch,
            expected_head,
        ));
        assert!(!registration_matches(
            &utf8_record("/tmp/hive/job", &head, "refs/heads/feature/other"),
            expected_path,
            expected_branch,
            expected_head,
        ));
    }

    #[test]
    fn does_not_combine_attributes_across_record_boundaries() {
        let head = "f".repeat(40);
        let mut bytes = utf8_record("/tmp/hive/job", &head, "refs/heads/feature/other");
        bytes.extend_from_slice(&utf8_record("/tmp/other", &head, "refs/heads/feature/job"));
        assert!(!registration_matches(
            &bytes,
            Path::new("/tmp/hive/job"),
            BranchRef("refs/heads/feature/job"),
            CommitId(&head),
        ));
        assert_eq!(parse_porcelain_z(&bytes).len(), 2);
    }

    #[test]
    fn empty_input_and_record_terminators_are_no_records() {
        assert!(parse_porcelain_z(b"").is_empty());
        assert!(parse_porcelain_z(&[0]).is_empty());
        assert!(parse_porcelain_z(&[0, 0]).is_empty());
    }

    #[test]
    fn missing_terminator_still_emits_the_open_record() {
        let head = "a".repeat(40);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"worktree /tmp/hive/job\0HEAD ");
        bytes.extend_from_slice(head.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(b"branch refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from("/tmp/hive/job"));
    }
}
