//! Shared HTTP helpers for BC server API clients.

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
