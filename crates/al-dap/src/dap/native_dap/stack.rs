//! stackTrace: the BC call stack as DAP frames.
//!
//! The frames come from the last `Break` callback kept in the session (BC has
//! no stack method). The callback carries the whole stack, so the `startFrame` and `levels` arguments are
//! applied here rather than asked of the server.

use std::path::{Path, PathBuf};

use crate::dap::bc_debug::position::{
    debugger_column_to_client, debugger_line_to_client, ClientBase,
};
use crate::dap::Result;

use super::{NativeDapState, ResolvedObject, ResolvedSource};

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    R: Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<ResolvedSource> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    pub(super) async fn handle_stack_trace<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        let stack_frames = if let Some(s) = session_arc {
            bc_stack_to_dap(s.call_stack().await, &self.resolve_path, &self.client_base)
        } else {
            Vec::new()
        };
        let (page, total) = page_stack_frames(stack_frames, arguments);
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({
                "stackFrames": page,
                "totalFrames": total,
            }),
        )
        .await?;
        Ok(())
    }
}

/// Slice a full DAP stack-frame list per the `stackTrace` request's
/// `startFrame`/`levels` arguments and return `(page, totalFrames)`.
///
/// Per the DAP spec: `startFrame` defaults to 0, and `levels` of 0 or absent
/// means "all remaining frames from `startFrame`". `totalFrames` is always
/// the *full* stack length regardless of paging, so a delayed-stack-trace
/// client knows how many more frames it can page in.
fn page_stack_frames(
    frames: Vec<serde_json::Value>,
    arguments: &serde_json::Value,
) -> (Vec<serde_json::Value>, usize) {
    let total = frames.len();
    let start_frame = arguments
        .get("startFrame")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0) as usize;
    let levels = arguments
        .get("levels")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if start_frame >= total {
        return (Vec::new(), total);
    }
    let end = if levels <= 0 {
        total
    } else {
        start_frame.saturating_add(levels as usize).min(total)
    };
    (frames[start_frame..end].to_vec(), total)
}

