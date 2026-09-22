//! stackTrace: the BC call stack as DAP frames.
//!
//! BC returns the whole stack, so the `startFrame` and `levels` arguments are
//! applied here rather than asked of the server.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::dap::Result;

use super::{NativeDapState, ResolvedObject};

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send,
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
            match s.get_call_stack().await {
                Ok(frames) => bc_stack_to_dap(frames, &self.resolve_path),
                Err(e) => {
                    debug!("get_call_stack failed: {e}");
                    Vec::new()
                }
            }
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

fn bc_stack_to_dap<P>(frames: serde_json::Value, resolve_path: &P) -> Vec<serde_json::Value>
where
    P: Fn(i32, i32) -> Option<PathBuf>,
{
    let arr = match frames.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .enumerate()
        .map(|(i, frame)| {
            let display_name = frame
                .get("DisplayName")
                .or_else(|| frame.get("displayName"))
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();

            // Current BC servers send `StatementSpan.From` instead of
            // `SourcePosition`; prefer it when present (see the fixture at
            // native_debug.rs:905). Both carry 0-based line/column — DAP
            // `stackFrame.line`/`column` are 1-based, so add 1 the same way
            // `parse_bc_stack` (native_debug.rs:527) does. Without the +1
            // Zed highlights one line above the actual stop; without the
            // StatementSpan fallback, servers that only send it report
            // line/column 0.
            let position = frame
                .get("StatementSpan")
                .or_else(|| frame.get("statementSpan"))
                .and_then(|span| span.get("From").or_else(|| span.get("from")))
                .or_else(|| {
                    frame
                        .get("SourcePosition")
                        .or_else(|| frame.get("sourcePosition"))
                });

            let line = position
                .and_then(|sp| sp.get("Line").or_else(|| sp.get("line")))
                .and_then(|v| v.as_i64())
                .map(|v| v.saturating_add(1))
                .unwrap_or(0);

            let col = position
                .and_then(|sp| sp.get("Column").or_else(|| sp.get("column")))
                .and_then(|v| v.as_i64())
                .map(|v| v.saturating_add(1))
                .unwrap_or(0);

            let object_type = frame
                .get("ApplicationObjectId")
                .and_then(|oid| oid.get("ObjectType").or_else(|| oid.get("objectType")))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);

            let object_number = frame
                .get("ApplicationObjectId")
                .and_then(|oid| oid.get("ObjectNumber").or_else(|| oid.get("objectNumber")))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);

            let source = match (object_type, object_number) {
                (Some(ot), Some(on)) => {
                    resolve_path(ot, on).map(|p| serde_json::json!({ "path": p.to_string_lossy() }))
                }
                _ => None,
            };

            let mut val = serde_json::json!({
                "id": i as i64,
                "name": display_name,
                "line": line,
                "column": col,
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

    #[test]
    fn bc_stack_to_dap_includes_source_when_path_resolves() {
        let frames = serde_json::json!([{
            "DisplayName": "MyCodeunit.OnRun",
            "SourcePosition": { "Line": 10, "Column": 4 },
            "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 50100 }
        }]);
        let resolve = |ot: i32, on: i32| -> Option<PathBuf> {
            if ot == bc_object_type::CODEUNIT && on == 50100 {
                Some(PathBuf::from("/workspace/src/MyCodeunit.al"))
            } else {
                None
            }
        };
        let result = bc_stack_to_dap(frames, &resolve);
        assert_eq!(result.len(), 1);
        let frame = &result[0];
        assert_eq!(frame["name"], "MyCodeunit.OnRun");
        // BC's SourcePosition is 0-based; DAP stackFrame line/column are 1-based.
        assert_eq!(frame["line"], 11);
        assert_eq!(frame["column"], 5);
        assert_eq!(
            frame["source"]["path"].as_str().unwrap_or(""),
            "/workspace/src/MyCodeunit.al"
        );
    }

    #[test]
    fn bc_stack_to_dap_prefers_statement_span_from_over_source_position() {
        // Current BC servers send StatementSpan.From instead of
        // SourcePosition. When both are present StatementSpan.From wins;
        // when only StatementSpan.From is present it must still be honored
        // (not fall back to the line/column-0 default).
        let frames = serde_json::json!([{
            "DisplayName": "MyCodeunit.OnRun",
            "StatementSpan": { "From": { "Line": 42, "Column": 8 } },
            "SourcePosition": { "Line": 0, "Column": 0 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result[0]["line"], 43);
        assert_eq!(result[0]["column"], 9);
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
    fn bc_stack_to_dap_statement_span_only_still_converts_to_one_based() {
        let frames = serde_json::json!([{
            "MethodName": "RunProbe - OnAction",
            "StatementSpan": { "From": { "Line": 42, "Column": 8 } }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result[0]["line"], 43);
        assert_eq!(result[0]["column"], 9);
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_path_unknown() {
        let frames = serde_json::json!([{
            "DisplayName": "UnknownObject.Trigger",
            "SourcePosition": { "Line": 1, "Column": 0 },
            "ApplicationObjectId": { "ObjectType": 5, "ObjectNumber": 99999 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].get("source").is_none(),
            "source should be absent when resolve_path returns None"
        );
    }

    #[test]
    fn bc_stack_to_dap_handles_missing_application_object_id() {
        let frames = serde_json::json!([{
            "DisplayName": "SomeProc",
            "SourcePosition": { "Line": 5, "Column": 0 }
        }]);
        let result = bc_stack_to_dap(frames, &|_ot: i32, _on: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(result[0].get("source").is_none());
        assert_eq!(result[0]["name"], "SomeProc");
    }

    #[test]
    fn bc_stack_to_dap_returns_empty_for_non_array_input() {
        let result = bc_stack_to_dap(serde_json::json!(null), &|_: i32, _: i32| None::<PathBuf>);
        assert!(result.is_empty(), "non-array input must yield empty vec");
    }

    #[test]
    fn bc_stack_to_dap_reads_camelcase_inner_keys() {
        // The mapper reads the *inner* position/object keys with a camelCase
        // fallback (Line→line, Column→column, ObjectType→objectType, etc.).
        // The outer container keys are still PascalCase (SourcePosition,
        // ApplicationObjectId); DisplayName has its own displayName fallback.
        let frames = serde_json::json!([{
            "displayName": "CamelProc",
            "SourcePosition": { "line": 12, "column": 3 },
            "ApplicationObjectId": { "objectType": 5, "objectNumber": 50100 }
        }]);
        let resolve = |ot: i32, on: i32| -> Option<PathBuf> {
            if ot == bc_object_type::CODEUNIT && on == 50100 {
                Some(PathBuf::from("/ws/Cu.al"))
            } else {
                None
            }
        };
        let result = bc_stack_to_dap(frames, &resolve);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "CamelProc");
        assert_eq!(result[0]["line"], 13);
        assert_eq!(result[0]["column"], 4);
        assert_eq!(
            result[0]["source"]["path"].as_str().unwrap_or(""),
            "/ws/Cu.al"
        );
    }

    #[test]
    fn bc_stack_to_dap_assigns_sequential_frame_ids() {
        // DAP stackTrace frame ids must be the array index so scopes/variables
        // can map a frameId back to the BC stack frame. A regression that reused
        // a constant id would break per-frame variable inspection.
        let frames = serde_json::json!([
            { "DisplayName": "Top", "SourcePosition": { "Line": 1, "Column": 0 } },
            { "DisplayName": "Mid", "SourcePosition": { "Line": 2, "Column": 0 } },
            { "DisplayName": "Bottom", "SourcePosition": { "Line": 3, "Column": 0 } },
        ]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["id"], 0);
        assert_eq!(result[1]["id"], 1);
        assert_eq!(result[2]["id"], 2);
        assert_eq!(result[0]["name"], "Top");
        assert_eq!(result[2]["name"], "Bottom");
    }

    #[test]
    fn bc_stack_to_dap_defaults_missing_fields() {
        // A frame missing DisplayName / SourcePosition must not panic and must
        // fall back to documented defaults: "(unknown)" name, line 0, column 0.
        let frames = serde_json::json!([{}]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "(unknown)");
        assert_eq!(result[0]["line"], 0);
        assert_eq!(result[0]["column"], 0);
        assert!(result[0].get("source").is_none());
    }

    #[test]
    fn bc_stack_to_dap_empty_array_yields_empty_vec() {
        let result = bc_stack_to_dap(serde_json::json!([]), &|_: i32, _: i32| None::<PathBuf>);
        assert!(result.is_empty());
    }

    #[test]
    fn bc_stack_to_dap_omits_source_when_only_object_type_present() {
        // resolve_path is only consulted when BOTH object_type and object_number
        // are present. A frame with object_type but no object_number must not
        // resolve a source (the (Some, None) match arm yields None).
        let frames = serde_json::json!([{
            "DisplayName": "Partial",
            "SourcePosition": { "Line": 1, "Column": 0 },
            "ApplicationObjectId": { "ObjectType": 5 }
        }]);
        let result = bc_stack_to_dap(frames, &|_: i32, _: i32| None::<PathBuf>);
        assert_eq!(result.len(), 1);
        assert!(
            result[0].get("source").is_none(),
            "source must be absent when object_number is missing"
        );
    }

    #[tokio::test]
    async fn stack_trace_without_session_returns_empty_frames() {
        let (_, frames) = run_request("stackTrace", serde_json::json!({"threadId": 1})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["totalFrames"], 0);
    }
}
