//! Versioned JSON request and response types.
//!
//! The schema-version 1 contract is implemented by GitHub #40.

use serde::Serialize;

/// Schema version for the cross-language CLI envelope.
pub const SCHEMA_VERSION: u8 = 1;
/// Schema version for the exact-base worktree-create request and response boundary.
pub const EXACT_BASE_SCHEMA_VERSION: u8 = 2;

/// Empty JSON object payload for scaffold responses.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize)]
pub struct EmptyData {}

/// Structured error payload reserved for later command failures.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct ErrorData {
    /// Stable error code for machine classification.
    pub code: String,
    /// Human-readable error summary.
    pub message: String,
}

/// Shared JSON envelope for CLI responses.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub struct Response<T> {
    /// Whether the command completed successfully.
    pub ok: bool,
    /// Explicitly selected boundary version.
    pub schema_version: u8,
    /// Machine-readable command identifier.
    pub command: &'static str,
    /// Command payload.
    pub data: T,
    /// Structured error payload, or `null` on success.
    pub error: Option<ErrorData>,
}

impl Response<EmptyData> {
    /// Build the scaffold success envelope emitted by `writ --json`.
    #[must_use]
    pub fn bootstrap_success() -> Self {
        Self {
            ok: true,
            schema_version: SCHEMA_VERSION,
            command: "cli.bootstrap",
            data: EmptyData::default(),
            error: None,
        }
    }
}

impl<T> Response<T> {
    /// Build a generic success envelope for the given command and payload.
    #[must_use]
    pub fn success(command: &'static str, data: T) -> Self {
        Self::success_with_schema(command, data, SCHEMA_VERSION)
    }

    /// Build a success envelope for an explicitly selected boundary schema.
    #[must_use]
    pub fn success_with_schema(command: &'static str, data: T, schema_version: u8) -> Self {
        Self {
            ok: true,
            schema_version,
            command,
            data,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EXACT_BASE_SCHEMA_VERSION, Response};

    #[test]
    fn bootstrap_response_serializes_to_v1_envelope() {
        let json = serde_json::to_string(&Response::bootstrap_success()).unwrap();

        assert_eq!(
            json,
            r#"{"ok":true,"schema_version":1,"command":"cli.bootstrap","data":{},"error":null}"#
        );
    }

    #[test]
    fn exact_base_response_serializes_to_selected_v2_envelope() {
        let response = Response::success_with_schema(
            "worktree.create",
            serde_json::json!({"worktree_registered": true}),
            EXACT_BASE_SCHEMA_VERSION,
        );

        assert_eq!(response.schema_version, 2);
    }
}
