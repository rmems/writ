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
    any_record(bytes, |record| {
        record.path.as_path() == expected_path
            && record.branch.as_deref() == Some(expected_branch_ref.as_str())
            && record.head.as_deref() == Some(expected_head.as_str())
    })
}

/// True when any record's worktree path equals `expected_path`.
#[must_use]
pub fn path_is_registered(bytes: &[u8], expected_path: &Path) -> bool {
    any_record(bytes, |record| record.path.as_path() == expected_path)
}

fn any_record(bytes: &[u8], pred: impl Fn(&WorktreeRecord) -> bool) -> bool {
    parse_porcelain_z(bytes).iter().any(pred)
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
            "HEAD" => self.head = Some(text_value(value)),
            "branch" => self.branch = Some(text_value(value)),
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

fn text_value(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
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
        for (label, value) in [
            (b"worktree".as_slice(), path),
            (b"HEAD".as_slice(), head.as_bytes()),
            (b"branch".as_slice(), branch.as_bytes()),
        ] {
            out.extend_from_slice(label);
            out.push(b' ');
            out.extend_from_slice(value);
            out.push(0);
        }
        out.push(0);
        out
    }

    fn utf8_record(path: &str, head: &str, branch: &str) -> Vec<u8> {
        record_bytes(path.as_bytes(), head, branch)
    }

    fn assert_one_complete_record(bytes: &[u8], path: &str, head: &str, branch: &str) {
        let records = parse_porcelain_z(bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from(path));
        assert_eq!(records[0].head.as_deref(), Some(head));
        assert_eq!(records[0].branch.as_deref(), Some(branch));
        assert!(registration_matches(
            bytes,
            Path::new(path),
            BranchRef(branch),
            CommitId(head),
        ));
        assert!(path_is_registered(bytes, Path::new(path)));
    }

    fn assert_identity_rejected(bytes: &[u8], path: &str, branch: &str, head: &str) {
        assert!(!registration_matches(
            bytes,
            Path::new(path),
            BranchRef(branch),
            CommitId(head),
        ));
    }

    #[test]
    fn complete_records_keep_path_head_and_branch_together() {
        let ordinary_head = "a".repeat(40);
        let newline_head = "b".repeat(40);
        let newline_path = "/tmp/base\nsegment/job";
        assert_one_complete_record(
            &utf8_record("/tmp/hive/job", &ordinary_head, "refs/heads/feature/job"),
            "/tmp/hive/job",
            &ordinary_head,
            "refs/heads/feature/job",
        );
        let newline = utf8_record(newline_path, &newline_head, "refs/heads/feature/job");
        assert_one_complete_record(
            &newline,
            newline_path,
            &newline_head,
            "refs/heads/feature/job",
        );
        assert!(!path_is_registered(&newline, Path::new("/tmp/base")));
    }

    #[test]
    fn path_bytes_are_not_trimmed() {
        let head = "d".repeat(40);
        let bytes = record_bytes(b"/tmp/job ", &head, "refs/heads/feature/job");
        assert_eq!(
            parse_porcelain_z(&bytes)[0].path,
            PathBuf::from("/tmp/job ")
        );
        assert!(!path_is_registered(&bytes, Path::new("/tmp/job")));
    }

    #[test]
    fn ignores_unrelated_records_and_rejects_partial_matches() {
        let head = "c".repeat(40);
        let other_head = "e".repeat(40);
        let mut mixed = utf8_record("/tmp/other", &head, "refs/heads/feature/job");
        mixed.extend_from_slice(&utf8_record(
            "/tmp/hive/job",
            &head,
            "refs/heads/feature/other",
        ));
        mixed.extend_from_slice(&utf8_record(
            "/tmp/elsewhere",
            &other_head,
            "refs/heads/feature/elsewhere",
        ));
        assert_identity_rejected(&mixed, "/tmp/hive/job", "refs/heads/feature/job", &head);
        assert!(!path_is_registered(&mixed, Path::new("/tmp/hive")));
        for (path, rec_head, branch) in [
            ("/tmp/other", head.as_str(), "refs/heads/feature/job"),
            (
                "/tmp/hive/job",
                other_head.as_str(),
                "refs/heads/feature/job",
            ),
            ("/tmp/hive/job", head.as_str(), "refs/heads/feature/other"),
        ] {
            assert_identity_rejected(
                &utf8_record(path, rec_head, branch),
                "/tmp/hive/job",
                "refs/heads/feature/job",
                &head,
            );
        }
    }

    #[test]
    fn does_not_combine_attributes_across_record_boundaries() {
        let head = "f".repeat(40);
        let mut bytes = utf8_record("/tmp/hive/job", &head, "refs/heads/feature/other");
        bytes.extend_from_slice(&utf8_record("/tmp/other", &head, "refs/heads/feature/job"));
        assert_identity_rejected(&bytes, "/tmp/hive/job", "refs/heads/feature/job", &head);
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
