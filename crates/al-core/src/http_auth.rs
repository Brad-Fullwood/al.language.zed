//! Shared HTTP helpers for BC server API clients.

/// Build a `reqwest::Client` with the given TLS and timeout settings.
///
/// Shared by `profiling` and `snapshot` so their `make_client` wrappers are
/// reduced to a single delegating call rather than duplicated builder chains.
pub(crate) fn build_http_client(
    accept_invalid_certs: bool,
    timeout_secs: u64,
) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(accept_invalid_certs)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
}

/// Apply Basic auth to a request builder if credentials are provided.
///
/// Used by both `profiling` and `snapshot` modules.
pub(crate) fn apply_basic_auth(
    req: reqwest::RequestBuilder,
    username: &Option<String>,
    password: &Option<String>,
) -> reqwest::RequestBuilder {
    if let (Some(user), Some(pass)) = (username, password) {
        req.basic_auth(user, Some(pass))
    } else {
        req
    }
}
