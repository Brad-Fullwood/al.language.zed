//! Stepping, pausing, continuing, and the single AL thread DAP sees.

use std::path::{Path, PathBuf};

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
    /// Step over / into / out — BC BreakpointExitReason 1 / 2 / 3 via
    /// SetBreakpointResponse. Merged: the three original arms differed only
    /// in which session method they invoked.
    pub(super) async fn handle_step<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            let step = match command {
                "stepIn" => s.step_in().await,
                "stepOut" => s.step_out().await,
                _ => s.step_over().await,
            };
            if let Err(e) = step {
                self.reply_failure(out, request_seq, command, e.to_string())
                    .await?;
                return Ok(());
            }
            self.variable_handles.lock().await.reset();
        }
        self.reply_ok(out, request_seq, command).await?;
        Ok(())
    }

    pub(super) async fn handle_pause<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        // BC's SignalR debug hub does not expose a "pause while running" method.
        // The BC debugger only pauses at breakpoints or on error; there is no
        // equivalent of a SIGSTOP that the client can trigger mid-execution.
        // Respond with failure so Zed shows the user a clear error instead of
        // silently doing nothing.
        self.reply_failure(
            out,
            request_seq,
            command,
            "pause is not supported by the BC debug hub; set a breakpoint instead".to_string(),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn handle_continue<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let session_arc = self.session.lock().await.clone();
        if let Some(s) = session_arc {
            // BC expects BreakpointExitReason integer 0 (continue)
            if let Err(e) = s.continue_execution(serde_json::json!(0)).await {
                self.reply_failure(out, request_seq, command, e.to_string())
                    .await?;
                return Ok(());
            }
            self.variable_handles.lock().await.reset();
        }
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({"allThreadsContinued": true}),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn handle_threads<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({"threads": [{"id": 1, "name": "AL Thread"}]}),
        )
        .await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::super::test_support::*;

    #[tokio::test]
    async fn threads_returns_single_al_thread() {
        let (_, frames) = run_request("threads", serde_json::json!({})).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["body"]["threads"][0]["id"], 1);
    }

    #[tokio::test]
    async fn pause_fails_with_actionable_message() {
        let (_, frames) = run_request("pause", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], false);
        assert!(
            frames[0]["message"]
                .as_str()
                .unwrap_or("")
                .contains("breakpoint"),
            "must tell the user the BC alternative: {frames:?}"
        );
    }

    #[tokio::test]
    async fn steps_without_session_still_acknowledge() {
        for cmd in ["next", "stepIn", "stepOut"] {
            let (_, frames) = run_request(cmd, serde_json::json!({})).await;
            assert_eq!(frames[0]["success"], true, "{cmd} must ack: {frames:?}");
            assert_eq!(frames[0]["command"], cmd);
        }
    }

    #[tokio::test]
    async fn continue_without_session_acknowledges_all_threads() {
        let (_, frames) = run_request("continue", serde_json::json!({})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["allThreadsContinued"], true);
    }
}
