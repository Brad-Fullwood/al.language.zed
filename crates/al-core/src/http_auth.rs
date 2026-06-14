//! Shared HTTP helpers for BC server API clients.

/// Canonical, user-facing message warning that TLS verification is disabled.
/// `context` names the surface (e.g. "BcClient", "DAP launch") so identical
/// wording appears across every code path that honours `acceptInvalidCerts`.
pub(crate) fn insecure_tls_message(context: &str) -> String {
    format!(
        "TLS certificate verification is DISABLED ({context}: acceptInvalidCerts=true). \
         Business Central traffic — including credentials and bearer tokens — is \
         vulnerable to interception/MITM. Use only against a trusted local-dev sandbox."
    )
}

/// Surface the insecure-TLS warning to every operator-visible channel: the
/// structured log AND, unconditionally, stderr — so it is seen even when
/// `RUST_LOG` filters out warn-level tracing (stderr reaches the editor's LSP/DAP
/// log and the CLI terminal). DAP callers should ALSO emit it to the debug
/// console (an `output` event), the surface the editing user actually watches.
pub(crate) fn warn_insecure_tls(context: &str) {
    let msg = insecure_tls_message(context);
    tracing::warn!("{msg}");
    eprintln!("al-lsp: {msg}");
}

/// Build a `reqwest::Client` with the given TLS and timeout settings.
///
/// Shared by `profiling` and `snapshot` so their `make_client` wrappers are
/// reduced to a single delegating call rather than duplicated builder chains.
///
/// When `accept_invalid_certs` is `true`, this disables TLS certificate
/// verification. The helper emits a `tracing::warn!` parity with the
/// other BC server client builders (`bc_server.rs`, `bc_debug.rs`,
/// `bc_client.rs`, `native_dap.rs`) so operators see the same warning
/// regardless of which code path constructs the client. F-OPEN-020.
pub(crate) fn build_http_client(
    accept_invalid_certs: bool,
    timeout_secs: u64,
) -> Result<reqwest::Client, reqwest::Error> {
    if accept_invalid_certs {
        warn_insecure_tls("BC HTTP client");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_tls_message_names_context_and_warns_clearly() {
        let m = insecure_tls_message("BcClient");
        assert!(m.contains("BcClient"), "must name the surface: {m}");
        assert!(m.contains("DISABLED"), "must be unambiguous: {m}");
        assert!(
            m.to_lowercase().contains("acceptinvalidcerts"),
            "must name the setting that caused it: {m}"
        );
        // Different contexts produce distinct, attributable messages.
        assert_ne!(m, insecure_tls_message("DAP launch"));
    }

    /// Build a throwaway request and return its `Authorization` header value,
    /// if any. This exercises the *real* effect of `apply_basic_auth` — that a
    /// correctly-encoded Basic credential is (or is not) attached to the
    /// outgoing request.
    fn authorization_header(req: reqwest::RequestBuilder) -> Option<String> {
        let request = req.build().expect("request should build");
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .map(|v| v.to_str().expect("header is valid ASCII").to_string())
    }

    fn get_request() -> reqwest::RequestBuilder {
        reqwest::Client::new().get("http://localhost:7049/BC")
    }

    #[test]
    fn build_http_client_with_valid_certs_succeeds() {
        // Happy path: standard TLS verification, normal timeout.
        let client = build_http_client(false, 30);
        assert!(client.is_ok(), "client builder should succeed: {client:?}");
    }

    #[test]
    fn build_http_client_accepting_invalid_certs_succeeds() {
        // The insecure path must still produce a usable client (it only
        // disables verification + logs a warning).
        let client = build_http_client(true, 30);
        assert!(client.is_ok(), "client builder should succeed: {client:?}");
    }

    #[test]
    fn build_http_client_with_zero_timeout_succeeds() {
        // Boundary value: a zero-second timeout is a legal Duration and the
        // builder must not reject it.
        let client = build_http_client(false, 0);
        assert!(
            client.is_ok(),
            "zero timeout should still build: {client:?}"
        );
    }

    #[test]
    fn build_http_client_with_large_timeout_succeeds() {
        // Boundary value: a very large timeout must not overflow the Duration.
        let client = build_http_client(false, u64::MAX);
        assert!(client.is_ok(), "max timeout should still build: {client:?}");
    }

    #[test]
    fn apply_basic_auth_with_both_credentials_sets_authorization() {
        // RED-GREEN anchor: with both username and password present, the
        // request must carry the correctly base64-encoded Basic credential.
        // `admin:password` => base64 "YWRtaW46cGFzc3dvcmQ=".
        let req = apply_basic_auth(
            get_request(),
            &Some("admin".to_string()),
            &Some("password".to_string()),
        );
        let header = authorization_header(req).expect("Authorization header must be present");
        assert_eq!(header, "Basic YWRtaW46cGFzc3dvcmQ=");
    }

    #[test]
    fn apply_basic_auth_with_empty_credentials_still_sets_authorization() {
        // Edge: empty (but present) username/password are still credentials —
        // `Some("")` is not the same as `None`. base64("" + ":" + "") = "Og==".
        let req = apply_basic_auth(get_request(), &Some(String::new()), &Some(String::new()));
        let header = authorization_header(req).expect("Authorization header must be present");
        assert_eq!(header, "Basic Og==");
    }

    #[test]
    fn apply_basic_auth_without_credentials_leaves_request_unauthenticated() {
        // No credentials -> no Authorization header (Windows auth path).
        let req = apply_basic_auth(get_request(), &None, &None);
        assert!(
            authorization_header(req).is_none(),
            "no Authorization header should be set when both credentials are absent"
        );
    }

    #[test]
    fn apply_basic_auth_with_only_username_leaves_request_unauthenticated() {
        // Partial credentials -> the tuple match fails -> request untouched.
        let req = apply_basic_auth(get_request(), &Some("admin".to_string()), &None);
        assert!(
            authorization_header(req).is_none(),
            "username without password must not produce a Basic header"
        );
    }

    #[test]
    fn apply_basic_auth_with_only_password_leaves_request_unauthenticated() {
        // Partial credentials -> the tuple match fails -> request untouched.
        let req = apply_basic_auth(get_request(), &None, &Some("password".to_string()));
        assert!(
            authorization_header(req).is_none(),
            "password without username must not produce a Basic header"
        );
    }

    #[test]
    fn apply_basic_auth_password_containing_colon_encodes_verbatim() {
        // RFC 7617: only the FIRST colon separates user from password, so a
        // password containing colons must be base64-encoded verbatim (the
        // colons inside it are not separators). This guards the real BC case
        // where service-account passwords can contain ':'.
        // "user:p:a:ss" => base64 "dXNlcjpwOmE6c3M=".
        let req = apply_basic_auth(
            get_request(),
            &Some("user".to_string()),
            &Some("p:a:ss".to_string()),
        );
        let header = authorization_header(req).expect("Authorization header must be present");
        assert_eq!(header, "Basic dXNlcjpwOmE6c3M=");
    }

    #[test]
    fn apply_basic_auth_non_ascii_credentials_utf8_base64_encoded() {
        // Edge: non-ASCII (multi-byte UTF-8) credentials must be encoded by
        // their UTF-8 bytes, not panic or mangle. "Bjørn:naïve" UTF-8 bytes
        // base64-encode to "QmrDuHJuOm5hw692ZQ==".
        let req = apply_basic_auth(
            get_request(),
            &Some("Bjørn".to_string()),
            &Some("naïve".to_string()),
        );
        let header = authorization_header(req).expect("Authorization header must be present");
        assert_eq!(header, "Basic QmrDuHJuOm5hw692ZQ==");
    }

    #[test]
    fn apply_basic_auth_empty_username_nonempty_password_encodes() {
        // Edge: present-but-empty username with a real password is still a
        // credential (Some/Some matches). ":secret" => base64 "OnNlY3JldA==".
        let req = apply_basic_auth(
            get_request(),
            &Some(String::new()),
            &Some("secret".to_string()),
        );
        let header = authorization_header(req).expect("Authorization header must be present");
        assert_eq!(header, "Basic OnNlY3JldA==");
    }
}
