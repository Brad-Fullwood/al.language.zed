//! Test engine errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TestRunnerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Authentication failed (HTTP {status}): {message}")]
    AuthenticationFailed { status: u16, message: String },
    #[error("BC server error (HTTP {status}): {message}")]
    ServerError { status: u16, message: String },
    #[error("invalid BC test-runner response: {0}")]
    InvalidResponse(String),
    #[error("No server configuration found in launch.json")]
    NoConfig,
    #[error(
        "Missing credentials: set BC_ACCESS_TOKEN (or BC_TOKEN) for AAD, or BC_USERNAME and BC_PASSWORD for UserPassword"
    )]
    MissingCredentials,
    #[error("Invalid bearer-token environment: {0}")]
    CredentialConfiguration(#[from] al_bc::http_auth::AccessTokenEnvError),
    #[error("Timeout after {secs}s waiting for test runner")]
    Timeout { secs: u64 },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("test event channel closed: receiver dropped")]
    ChannelClosed,
    #[error("test worker failed: {0}")]
    WorkerFailed(String),
    #[error("test discovery failed: {0}")]
    TestDiscovery(#[from] al_analysis::queries::tests::TestQueryError),
}
