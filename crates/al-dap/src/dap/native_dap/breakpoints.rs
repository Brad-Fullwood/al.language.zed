//! setBreakpoints, and the two paths a breakpoint can take to BC.
//!
//! A client sends breakpoints before launch completes, so they are queued per
//! source path and replaced wholesale, which mirrors BC's own full-replace
//! semantics. Once a session exists the queue drains and each row is
//! re-verified with a `breakpoint` event.

use std::path::{Path, PathBuf};

use crate::dap::bc_debug::BcDebugSession;
use crate::dap::Result;

use super::{make_event, write_dap, BpRequest, NativeDapState, PendingBreakpoints, ResolvedObject};

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
    pub(super) async fn handle_set_breakpoints<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let source_path = arguments
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bp_requests = arguments
            .get("breakpoints")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let parsed = parse_bp_requests(&bp_requests);

        // Clone the Arc<BcDebugSession> while holding the session mutex,
        // then drop the guard immediately so no mutex is held across
        // the async breakpoint operations below (prevents deadlock).
        let session_arc = self.session.lock().await.clone();

        let result_bps = if let Some(s) = session_arc {
            // Resolve object type and ID from workspace symbol index.
            match (self.resolve_object)(&source_path) {
                Some(obj) => {
                    self.apply_breakpoints_to_session(
                        &s,
                        &source_path,
                        &parsed,
                        obj.object_type,
                        obj.object_id,
                    )
                    .await
                }
                None => unresolved_object_breakpoints(&source_path, &parsed),
            }
        } else {
            // DAP clients (Zed included) send `setBreakpoints` during the
            // configuration phase — right after `initialized`, well before
            // `launch`/`attach` creates a session. Losing these would mean
            // every breakpoint set before launch silently never fires.
            // Queue them (replacing whatever was queued for this source
            // before) and answer unverified-pending rather than permanently
            // failed; `apply_pending_breakpoints` re-applies and re-verifies
            // them once a session exists.
            self.pending_breakpoints
                .lock()
                .await
                .insert(source_path.clone(), parsed.clone());
            parsed
                .iter()
                .map(|(line, _)| {
                    serde_json::json!({
                        "verified": false,
                        "line": line,
                        "message": "Debug session not started yet; breakpoint will be verified once the session starts",
                    })
                })
                .collect()
        };

        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({"breakpoints": result_bps}),
        )
        .await?;
        Ok(())
    }

    /// Add/replace the full breakpoint set for one source against a *live*
    /// BC session: remove any previously-tracked ids for that source, add
    /// the requested ones, and return each request's DAP `Breakpoint`
    /// result. Shared by `setBreakpoints` (session already exists) and
    /// `apply_pending_breakpoints` (session just started, applying requests
    /// queued while there was none).
    pub(super) async fn apply_breakpoints_to_session(
        &self,
        session: &BcDebugSession,
        source_path: &str,
        bp_requests: &[BpRequest],
        object_type: i32,
        object_id: i32,
    ) -> Vec<serde_json::Value> {
        let mut result_bps = Vec::new();

        // Hold the breakpoints lock for the ENTIRE remove → add → store
        // cycle so two concurrent setBreakpoints calls on the same
        // source_path serialise correctly. Without this hold-across-
        // await (tokio::sync::Mutex makes that safe), both callers
        // would read the same `old_ids`, both remove the same set on
        // BC, both add fresh breakpoints, and one caller's `new_ids`
        // would overwrite the other in the map — leaving the BC
        // server's bp set as the union of both adds but the local map
        // tracking only one half and orphaning the rest.
        let mut bps = self.breakpoints.lock().await;
        let old_ids: Vec<i64> = bps.remove(source_path).unwrap_or_default();
        for id in old_ids {
            if let Err(e) = session.remove_breakpoint(id).await {
                tracing::warn!(
                    breakpoint_id = id,
                    error = %e,
                    "DAP setBreakpoints: removing prior breakpoint failed; \
                     local state will be overwritten regardless"
                );
            }
        }

        let mut new_ids = Vec::new();
        for (line, condition) in bp_requests {
            let server_line = line.saturating_sub(1);

            match session
                .add_breakpoint(object_type, object_id, server_line, 0, condition)
                .await
            {
                Ok(result) => {
                    // BC's add_breakpoint can return Ok(Value::Null) or a
                    // payload without an Id field (bc_debug.rs:945). A
                    // breakpoint id of 0 is not a usable handle: we could
                    // neither remove it on a later setBreakpoints nor honour
                    // a "verified: true" claim. Treat a missing/zero id as a
                    // failure rather than recording an orphaned breakpoint.
                    match extract_breakpoint_id(&result) {
                        Some(bp_id) => {
                            new_ids.push(bp_id);
                            result_bps.push(serde_json::json!({
                                "id": bp_id,
                                "verified": true,
                                "line": line,
                            }));
                        }
                        None => {
                            tracing::warn!(
                                line = line,
                                ?result,
                                "DAP setBreakpoints: BC accepted the breakpoint \
                                         but returned no usable id; not tracking it"
                            );
                            result_bps.push(serde_json::json!({
                                        "verified": false,
                                        "line": line,
                                        "message": "Breakpoint created but ID could not be extracted from BC response",
                                    }));
                        }
                    }
                }
                Err(e) => {
                    result_bps.push(serde_json::json!({
                        "verified": false,
                        "line": line,
                        "message": e.to_string(),
                    }));
                }
            }
        }
        bps.insert(source_path.to_string(), new_ids);
        drop(bps);
        result_bps
    }

    /// Re-apply `setBreakpoints` requests that were queued while no debug
    /// session existed yet (see `handle_set_breakpoints`). Called once after
    /// `launch`/`attach` establishes a session: resolves each queued
    /// source's AL object, adds the breakpoints on BC via the same path
    /// live `setBreakpoints` uses, and emits a `breakpoint` event per
    /// breakpoint so the client updates its initially-unverified rows
    /// (matched by `source.path` + `line` since no `id` was known yet at
    /// the time of the original response).
    pub(super) async fn apply_pending_breakpoints<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        session: &BcDebugSession,
        out: &mut W,
    ) -> Result<()> {
        let pending: PendingBreakpoints = {
            let mut guard = self.pending_breakpoints.lock().await;
            std::mem::take(&mut *guard)
        };

        for (source_path, bp_requests) in pending {
            let result_bps = match (self.resolve_object)(&source_path) {
                Some(obj) => {
                    self.apply_breakpoints_to_session(
                        session,
                        &source_path,
                        &bp_requests,
                        obj.object_type,
                        obj.object_id,
                    )
                    .await
                }
                None => unresolved_object_breakpoints(&source_path, &bp_requests),
            };

            for mut bp in result_bps {
                bp["source"] = serde_json::json!({ "path": &source_path });
                write_dap(
                    out,
                    &make_event(
                        &self.seq,
                        "breakpoint",
                        Some(serde_json::json!({
                            "reason": "changed",
                            "breakpoint": bp,
                        })),
                    ),
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// Pull a usable breakpoint id out of BC's `AddBreakpoint` response.
///
/// Current BC returns `BreakpointId`; older variants used `Id`/`id`. BC may
/// also answer with `Value::Null` or a payload lacking any usable ID
/// (see `bc_debug::add_breakpoint`). An id of `0` is not a valid handle — we
/// could neither later remove it nor truthfully report `verified: true` — so a
/// missing or zero id maps to `None`.
fn extract_breakpoint_id(result: &serde_json::Value) -> Option<i64> {
    result
        .get("BreakpointId")
        .or_else(|| result.get("breakpointId"))
        .or_else(|| result.get("Id"))
        .or_else(|| result.get("id"))
        .and_then(|v| v.as_i64())
        .filter(|&id| id != 0)
}

/// Parse a `setBreakpoints` request's raw `breakpoints` array into
/// `(line, condition)` pairs — the minimal data needed to (re)apply them
/// against a BC session, whether immediately or after queuing.
fn parse_bp_requests(bp_requests: &[serde_json::Value]) -> Vec<BpRequest> {
    bp_requests
        .iter()
        .map(|bp| {
            let line = bp.get("line").and_then(|v| v.as_i64()).unwrap_or(1);
            let condition = bp
                .get("condition")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (line, condition)
        })
        .collect()
}

/// DAP `Breakpoint` results for a source path the workspace index couldn't
/// resolve to an AL object — shared by the live and queued-and-deferred
/// `setBreakpoints` paths.
fn unresolved_object_breakpoints(
    source_path: &str,
    bp_requests: &[BpRequest],
) -> Vec<serde_json::Value> {
    bp_requests
        .iter()
        .map(|(line, _)| {
            serde_json::json!({
                "verified": false, "line": line,
                "message": format!("Could not resolve AL object from workspace index for: {source_path}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    use tokio::sync::{watch, Mutex};

    use crate::dap::framing::read_dap_body;

    use super::super::test_support::*;
    use super::super::VariableHandleStore;

    #[test]
    fn extract_breakpoint_id_reads_pascal_and_camel_case() {
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "BreakpointId": 99 })),
            Some(99)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "breakpointId": 88 })),
            Some(88)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "Id": 42 })),
            Some(42)
        );
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "id": 7 })),
            Some(7)
        );
    }

    #[test]
    fn extract_breakpoint_id_rejects_null_missing_and_zero() {
        // A null response, a payload without an id, and an explicit zero are
        // all unusable handles — recording them would orphan breakpoints on BC
        // and let us falsely report verified: true.
        assert_eq!(extract_breakpoint_id(&serde_json::Value::Null), None);
        assert_eq!(
            extract_breakpoint_id(&serde_json::json!({ "Other": 1 })),
            None
        );
        assert_eq!(extract_breakpoint_id(&serde_json::json!({ "Id": 0 })), None);
        assert_eq!(extract_breakpoint_id(&serde_json::json!({ "id": 0 })), None);
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_reports_unverified() {
        // Breakpoints set before launch/attach (DAP configuration phase)
        // must not be answered as permanently failed — they're queued and
        // will be verified once a session starts (see
        // `set_breakpoints_without_session_are_queued_for_later_verification`).
        let (_, frames) = run_request(
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Foo.al"},
                "breakpoints": [{"line": 10}, {"line": 20}],
            }),
        )
        .await;
        let bps = frames[0]["body"]["breakpoints"]
            .as_array()
            .expect("breakpoints array");
        assert_eq!(bps.len(), 2);
        for bp in bps {
            assert_eq!(bp["verified"], false);
            assert!(bp.get("id").is_none(), "no BC id exists yet: {bp:?}");
            assert_eq!(
                bp["message"],
                "Debug session not started yet; breakpoint will be verified once the session starts"
            );
        }
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_are_queued_and_reverified_on_launch() {
        // Regression for the pre-session setBreakpoints finding: breakpoints
        // set during DAP configuration (before launch/attach) must be
        // queued, not lost, and re-applied/re-verified once a session
        // starts.
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        std::mem::forget(_dap_event_rx);
        let state: TestState = NativeDapState {
            seq: Arc::new(AtomicU64::new(1)),
            session: Arc::new(Mutex::new(None)),
            debug_config: Arc::new(Mutex::new(None)),
            breakpoints: Arc::new(Mutex::new(HashMap::new())),
            pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
            configured: Arc::new(Mutex::new(false)),
            variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
            cancel_tx,
            cancel_rx,
            dap_event_tx,
            project_root: "/proj".to_string(),
            acquire_token: no_token,
            resolve_object: resolve_foo_al,
            resolve_path: |_: i32, _: i32| -> Option<PathBuf> { None },
            compile: no_compile,
            find_app: |_| Ok(None),
        };

        // 1. setBreakpoints arrives before any session exists.
        let (_, frames) = run_request_on(
            &state,
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Foo.al"},
                "breakpoints": [{"line": 10}, {"line": 20, "condition": "X > 1"}],
            }),
        )
        .await;
        let bps = frames[0]["body"]["breakpoints"].as_array().unwrap();
        assert_eq!(bps.len(), 2);
        assert!(bps.iter().all(|bp| bp["verified"] == false));
        assert_eq!(
            state
                .pending_breakpoints
                .lock()
                .await
                .get("/proj/src/Foo.al")
                .cloned(),
            Some(vec![(10, String::new()), (20, "X > 1".to_string())]),
            "queued exactly the requested (line, condition) pairs"
        );

        // 2. A session "starts" — drive `apply_pending_breakpoints` directly,
        // exactly as `handle_launch_attach` does right after connect/attach
        // succeed, against a fake BC hub.
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        fake.reply_ok("AddBreakpoint", serde_json::json!({ "Id": 501 }));
        fake.reply_ok("AddBreakpoint", serde_json::json!({ "Id": 502 }));

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        state
            .apply_pending_breakpoints(&session, &mut client)
            .await
            .expect("apply_pending_breakpoints must not error");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.unwrap();
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut events: Vec<serde_json::Value> = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            events.push(serde_json::from_slice(&body).expect("valid JSON frame"));
        }

        assert_eq!(
            events.len(),
            2,
            "one breakpoint event per queued breakpoint: {events:?}"
        );
        for event in &events {
            assert_eq!(event["event"], "breakpoint");
            assert_eq!(event["body"]["reason"], "changed");
            assert_eq!(event["body"]["breakpoint"]["verified"], true);
            assert_eq!(
                event["body"]["breakpoint"]["source"]["path"],
                "/proj/src/Foo.al"
            );
        }
        let lines: Vec<i64> = events
            .iter()
            .map(|e| e["body"]["breakpoint"]["line"].as_i64().unwrap())
            .collect();
        assert_eq!(lines, vec![10, 20], "verification events preserve order");

        // The pending queue is drained and the newly-added BC ids are now
        // tracked under `breakpoints` for future replace/remove.
        assert!(state.pending_breakpoints.lock().await.is_empty());
        assert_eq!(
            state
                .breakpoints
                .lock()
                .await
                .get("/proj/src/Foo.al")
                .cloned(),
            Some(vec![501, 502])
        );
    }

    #[tokio::test]
    async fn set_breakpoints_without_session_unresolvable_object_is_still_queued() {
        // A source path the workspace index can't resolve yet (e.g. still
        // indexing) must still queue rather than drop the request — it may
        // resolve by the time the session starts.
        let state = test_state(); // resolve_object always returns None
        let (_, frames) = run_request_on(
            &state,
            "setBreakpoints",
            serde_json::json!({
                "source": {"path": "/proj/src/Unresolvable.al"},
                "breakpoints": [{"line": 5}],
            }),
        )
        .await;
        assert_eq!(frames[0]["body"]["breakpoints"][0]["verified"], false);
        assert_eq!(
            state
                .pending_breakpoints
                .lock()
                .await
                .get("/proj/src/Unresolvable.al")
                .cloned(),
            Some(vec![(5, String::new())])
        );
    }
}
