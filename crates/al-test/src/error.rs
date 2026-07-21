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
    #[error("No server configuration found in launch.json")]
    NoConfig,
    #[error("Missing credentials: set BC_USERNAME and BC_PASSWORD environment variables")]
    MissingCredentials,
    #[error("Timeout after {secs}s waiting for test runner")]
    Timeout { secs: u64 },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("test event channel closed: receiver dropped")]
    ChannelClosed,
}
