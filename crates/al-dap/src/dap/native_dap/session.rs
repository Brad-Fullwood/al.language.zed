//! Initialize, configurationDone, launch, attach and disconnect.
//!
//! Launch and attach share one handler because the only difference is whether
//! a compile and publish runs first. `configurationDone` can be rejected by BC
//! until the client has attached, so `try_configuration_done` is what the
//! event forwarder retries with.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::dap::bc_debug::{get_web_endpoint, publish_app, BcDebugConfig, BcDebugSession};
use crate::dap::DapError;
use crate::dap::Result;

use super::browser::{build_debug_browser_url, open_browser};
use super::{make_event, make_response, write_dap, NativeDapState, ResolvedObject};

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
    pub(super) async fn handle_initialize<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let resp = make_response(
            &self.seq,
            request_seq,
            command,
            true,
            Some(serde_json::json!({
                "supportsConfigurationDoneRequest": true,
                "supportsFunctionBreakpoints": false,
                "supportsConditionalBreakpoints": true,
                "supportsEvaluateForHovers": true,
                "supportsStepBack": false,
                "supportsSetVariable": false,
                "supportsRestartFrame": false,
                "supportsCompletionsRequest": false,
                "supportsTerminateRequest": true,
                "supportsDelayedStackTraceLoading": true,
                "supportsRestartRequest": false,
            })),
            None,
        );
        write_dap(out, &resp).await?;

        let evt = make_event(&self.seq, "initialized", None);
        write_dap(out, &evt).await?;
        Ok(())
    }

    pub(super) async fn handle_configuration_done<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Clone the Arc before dropping the lock so we don't hold the mutex
        // guard across the async invoke() call.
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            // Current BC online rejects DebugAdapterConfigurationDone until
            // OnAttachedToConnection fires — which for break-on-next
            // web-client launches happens only after the browser attaches,
            // i.e. after the client already sent this very request. Attempt
            // immediately when already attached (fast path); otherwise the
            // background event-forwarding task retries on every poll once
            // `is_attached()` flips true (same retry the MCP path performs
            // in `NativeDebugSession::drain_events`).
            try_configuration_done(&s, &self.debug_config, &self.configured).await;
        }
        self.reply_ok(out, request_seq, command).await?;
        Ok(())
    }

    pub(super) async fn handle_launch_attach<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let config = BcDebugConfig::from_dap_args(arguments);
        if let Err(error) = config.validate_native() {
            self.reply_failure(out, request_seq, command, error).await?;
            return Ok(());
        }
        if let Err(error) = (self.authorize_target)(&config) {
            self.reply_failure(out, request_seq, command, error).await?;
            return Ok(());
        }
        self.variable_handles.lock().await.reset();
        *self.configured.lock().await = false;
        // Store config for use in the configurationDone handler.
        *self.debug_config.lock().await = Some(config.clone());

        let mut onprem_web_base = None;
        if command == "launch" {
            self.emit_output(out, "console", "Compiling AL project...\r\n")
                .await?;
            // Native-first: build the deploy `.app` with the shared build
            // service. The native emitter needs neither `alc` nor a configured
            // Microsoft toolchain, so launch must never skip compilation merely
            // because those optional components are absent.
            let compile_outcome = (self.compile)(PathBuf::from(&self.project_root)).await;
            match compile_outcome {
                Ok(output) => {
                    if !output.is_empty() {
                        self.emit_output(out, "console", format!("{output}\r\n"))
                            .await?;
                    }
                    self.emit_output(out, "console", "Compilation succeeded.\r\n")
                        .await?;
                }
                Err(e) => {
                    self.emit_output(out, "stderr", format!("Compilation failed: {e}\r\n"))
                        .await?;
                    self.reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Compilation failed: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        self.emit_output(
            out,
            "console",
            format!("Authenticating to tenant {}...\r\n", config.tenant),
        )
        .await?;

        let token = match (self.acquire_token)(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                self.reply_failure(
                    out,
                    request_seq,
                    command,
                    format!("Authentication failed: {e}"),
                )
                .await?;
                return Ok(());
            }
        };

        if command == "launch" {
            self.emit_output(out, "console", "Publishing package...\r\n")
                .await?;

            if config.accept_invalid_certs {
                al_bc::http_auth::warn_insecure_tls("DAP launch");
                self.emit_output(
                    out,
                    "important",
                    format!(
                        "{}\r\n",
                        al_bc::http_auth::insecure_tls_message("DAP launch")
                    ),
                )
                .await?;
            }

            let http = reqwest::Client::builder()
                .danger_accept_invalid_certs(config.accept_invalid_certs)
                .build()
                .map_err(|e| DapError::PublishFailed(e.to_string()))?;

            if config.launch_browser && config.environment_type.eq_ignore_ascii_case("OnPrem") {
                match get_web_endpoint(&http, &config, &token).await {
                    Ok(endpoint) => onprem_web_base = Some(endpoint),
                    Err(error) => {
                        self.reply_failure(
                            out,
                            request_seq,
                            command,
                            format!("Cannot resolve the on-premises Web client URL: {error}"),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }

            let app_path = match (self.find_app)(Path::new(&self.project_root)) {
                Ok(app_path) => app_path,
                Err(error) => {
                    self.reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Cannot locate compiled package: {error}"),
                    )
                    .await?;
                    return Ok(());
                }
            };
            if let Some(app_path) = app_path {
                match publish_app(&http, &config, &token, &app_path).await {
                    Ok(()) => {
                        self.emit_output(out, "console", "Package published successfully.\r\n")
                            .await?;
                    }
                    Err(e) => {
                        self.reply_failure(
                            out,
                            request_seq,
                            command,
                            format!("Publish failed: {e}"),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            } else {
                // a missing .app means compile failed (or
                // hasn't run). Continuing into publish/attach would
                // either silently use a stale .app from a previous
                // build (worse — debugging the wrong source) or
                // produce a confusing "Connect failed" trail. Fail
                // the launch with a clear error so the user sees
                // the compile failure as the root cause.
                write_dap(
                            out,
                            &make_response(
                                &self.seq,
                                request_seq,
                                command,
                                false,
                                None,
                                Some(
                                    "No compiled .app found in project root — compile must succeed before launch (run `al-explorer compile`)."
                                        .to_string(),
                                ),
                            ),
                        )
                        .await?;
                return Ok(());
            }
        }

        self.emit_output(out, "console", "Connecting to debug hub...\r\n")
            .await?;

        match BcDebugSession::connect(&config, &token).await {
            Ok(debug_session) => {
                if let Err(e) = debug_session.attach(&config).await {
                    self.reply_failure(out, request_seq, command, format!("Attach failed: {e}"))
                        .await?;
                    return Ok(());
                }

                // Capture connection ID before moving session into Arc+mutex
                let conn_id = debug_session.connection_id.clone();
                let web_url = if command == "launch" && config.launch_browser {
                    match build_debug_browser_url(&config, &conn_id, onprem_web_base.as_deref()) {
                        Ok(url) => Some(url),
                        Err(error) => {
                            self.reply_failure(
                                out,
                                request_seq,
                                command,
                                format!("Cannot build the Web client URL: {error}"),
                            )
                            .await?;
                            return Ok(());
                        }
                    }
                } else {
                    None
                };
                let session_arc = Arc::new(debug_session);
                *self.session.lock().await = Some(session_arc.clone());

                self.spawn_event_forwarder();

                self.emit_output(out, "console", "Debug session started.\r\n")
                    .await?;

                write_dap(
                    out,
                    &make_response(&self.seq, request_seq, command, true, None, None),
                )
                .await?;

                // Re-apply any breakpoints the client set during DAP
                // configuration (before this session existed) — they were
                // answered `verified: false` at the time and queued rather
                // than lost. Now that a session exists, resolve/add them on
                // BC and tell the client their real verification state via
                // `breakpoint` events.
                self.apply_pending_breakpoints(&session_arc, out).await?;

                // Open browser with debug context params (must match SignalR ConnectionId)
                if let Some(web_url) = web_url {
                    write_dap(
                        out,
                        &make_event(
                            &self.seq,
                            "al/openUri",
                            Some(serde_json::json!({
                                "uri": web_url
                            })),
                        ),
                    )
                    .await?;

                    if !open_browser(&web_url) {
                        tracing::warn!(url = %web_url,
                                    "DAP launch: could not auto-open browser for AAD \
                                     device-code; user must navigate manually");
                    }
                }
            }
            Err(e) => {
                self.reply_failure(
                    out,
                    request_seq,
                    command,
                    format!("Debug hub connection failed: {e}"),
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn handle_disconnect<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Clone Arc, drop guard, then stop (don't hold mutex across await).
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            let _ = s.stop_debugging().await;
        }
        *self.session.lock().await = None;
        self.variable_handles.lock().await.reset();
        self.reply_ok(out, request_seq, command).await?;

        write_dap(out, &make_event(&self.seq, "terminated", None)).await?;
        Ok(())
    }
}

/// Attempt `DebugAdapterConfigurationDone` if it hasn't already succeeded
/// and BC reports the client has attached. Shared by `handle_configuration_done`
/// (the fast path, attempted the moment the client's request arrives) and
/// the background event-forwarding task (the retry path, polled every cycle
/// until it succeeds) — mirrors the retry the MCP path performs in
/// `NativeDebugSession::drain_events` (native_debug.rs:222): current BC
/// online rejects the RPC until `OnAttachedToConnection` fires, which for
/// break-on-next web-client launches happens only after the browser
/// attaches, i.e. often after the DAP client already sent `configurationDone`
/// once and got rejected.
///
/// Returns `true` if an attempt (successful or not) was made, `false` if
/// skipped (already configured, no config stored yet, or not yet attached) —
/// mainly useful for tests.
pub(super) async fn try_configuration_done(
    session: &BcDebugSession,
    debug_config: &Mutex<Option<BcDebugConfig>>,
    configured: &Mutex<bool>,
) -> bool {
    if *configured.lock().await {
        return false;
    }
    if !session.is_attached().await {
        return false;
    }
    let Some(cfg) = debug_config.lock().await.clone() else {
        return false;
    };
    match session.configuration_done(&cfg).await {
        Ok(()) => {
            *configured.lock().await = true;
            info!("DAP configurationDone accepted after client attach");
        }
        Err(error) => {
            warn!(%error, "configurationDone rejected; will retry");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    use tokio::sync::watch;

    use crate::dap::framing::read_dap_body;

    use super::super::test_support::*;
    use super::super::VariableHandleStore;

    #[tokio::test]
    async fn initialize_reports_capabilities_and_initialized_event() {
        let (term, frames) = run_request("initialize", serde_json::json!({})).await;
        assert!(!term);
        assert_eq!(frames.len(), 2, "response + initialized event: {frames:?}");
        assert_eq!(frames[0]["type"], "response");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(
            frames[0]["body"]["supportsConfigurationDoneRequest"], true,
            "capabilities must be advertised"
        );
        for capability in [
            "supportsFunctionBreakpoints",
            "supportsStepBack",
            "supportsSetVariable",
            "supportsRestartFrame",
            "supportsCompletionsRequest",
            "supportsRestartRequest",
        ] {
            assert_eq!(
                frames[0]["body"][capability], false,
                "{capability} must stay false until the native adapter has a real implementation"
            );
        }
        assert_eq!(frames[1]["event"], "initialized");
        // Monotonic seq across both messages.
        assert!(frames[0]["seq"].as_u64() < frames[1]["seq"].as_u64());
    }

    #[tokio::test]
    async fn configuration_done_skips_rpc_when_client_not_yet_attached() {
        // Current BC online rejects DebugAdapterConfigurationDone before
        // OnAttachedToConnection fires; the handler must not even attempt
        // the RPC while unattached (it defers to the event-forwarder retry
        // instead of burning a doomed call).
        let state = test_state();
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        *state.session.lock().await = Some(Arc::new(session));
        *state.debug_config.lock().await = Some(BcDebugConfig::default());

        let (_, frames) = run_request_on(&state, "configurationDone", serde_json::json!({})).await;
        assert_eq!(
            frames[0]["success"], true,
            "response always acks: {frames:?}"
        );
        assert!(
            fake.sent_frames()
                .iter()
                .all(|f| f["target"] != "DebugAdapterConfigurationDone"),
            "must not attempt the RPC while BC reports not attached"
        );
        assert!(
            !*state.configured.lock().await,
            "must not mark configured when no attempt was made"
        );
    }

    #[tokio::test]
    async fn configuration_done_succeeds_immediately_when_already_attached() {
        let state = test_state();
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        // Drive OnAttachedToConnection through the session before wrapping
        // it in the state, so is_attached() reads true.
        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;
        assert!(session.is_attached().await);

        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));
        *state.session.lock().await = Some(Arc::new(session));
        *state.debug_config.lock().await = Some(BcDebugConfig::default());

        let (_, frames) = run_request_on(&state, "configurationDone", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(
            fake.sent_frames()
                .iter()
                .filter(|f| f["target"] == "DebugAdapterConfigurationDone")
                .count(),
            1
        );
        assert!(*state.configured.lock().await, "must mark configured");
    }

    #[tokio::test]
    async fn try_configuration_done_retries_after_attach_and_only_configures_once() {
        // Models the background event-forwarder's retry loop directly:
        // first call while unattached does nothing; once BC reports
        // attached, the same helper succeeds; a third call is a no-op
        // because it's already configured (BC must only see one call).
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        let debug_config = Mutex::new(Some(BcDebugConfig::default()));
        let configured = Mutex::new(false);

        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(!attempted, "must skip while not attached");
        assert!(!*configured.lock().await);

        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;
        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));

        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted, "must attempt once attached");
        assert!(*configured.lock().await);

        // A further call (e.g. the forwarder's next 50ms poll) must not
        // re-invoke BC now that configuration succeeded.
        let attempted_again = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(!attempted_again);
        assert_eq!(
            fake.sent_frames()
                .iter()
                .filter(|f| f["target"] == "DebugAdapterConfigurationDone")
                .count(),
            1,
            "BC must see exactly one DebugAdapterConfigurationDone call"
        );
    }

    #[tokio::test]
    async fn try_configuration_done_keeps_retrying_after_a_rejection() {
        // BC rejects the first attempt (still not really ready) — the next
        // call must retry rather than giving up permanently.
        let (session, fake) = crate::dap::bc_debug::fake::FakeBc::start("conn-1");
        let debug_config = Mutex::new(Some(BcDebugConfig::default()));
        let configured = Mutex::new(false);

        fake.push_callback("OnAttachedToConnection", serde_json::Value::Null);
        session.try_drain_push_events().await;

        // `configuration_done` itself retries once with no args if the
        // debug-options form is rejected (older-BC compat); queue a
        // rejection for both attempts so the overall call fails.
        fake.reply_err("DebugAdapterConfigurationDone", "not ready");
        fake.reply_err("DebugAdapterConfigurationDone", "still not ready");
        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted);
        assert!(
            !*configured.lock().await,
            "a rejected attempt must not mark configured"
        );

        fake.reply_ok("DebugAdapterConfigurationDone", serde_json::json!(null));
        let attempted = try_configuration_done(&session, &debug_config, &configured).await;
        assert!(attempted);
        assert!(*configured.lock().await, "retry must succeed");
    }

    #[tokio::test]
    async fn disconnect_terminates_with_response_then_terminated_event() {
        let (term, frames) = run_request("disconnect", serde_json::json!({})).await;
        assert!(term, "disconnect must signal loop exit");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["type"], "response");
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[1]["event"], "terminated");
    }

    #[tokio::test]
    async fn attach_without_auth_fails_with_message() {
        let (term, frames) = run_request(
            "attach",
            serde_json::json!({"tenant": "test-tenant", "environmentType": "Sandbox"}),
        )
        .await;
        assert!(!term);
        let last = frames.last().expect("at least one frame");
        assert_eq!(last["success"], false, "frames: {frames:?}");
        assert!(last["message"]
            .as_str()
            .unwrap_or("")
            .contains("Authentication failed"));
    }

    /// A launch whose target the authoriser refuses is answered with the
    /// refusal before anything is compiled, a token is read, or a request
    /// leaves. The configuration is what a cloned repository's
    /// `.zed/debug.json` can hold.
    #[tokio::test]
    async fn dap_refuses_a_launch_the_authoriser_refuses_before_any_token_or_request() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let collector = wiremock::MockServer::start().await;
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        std::mem::forget(_dap_event_rx);
        let compiled = Arc::new(AtomicBool::new(false));
        let compiled_by_hook = Arc::clone(&compiled);
        let token_read = Arc::new(AtomicBool::new(false));
        let token_read_by_hook = Arc::clone(&token_read);
        let authorised = Arc::new(std::sync::Mutex::new(None::<(Option<String>, u16)>));
        let authorised_by_hook = Arc::clone(&authorised);
        let state = NativeDapState {
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
            project_root: "/test/project".to_string(),
            authorize_target: Arc::new(move |config: &BcDebugConfig| {
                *authorised_by_hook.lock().unwrap() = Some((config.server.clone(), config.port));
                Err("Refusing to send a cached Business Central token: not trusted".to_string())
            }),
            acquire_token: move |_: String| {
                token_read_by_hook.store(true, Ordering::SeqCst);
                std::future::ready(Ok("test-token".to_string()))
            },
            resolve_object: |_: &str| -> Option<ResolvedObject> { None },
            resolve_path: |_: i32, _: i32| -> Option<PathBuf> { None },
            compile: move |_: PathBuf| {
                compiled_by_hook.store(true, Ordering::SeqCst);
                std::future::ready(Ok(String::new()))
            },
            find_app: |_: &Path| -> std::result::Result<Option<PathBuf>, String> { Ok(None) },
        };

        let port = url::Url::parse(&collector.uri()).unwrap().port().unwrap();
        for command in ["launch", "attach"] {
            let (mut client, server) = tokio::io::duplex(64 * 1024);
            state
                .handle_request(
                    &mut client,
                    command,
                    7,
                    &serde_json::json!({
                        "environmentType": "OnPrem",
                        "server": "http://127.0.0.1",
                        "port": port,
                        "serverInstance": "BC",
                        "authentication": "AAD",
                        "tenant": "organizations",
                    }),
                )
                .await
                .expect("launch handler");
            use tokio::io::AsyncWriteExt;
            client.shutdown().await.expect("shutdown");
            drop(client);
            let mut reader = tokio::io::BufReader::new(server);
            let mut frames: Vec<serde_json::Value> = Vec::new();
            while let Ok(body) = read_dap_body(&mut reader).await {
                frames.push(serde_json::from_slice(&body).expect("DAP JSON"));
            }
            let response = frames.last().expect("a response");
            assert_eq!(response["success"], false, "{command}: {frames:?}");
            assert!(
                response["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("not trusted"),
                "{command}: {frames:?}"
            );
        }
        assert_eq!(
            *authorised.lock().unwrap(),
            Some((Some("http://127.0.0.1".to_string()), port)),
            "the authoriser must judge the server and port the session would use"
        );
        assert!(!compiled.load(Ordering::SeqCst), "compiled before refusing");
        assert!(
            !token_read.load(Ordering::SeqCst),
            "read a token before refusing"
        );
        assert!(
            collector
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "a request reached the server the scenario named"
        );
    }

    #[tokio::test]
    async fn launch_compiles_then_rejects_missing_manifest_selected_artifact() {
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        std::mem::forget(_dap_event_rx);
        let compiled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let compiled_by_hook = Arc::clone(&compiled);
        let state = NativeDapState {
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
            project_root: "/test/project".to_string(),
            authorize_target: allow_every_target(),
            acquire_token: |_: String| std::future::ready(Ok("test-token".to_string())),
            resolve_object: |_: &str| -> Option<ResolvedObject> { None },
            resolve_path: |_: i32, _: i32| -> Option<PathBuf> { None },
            compile: move |_: PathBuf| {
                compiled_by_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(Ok("shared build completed".to_string()))
            },
            // This is the shared manifest-name selector supplied by al-lsp;
            // no match must fail rather than publishing a stale neighbouring app.
            find_app: |_: &Path| -> std::result::Result<Option<PathBuf>, String> { Ok(None) },
        };

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let terminate = state
            .handle_request(
                &mut client,
                "launch",
                7,
                &serde_json::json!({"tenant": "test-tenant", "environmentType": "Sandbox"}),
            )
            .await
            .expect("launch handler");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.expect("shutdown");
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut frames: Vec<serde_json::Value> = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            frames.push(serde_json::from_slice(&body).expect("DAP JSON"));
        }
        assert!(!terminate);
        assert!(compiled.load(std::sync::atomic::Ordering::SeqCst));
        let response = frames.last().expect("launch response");
        assert_eq!(response["success"], false, "frames: {frames:?}");
        assert!(response["message"]
            .as_str()
            .unwrap_or("")
            .contains("No compiled .app found"));
        assert!(
            state.session.lock().await.is_none(),
            "must not connect after missing artifact"
        );
    }
}
