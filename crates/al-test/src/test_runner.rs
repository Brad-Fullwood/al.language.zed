//! Running tests on live Business Central: not implemented.
//!
//! BC runs tests through the SignalR hub `{base}/dev/TestRunnerHub`
//! (`Initialize`, `RunTests`, then `TestStarted`/`TestCompleted` callbacks).
//! This crate has no client for that hub yet. An earlier version of this
//! module posted to a `tests/{codeunit}/run` route under `{base}/dev`, which BC answers
//! with 404 like any unknown path. See
//! `testing-01-live-bc-dev-tests-endpoint-invented.md` in the audit findings.
//!
//! Standard route checked: the debugger's SignalR transport in `al-dap` is the
//! piece to reuse when the hub client is written, and another change owns it.

use crate::error::TestRunnerError;
use crate::result::TestCodeunitResult;

/// The text reported for every test that routes to live BC.
pub const LIVE_BC_SKIP_MESSAGE: &str = "Skipped: runs on live BC, which needs the TestRunnerHub client (not implemented yet, see Docs/features/native-test-runtime.md)";

/// Placeholder for the future TestRunnerHub client.
pub struct TestRunnerClient;

impl TestRunnerClient {
    /// Always returns [`TestRunnerError::LiveBcNotImplemented`].
    pub async fn run_codeunit(
        &self,
        _codeunit_id: i32,
        _codeunit_name: &str,
        _method: Option<&str>,
    ) -> Result<TestCodeunitResult, TestRunnerError> {
        Err(TestRunnerError::LiveBcNotImplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_codeunit_reports_not_implemented() {
        let error = TestRunnerClient
            .run_codeunit(50100, "Tests", None)
            .await
            .expect_err("no hub client exists yet");
        assert!(matches!(error, TestRunnerError::LiveBcNotImplemented));
        assert!(error.to_string().contains("TestRunnerHub"));
    }
}
