//! Live BC debug session adapter for the snapshot recorder.
//!
//! [`BcDebugSessionAdapter`] wraps a [`BcDebugSession`] and implements
//! [`DebuggerSession`] so the snapshot recorder can drive a real BC instance
//! without knowing about SignalR internals.
//!
//! ## Object metadata extraction
//!
//! `add_breakpoint(file, line)` must translate a file path + line number into a
//! BC `{ObjectType, ObjectNumber}` pair before calling the SignalR hub. It does
//! this by reading the AL source, parsing it with the tree-sitter parser, and
//! calling `syntax::find_object_declaration`.
//!
//! ## wait_for_break design
//!
//! `wait_for_break` delegates to `BcDebugSession::wait_for_break_event()`, which
//! blocks on a dedicated unbounded channel populated by the WebSocket reader task.
//! No polling loop, no sleep, no 30-second cap. Events buffered before the call
//! are returned immediately from the channel.

use std::sync::Arc;

use tracing::debug;

use al_dap::dap::bc_debug::{BcDebugConfig, BcDebugSession};
use al_dap::dap::native_dap::kind_to_object_type;
use al_syntax::{find_object_declaration, AlParser};

use super::replayer::{DebuggerSession, ReplayerError};

pub struct BcDebugSessionAdapter {
    session: Arc<BcDebugSession>,
    config: BcDebugConfig,
}

impl BcDebugSessionAdapter {
    pub fn new(session: Arc<BcDebugSession>, config: BcDebugConfig) -> Self {
        Self { session, config }
    }
}

/// Parse an AL source file to extract the object kind and numeric ID.
///
/// Returns `(kind_lowercase, object_id)` or an error if the file cannot be
/// read or the object declaration cannot be found.
pub fn extract_object_metadata(file: &str) -> Result<(String, i32), ReplayerError> {
    let source = std::fs::read_to_string(file)?;
    let result = AlParser::parse_quick(&source);
    let obj = find_object_declaration(&result.tree, &source)
        .ok_or_else(|| ReplayerError::Parse(format!("no object declaration found in {file}")))?;
    let id = obj
        .id
        .ok_or_else(|| ReplayerError::Parse(format!("object in {file} has no numeric ID")))?
        as i32;
    Ok((obj.kind, id))
}

impl DebuggerSession for BcDebugSessionAdapter {
    async fn add_breakpoint(&self, file: &str, line: u32) -> Result<u32, ReplayerError> {
        let (kind, object_id) = extract_object_metadata(file)?;
        let object_type = kind_to_object_type(&kind);

        debug!(
            file = file,
            line = line,
            kind = %kind,
            object_type = object_type,
            object_id = object_id,
            "BcDebugSessionAdapter: add_breakpoint"
        );

        let response = self
            .session
            .add_breakpoint(object_type, object_id, line as i64, 0, "")
            .await
            .map_err(|e| ReplayerError::Session(format!("add_breakpoint failed: {e}")))?;

        // BC returns a JSON object with the assigned breakpoint ID.
        // The field is typically "Id" (PascalCase, Newtonsoft.Json convention).
        let bp_id = response
            .get("Id")
            .or_else(|| response.get("id"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        Ok(bp_id)
    }

    async fn configuration_done(&self) -> Result<(), ReplayerError> {
        self.session
            .configuration_done(&self.config)
            .await
            .map_err(|e| ReplayerError::Session(format!("configuration_done failed: {e}")))
    }

    /// Wait for the next BC `Break` event.
    ///
    /// Delegates to [`BcDebugSession::wait_for_break_event`], which blocks on a
    /// dedicated channel populated by the WebSocket reader task. Returns `true`
    /// if execution stopped at a breakpoint and `false` if the session ended.
    async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
        Ok(self.session.wait_for_break_event().await)
    }

    async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
        self.session
            .get_variables(0)
            .await
            .map_err(|e| ReplayerError::Session(format!("get_variables failed: {e}")))
    }

    async fn continue_execution(&self) -> Result<(), ReplayerError> {
        self.session
            .continue_execution(serde_json::json!({}))
            .await
            .map_err(|e| ReplayerError::Session(format!("continue_execution failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_object_metadata_codeunit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("MyCu.al");
        std::fs::write(&file, r#"codeunit 50100 "My Cu" { }"#).unwrap();

        let (kind, id) = extract_object_metadata(file.to_str().unwrap())
            .expect("should parse codeunit metadata");

        let bc_type = kind_to_object_type(&kind);
        assert_eq!(
            bc_type,
            al_dap::dap::native_dap::bc_object_type::CODEUNIT,
            "codeunit kind={kind} should map to BC CODEUNIT"
        );
        assert_eq!(id, 50100, "object id should be 50100");
    }

    #[test]
    fn extract_object_metadata_table() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("MyTable.al");
        std::fs::write(&file, r#"table 1234 "My Table" { fields { } }"#).unwrap();

        let (kind, id) =
            extract_object_metadata(file.to_str().unwrap()).expect("should parse table metadata");

        let bc_type = kind_to_object_type(&kind);
        assert_eq!(
            bc_type,
            al_dap::dap::native_dap::bc_object_type::TABLE,
            "table kind={kind} should map to BC TABLE"
        );
        assert_eq!(id, 1234);
    }

    #[test]
    fn extract_object_metadata_missing_file_returns_error() {
        let result = extract_object_metadata("/nonexistent/path/to/file.al");
        assert!(result.is_err(), "should fail for missing file");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ReplayerError::Io(_)),
            "expected Io error, got: {err}"
        );
    }

    #[test]
    fn extract_object_metadata_no_declaration_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Empty.al");
        std::fs::write(&file, "// just a comment\n").unwrap();

        let result = extract_object_metadata(file.to_str().unwrap());
        assert!(result.is_err(), "should fail when no object declaration");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ReplayerError::Parse(_)),
            "expected Parse error, got: {err}"
        );
    }

    /// Confirm `ReplayerError::NotYetWired` displays correctly — the variant is
    /// kept for callers that may still produce it, even though `wait_for_break`
    /// no longer does.
    #[test]
    fn replayer_error_not_yet_wired_display() {
        let err = ReplayerError::NotYetWired("test gap".to_string());
        assert!(
            err.to_string().contains("not yet wired"),
            "display should mention 'not yet wired': {err}"
        );
    }
}
