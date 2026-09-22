//! The reply shapes every handler writes, and the command table that routes a
//! request to one of them.
//!
//! Four reply shapes cover every exit: success with no body, success with a
//! body, failure with a message, and a console output event. Before they
//! existed each exit wrote the `write_dap` plus `make_response` pair inline.

use std::path::{Path, PathBuf};

use crate::dap::Result;

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
    /// Write a successful response with no body.
    pub(super) async fn reply_ok<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, None, None),
        )
        .await?;
        Ok(())
    }

    /// Write a successful response carrying `body`.
    pub(super) async fn reply_body<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        body: serde_json::Value,
    ) -> Result<()> {
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, Some(body), None),
        )
        .await?;
        Ok(())
    }

    /// Write a failure response carrying `message`.
    pub(super) async fn reply_failure<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        message: impl Into<String>,
    ) -> Result<()> {
        write_dap(
            out,
            &make_response(
                &self.seq,
                request_seq,
                command,
                false,
                None,
                Some(message.into()),
            ),
        )
        .await?;
        Ok(())
    }

    /// Write an `output` event. `category` is the DAP console category
    /// (`console`, `stdout`, `stderr`).
    pub(super) async fn emit_output<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        category: &str,
        text: impl Into<String>,
    ) -> Result<()> {
        write_dap(
            out,
            &make_event(
                &self.seq,
                "output",
                Some(serde_json::json!({ "category": category, "output": text.into() })),
            ),
        )
        .await?;
        Ok(())
    }

    /// Dispatch one DAP request to its handler. Returns `Ok(true)` when the
    /// server loop should exit (disconnect/terminate).
    pub(crate) async fn handle_request<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        command: &str,
        request_seq: i64,
        arguments: &serde_json::Value,
    ) -> Result<bool> {
        match command {
            "initialize" => self.handle_initialize(out, request_seq, command).await?,
            "configurationDone" => {
                self.handle_configuration_done(out, request_seq, command)
                    .await?
            }
            "launch" | "attach" => {
                self.handle_launch_attach(out, request_seq, command, arguments)
                    .await?
            }
            "setBreakpoints" => {
                self.handle_set_breakpoints(out, request_seq, command, arguments)
                    .await?
            }
            "next" | "stepIn" | "stepOut" => self.handle_step(out, request_seq, command).await?,
            "pause" => self.handle_pause(out, request_seq, command).await?,
            "setFunctionBreakpoints" | "setVariable" | "completions" | "restart" | "stepBack" => {
                self.handle_unsupported_capability(out, request_seq, command)
                    .await?
            }
            "continue" => self.handle_continue(out, request_seq, command).await?,
            "threads" => self.handle_threads(out, request_seq, command).await?,
            "stackTrace" => {
                self.handle_stack_trace(out, request_seq, command, arguments)
                    .await?
            }
            "scopes" => {
                self.handle_scopes(out, request_seq, command, arguments)
                    .await?
            }
            "variables" => {
                self.handle_variables(out, request_seq, command, arguments)
                    .await?
            }
            "evaluate" => {
                self.handle_evaluate(out, request_seq, command, arguments)
                    .await?
            }
            "disconnect" | "terminate" => {
                self.handle_disconnect(out, request_seq, command).await?;
                return Ok(true);
            }
            _ => self.handle_unknown(out, request_seq, command).await?,
        }
        Ok(false)
    }

    pub(super) async fn handle_unsupported_capability<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // Keep these command-specific failures aligned with the false
        // initialize capabilities. Protocol DTOs are not hub operations: the
        // live HubBasedDebuggerService has no method for any command below.
        let message = match command {
            "setFunctionBreakpoints" => {
                "function breakpoints are not supported by the BC debug hub; use source breakpoints instead"
            }
            "setVariable" => {
                "setVariable is not supported by the BC debug hub; variables can be inspected and evaluated but not mutated"
            }
            "completions" => {
                "completions are not supported by the native adapter; BC exposes no completion method and Microsoft's adapter delegates this request to its editor workspace"
            }
            "restart" => {
                "restart is not supported by the BC debug hub; disconnect and launch or attach a new debug session"
            }
            "stepBack" => {
                "stepBack is not supported by the BC debug hub; only continue, step-over, step-in, and step-out are available"
            }
            _ => unreachable!("only capability-gated commands are dispatched here"),
        };
        self.reply_failure(out, request_seq, command, message.to_string())
            .await?;
        Ok(())
    }

    pub(super) async fn handle_unknown<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        self.reply_failure(
            out,
            request_seq,
            command,
            format!("Unsupported command: {command}"),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod handler_tests {
    use crate::dap::bc_debug::BcDebugConfig;

    use super::super::test_support::*;
    use super::super::VariableHandle;

    #[tokio::test]
    async fn unsupported_capability_requests_fail_explicitly_without_mutating_state() {
        let state = test_state();
        *state.debug_config.lock().await = Some(BcDebugConfig::from_dap_args(
            &serde_json::json!({"tenant": "state-sentinel"}),
        ));
        state
            .breakpoints
            .lock()
            .await
            .insert("/project/Sentinel.al".to_string(), vec![41]);
        state
            .variable_handles
            .lock()
            .await
            .create(VariableHandle {
                frame_id: 3,
                parent_path: "Sentinel".to_string(),
                nodes: Some(Vec::new()),
            })
            .expect("sentinel variable handle");

        let cases = [
            (
                "setFunctionBreakpoints",
                "function breakpoints are not supported by the BC debug hub; use source breakpoints instead",
            ),
            (
                "setVariable",
                "setVariable is not supported by the BC debug hub; variables can be inspected and evaluated but not mutated",
            ),
            (
                "completions",
                "completions are not supported by the native adapter; BC exposes no completion method and Microsoft's adapter delegates this request to its editor workspace",
            ),
            (
                "restart",
                "restart is not supported by the BC debug hub; disconnect and launch or attach a new debug session",
            ),
            (
                "stepBack",
                "stepBack is not supported by the BC debug hub; only continue, step-over, step-in, and step-out are available",
            ),
        ];

        for (command, expected_message) in cases {
            let (terminate, frames) = run_request_on(&state, command, serde_json::json!({})).await;
            assert!(!terminate, "{command} must not terminate the adapter");
            assert_eq!(frames.len(), 1, "{command}: {frames:?}");
            assert_eq!(frames[0]["type"], "response");
            assert_eq!(frames[0]["request_seq"], 7);
            assert_eq!(frames[0]["requestSeq"], 7);
            assert_eq!(frames[0]["command"], command);
            assert_eq!(frames[0]["success"], false);
            assert_eq!(frames[0]["message"], expected_message);
            assert!(
                frames[0].get("body").is_none(),
                "failed {command} response must not fabricate a body: {frames:?}"
            );

            assert!(state.session.lock().await.is_none());
            assert_eq!(
                state
                    .debug_config
                    .lock()
                    .await
                    .as_ref()
                    .map(|config| config.tenant.as_str()),
                Some("state-sentinel")
            );
            assert_eq!(
                state
                    .breakpoints
                    .lock()
                    .await
                    .get("/project/Sentinel.al")
                    .cloned(),
                Some(vec![41])
            );
            assert_eq!(state.variable_handles.lock().await.handles.len(), 1);
        }
    }

    #[tokio::test]
    async fn unknown_command_is_rejected_not_ignored() {
        let (term, frames) = run_request("bogusCommand", serde_json::json!({})).await;
        assert!(!term);
        assert_eq!(frames[0]["success"], false);
        assert!(frames[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Unsupported command: bogusCommand"));
    }
}