/// Map BC `StackFrame` objects (StackFrame.cs: `MethodName`, `ObjectId`,
/// `ObjectName`, `StatementSpan`) to DAP stack frames, as
/// DebugAdapterStackTraceHandler.cs:101-104 does. Line and column conversion
/// follows the client's `linesStartAt1` / `columnsStartAt1`.
fn bc_stack_to_dap<P>(
    frames: serde_json::Value,
    resolve_path: &P,
    base: &ClientBase,
) -> Vec<serde_json::Value>
where
    P: Fn(i32, i32) -> Option<ResolvedSource>,
{
    let arr = match frames.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let lines_one = base.lines_start_at_1();
    let columns_one = base.columns_start_at_1();
    let number = |value: Option<&serde_json::Value>, key: &str| {
        value.and_then(|v| v.get(key)).and_then(|v| v.as_i64())
    };

    arr.iter()
        .enumerate()
        .map(|(i, frame)| {
            let name = frame
                .get("MethodName")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();

            let span = frame.get("StatementSpan");
            let from = span.and_then(|s| s.get("From"));
            let to = span.and_then(|s| s.get("To"));

            let object_id = frame.get("ObjectId");
            let resolved = match (
                number(object_id, "ObjectType"),
                number(object_id, "ObjectNumber"),
            ) {
                (Some(ot), Some(on)) => resolve_path(ot as i32, on as i32),
                _ => None,
            };
            // DebugAdapterStackTraceHandler.cs:80-101 takes the object's offset
            // off the frame line when the file holds more than one object.
            let shift = resolved.as_ref().map_or(0, |r| r.line_shift);
            let source = resolved
                .as_ref()
                .map(|r| serde_json::json!({ "path": r.path.to_string_lossy() }));

            let mut val = serde_json::json!({
                "id": i as i64,
                "name": name,
                "line": debugger_line_to_client(number(from, "Line").unwrap_or(0), lines_one) - shift,
                "column": debugger_column_to_client(number(from, "Column").unwrap_or(0), columns_one),
                "endLine": debugger_line_to_client(number(to, "Line").unwrap_or(0), lines_one) - shift,
                "endColumn": debugger_column_to_client(number(to, "Column").unwrap_or(0), columns_one),
            });
            if let Some(src) = source {
                val["source"] = src;
            }
            val
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::bc_object_type;

    use super::super::test_support::*;

    use crate::dap::bc_debug::test_fixtures::stack_frame;

    fn source(path: &str, line_shift: i64) -> ResolvedSource {
        ResolvedSource {
            path: PathBuf::from(path),
            line_shift,
        }
    }

    // DebugAdapterStackTraceHandler.cs:80-101: a file with two objects gets the
    // object's offset taken off BC's line. BC's row 4 is file row 8 (shift -4).
    #[test]
    fn bc_stack_to_dap_takes_the_object_line_shift_off() {
        let frames = serde_json::json!([stack_frame("Post", (4, 8), (4, 20))]);
        let resolve = |_: i32, _: i32| Some(source("/ws/Two.al", -4));
        let result = bc_stack_to_dap(frames, &resolve, &ClientBase::default());
        assert_eq!(result[0]["line"], 9);
        assert_eq!(result[0]["endLine"], 9);
    }

    fn no_path(_: i32, _: i32) -> Option<ResolvedSource> {
        None
    }

    // fixture source: see test_fixtures::stack_frame (StackFrame.cs:34-58)
    #[test]
    fn bc_stack_to_dap_includes_source_when_path_resolves() {
        let frames = serde_json::json!([stack_frame("OnRun", (41, 8), (43, 30))]);
        let resolve = |ot: i32, on: i32| -> Option<ResolvedSource> {
            if ot == bc_object_type::CODEUNIT && on == 50100 {
                Some(source("/workspace/src/MyCodeunit.al", 0))
            } else {
                None
            }
        };
        let result = bc_stack_to_dap(frames, &resolve, &ClientBase::default());
        assert_eq!(result.len(), 1);
        let frame = &result[0];
        assert_eq!(frame["name"], "OnRun");
        // DebugHelper.cs:105-121: a one-based client gets line and column + 1
        // for values above 0. StatementSpan.To fills endLine and endColumn
        // (DebugAdapterStackTraceHandler.cs:101-104).
        assert_eq!(frame["line"], 42);
        assert_eq!(frame["column"], 9);
        assert_eq!(frame["endLine"], 44);
        assert_eq!(frame["endColumn"], 31);
        assert_eq!(
            frame["source"]["path"].as_str().unwrap_or(""),
            "/workspace/src/MyCodeunit.al"
        );
    }

    #[test]
    fn break_callback_frames_become_named_frames_with_a_source_path() {
        // fixture source: test_fixtures::break_args (HubBasedDebuggerService.cs:393)
        let args = crate::dap::bc_debug::test_fixtures::break_args(
            vec![
                stack_frame("OnRun", (41, 8), (41, 30)),
                stack_frame("Caller", (9, 4), (9, 20)),
            ],
            "",
        );
        let resolve = |ot: i32, on: i32| -> Option<ResolvedSource> {
            (ot == bc_object_type::CODEUNIT && on == 50100).then(|| source("/ws/Probe.al", 0))
        };
        let result = bc_stack_to_dap(args[1].clone(), &resolve, &ClientBase::default());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["name"], "OnRun");
        assert_eq!(result[1]["name"], "Caller");
        assert_eq!(result[1]["source"]["path"], "/ws/Probe.al");
        assert_eq!(result[1]["line"], 10);
    }

    #[test]
    fn bc_stack_to_dap_keeps_debugger_numbers_for_a_zero_based_client() {
        let base = ClientBase::default();
        base.set_from_initialize(&serde_json::json!({
            "linesStartAt1": false,
            "columnsStartAt1": false
        }));
        let frames = serde_json::json!([stack_frame("OnRun", (41, 8), (43, 30))]);
        let result = bc_stack_to_dap(frames, &no_path, &base);
        assert_eq!(result[0]["line"], 41);
        assert_eq!(result[0]["column"], 8);
    }

    #[test]
    fn bc_stack_to_dap_leaves_line_zero_at_zero() {
        // DebugHelper.cs:107: the +1 applies only when the line is above 0.
        let frames = serde_json::json!([stack_frame("OnRun", (0, 0), (0, 0))]);
        let result = bc_stack_to_dap(frames, &no_path, &ClientBase::default());
        assert_eq!(result[0]["line"], 0);
        assert_eq!(result[0]["column"], 0);
    }

    #[test]
    fn bc_stack_to_dap_ignores_fields_the_bc_type_does_not_have() {
        // StackFrame.cs has no DisplayName, ApplicationObjectId or SourcePosition.
        let frames = serde_json::json!([{
            "DisplayName": "Invented",
            "SourcePosition": { "Line": 9, "Column": 9 },
            "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 50100 }
        }]);
        let resolve = |_: i32, _: i32| Some(source("/x.al", 0));
        let result = bc_stack_to_dap(frames, &resolve, &ClientBase::default());
        assert_eq!(result[0]["name"], "(unknown)");
        assert_eq!(result[0]["line"], 0);
        assert!(result[0].get("source").is_none());
    }

    #[test]
    fn page_stack_frames_honors_start_frame_and_levels() {
        let frames: Vec<serde_json::Value> =
            (0..10).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) =
            page_stack_frames(frames, &serde_json::json!({ "startFrame": 2, "levels": 3 }));
        assert_eq!(total, 10, "totalFrames must report the full stack size");
        assert_eq!(page.len(), 3);
        assert_eq!(page[0]["id"], 2);
        assert_eq!(page[2]["id"], 4);
    }

    #[test]
    fn page_stack_frames_defaults_to_full_stack_when_arguments_absent() {
        let frames: Vec<serde_json::Value> =
            (0..5).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(frames, &serde_json::json!({}));
        assert_eq!(total, 5);
        assert_eq!(page.len(), 5, "no startFrame/levels means the whole stack");
    }

    #[test]
    fn page_stack_frames_zero_levels_means_all_remaining() {
        let frames: Vec<serde_json::Value> =
            (0..5).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) =
            page_stack_frames(frames, &serde_json::json!({ "startFrame": 3, "levels": 0 }));
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2, "levels=0 means all frames from startFrame");
        assert_eq!(page[0]["id"], 3);
    }

    #[test]
    fn page_stack_frames_start_frame_past_end_yields_empty_page() {
        let frames: Vec<serde_json::Value> =
            (0..3).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(
            frames,
            &serde_json::json!({ "startFrame": 10, "levels": 5 }),
        );
        assert_eq!(total, 3);
        assert!(page.is_empty());
    }

    #[test]
    fn page_stack_frames_levels_beyond_end_clamps_to_total() {
        let frames: Vec<serde_json::Value> =
            (0..3).map(|i| serde_json::json!({ "id": i })).collect();
        let (page, total) = page_stack_frames(
            frames,
            &serde_json::json!({ "startFrame": 1, "levels": 100 }),
        );
        assert_eq!(total, 3);
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_path_unknown() {
        let frames = serde_json::json!([stack_frame("OnRun", (1, 0), (1, 5))]);
        let result = bc_stack_to_dap(frames, &no_path, &ClientBase::default());
        assert_eq!(result.len(), 1);
        assert!(
            result[0].get("source").is_none(),
            "source should be absent when resolve_path returns None"
        );
    }

    #[test]
    fn bc_stack_to_dap_handles_missing_object_id() {
        let mut frame = stack_frame("SomeProc", (5, 0), (5, 3));
        frame.as_object_mut().unwrap().remove("ObjectId");
        let result = bc_stack_to_dap(serde_json::json!([frame]), &no_path, &ClientBase::default());
        assert_eq!(result.len(), 1);
        assert!(result[0].get("source").is_none());
        assert_eq!(result[0]["name"], "SomeProc");
    }

    #[test]
    fn bc_stack_to_dap_returns_empty_for_non_array_input() {
        let result = bc_stack_to_dap(serde_json::json!(null), &no_path, &ClientBase::default());
        assert!(result.is_empty(), "non-array input must yield empty vec");
    }

    #[test]
    fn bc_stack_to_dap_assigns_sequential_frame_ids() {
        // DAP stackTrace frame ids must be the array index so scopes/variables
        // can map a frameId back to the BC stack frame.
        let frames = serde_json::json!([
            stack_frame("Top", (1, 0), (1, 1)),
            stack_frame("Mid", (2, 0), (2, 1)),
            stack_frame("Bottom", (3, 0), (3, 1)),
        ]);
        let result = bc_stack_to_dap(frames, &no_path, &ClientBase::default());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["id"], 0);
        assert_eq!(result[1]["id"], 1);
        assert_eq!(result[2]["id"], 2);
        assert_eq!(result[0]["name"], "Top");
        assert_eq!(result[2]["name"], "Bottom");
    }

    #[test]
    fn bc_stack_to_dap_defaults_missing_fields() {
        let frames = serde_json::json!([{}]);
        let result = bc_stack_to_dap(frames, &no_path, &ClientBase::default());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "(unknown)");
        assert_eq!(result[0]["line"], 0);
        assert_eq!(result[0]["column"], 0);
        assert!(result[0].get("source").is_none());
    }

    #[test]
    fn bc_stack_to_dap_empty_array_yields_empty_vec() {
        let result = bc_stack_to_dap(serde_json::json!([]), &no_path, &ClientBase::default());
        assert!(result.is_empty());
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_only_object_type_present() {
        // resolve_path is only consulted when both ObjectType and ObjectNumber are present.
        let mut frame = stack_frame("Partial", (1, 0), (1, 1));
        frame["ObjectId"] = serde_json::json!({ "ObjectType": 5 });
        let resolve = |_: i32, _: i32| Some(source("/x.al", 0));
        let result = bc_stack_to_dap(serde_json::json!([frame]), &resolve, &ClientBase::default());
        assert!(
            result[0].get("source").is_none(),
            "source must be absent when ObjectNumber is missing"
        );
    }

    #[tokio::test]
    async fn stack_trace_without_session_returns_empty_frames() {
        let (_, frames) = run_request("stackTrace", serde_json::json!({"threadId": 1})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["totalFrames"], 0);
    }
}
