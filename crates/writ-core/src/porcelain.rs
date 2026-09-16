//! NUL-delimited `git worktree list --porcelain -z` parser.
//!
//! Git's documented machine contract: `-z` terminates each attribute with NUL,
//! and an empty attribute is the worktree-record boundary. That is what makes
//! newline-containing worktree paths parseable. Callers must keep stdout as
//! bytes; do not UTF-8-lossy-convert or trim before parsing.

use std::path::{Path, PathBuf};

use crate::identity::{BranchRef, CommitId};

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
        paths_equal(&record.path, expected_path)
            && record.branch.as_deref() == Some(expected_branch_ref.as_str())
            && record.head.as_deref() == Some(expected_head.as_str())
    })
}

/// True when any record's worktree path equals `expected_path`.
#[must_use]
pub fn path_is_registered(bytes: &[u8], expected_path: &Path) -> bool {
    parse_porcelain_z(bytes)
        .iter()
        .any(|record| paths_equal(&record.path, expected_path))
}

pub(crate) fn paths_equal(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (
        crate::paths::canonicalize_for_tools(left),
        crate::paths::canonicalize_for_tools(right),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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
            "HEAD" => self.head = Some(lossy_utf8(value)),
            "branch" => self.branch = Some(lossy_utf8(value)),
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
    match attr.iter().position(|b| *b == b' ') {
        Some(idx) => {
            let label = std::str::from_utf8(&attr[..idx]).unwrap_or("");
            (label, Some(&attr[idx + 1..]))
        }
        None => (std::str::from_utf8(attr).unwrap_or(""), None),
    }
}

fn lossy_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn record_bytes(path: &str, head: &str, branch: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"worktree ");
        out.extend_from_slice(path.as_bytes());
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

    #[test]
    fn parses_ordinary_record() {
        let head = "a".repeat(40);
        let bytes = record_bytes("/tmp/hive/job", &head, "refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from("/tmp/hive/job"));
        assert_eq!(records[0].head.as_deref(), Some(head.as_str()));
        assert_eq!(records[0].branch.as_deref(), Some("refs/heads/feature/job"));
    }

    #[test]
    fn newline_in_path_stays_in_one_record() {
        let head = "b".repeat(40);
        let path = "/tmp/base\nsegment/job";
        let bytes = record_bytes(path, &head, "refs/heads/feature/job");
        let records = parse_porcelain_z(&bytes);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, PathBuf::from(path));
        assert!(registration_matches(
            &bytes,
            Path::new(path),
            BranchRef("refs/heads/feature/job"),
            CommitId(&head),
        ));
    }

    #[test]
    fn ignores_unrelated_records_and_rejects_partial_matches() {
        let head = "c".repeat(40);
        let mut bytes = record_bytes("/tmp/other", &head, "refs/heads/feature/job");
        bytes.extend_from_slice(&record_bytes(
            "/tmp/hive/job",
            &head,
            "refs/heads/feature/other",
        ));
        assert!(!registration_matches(
            &bytes,
            Path::new("/tmp/hive/job"),
            BranchRef("refs/heads/feature/job"),
            CommitId(&head),
        ));
        assert!(!path_is_registered(&bytes, Path::new("/tmp/hive")));
    }

    #[test]
    fn empty_input_is_no_records() {
        assert!(parse_porcelain_z(b"").is_empty());
        assert!(parse_porcelain_z(&[0]).is_empty());
    }
}
