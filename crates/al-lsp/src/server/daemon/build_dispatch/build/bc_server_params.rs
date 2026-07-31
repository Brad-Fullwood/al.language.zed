//! Shared BC-server connection parameter parsing and the SSRF `serverUrl`
//! guard, used by the snapshot and profiling dispatchers.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};

/// Common BC server connection parameters extracted from JSON-RPC params.
pub(super) struct BcServerParams {
    pub(super) server_url: String,
    pub(super) company: String,
    pub(super) output_dir: std::path::PathBuf,
    pub(super) username: Option<String>,
    pub(super) password: Option<String>,
    pub(super) accept_invalid_certs: bool,
}
pub(super) fn parse_bc_server_params(
    params: &serde_json::Value,
    output_subdir: &str,
) -> Result<BcServerParams, String> {
    fn optional_string(params: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
        match params.get(key) {
            None => Ok(None),
            Some(value) => value
                .as_str()
                .map(|value| Some(value.to_string()))
                .ok_or_else(|| format!("'{key}' must be a string when supplied")),
        }
    }

    let server_url = optional_string(params, "serverUrl")?
        .unwrap_or_else(|| "http://localhost:7049/BC".to_string());
    let company = optional_string(params, "company")?.unwrap_or_default();
    let output_dir = match optional_string(params, "outputDir")? {
        Some(output_dir) => {
            let output_dir = std::path::PathBuf::from(output_dir);
            if !output_dir.is_absolute() {
                return Err("'outputDir' must be an absolute path".to_string());
            }
            output_dir
        }
        None => dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("al-lsp")
            .join(output_subdir),
    };
    let username = optional_string(params, "username")?;
    let password = optional_string(params, "password")?;
    let accept_invalid_certs = match params.get("acceptInvalidCerts") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "'acceptInvalidCerts' must be a boolean when supplied".to_string())?,
    };
    Ok(BcServerParams {
        server_url,
        company,
        output_dir,
        username,
        password,
        accept_invalid_certs,
    })
}

/// SSRF guard: reject a `serverUrl` whose scheme is not http(s) before any
/// network use. The daemon socket is same-user only, but a `serverUrl` of
/// `file://`, `gopher://`, etc. would otherwise be handed straight to the BC
/// HTTP client. Reuses the launch-config allowlist so both surfaces agree.
/// Returns `Some(INVALID_PARAMS error)` when the URL must be rejected.
pub(super) fn reject_unsafe_server_url(id: u64, server_url: &str) -> Option<Response> {
    if al_bc::launch::is_safe_http_server(server_url) {
        return None;
    }
    Some(Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: format!("serverUrl '{server_url}' is not an http(s) URL; refusing to connect"),
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;

    #[test]
    fn reject_unsafe_server_url_blocks_non_http_schemes() {
        for bad in ["file:///etc/passwd", "gopher://internal", "ftp://host", ""] {
            let resp = reject_unsafe_server_url(1, bad)
                .unwrap_or_else(|| panic!("{bad:?} must be rejected"));
            assert_eq!(resp.error.unwrap().code, error_codes::INVALID_PARAMS);
            assert!(resp.result.is_none());
        }
    }

    #[test]
    fn reject_unsafe_server_url_allows_http_and_https() {
        for ok in [
            "http://localhost:7049/BC",
            "https://bc.example/inst",
            "localhost:7048",
        ] {
            assert!(
                reject_unsafe_server_url(1, ok).is_none(),
                "{ok:?} must be allowed"
            );
        }
    }

    #[test]
    fn bc_server_params_apply_documented_defaults_when_absent() {
        // Empty params: every field must fall back to its documented default
        // and the output dir must end in the supplied subdir.
        let bc = parse_bc_server_params(&serde_json::json!({}), "snapshots").unwrap();
        assert_eq!(bc.server_url, "http://localhost:7049/BC");
        assert_eq!(bc.company, "");
        assert!(bc.username.is_none());
        assert!(bc.password.is_none());
        assert!(!bc.accept_invalid_certs);
        assert!(
            bc.output_dir.ends_with("snapshots"),
            "default output dir must end in the subdir, got {:?}",
            bc.output_dir
        );
    }

    #[test]
    fn bc_server_params_honour_explicit_overrides() {
        // Positive: every explicit field is threaded through verbatim, and an
        // explicit outputDir wins over the subdir-based default.
        let bc = parse_bc_server_params(
            &serde_json::json!({
                "serverUrl": "https://bc.example/inst",
                "company": "CRONUS",
                "outputDir": "/data/out",
                "username": "admin",
                "password": "s3cret",
                "acceptInvalidCerts": true,
            }),
            "profiles",
        )
        .unwrap();
        assert_eq!(bc.server_url, "https://bc.example/inst");
        assert_eq!(bc.company, "CRONUS");
        assert_eq!(bc.output_dir, std::path::PathBuf::from("/data/out"));
        assert_eq!(bc.username.as_deref(), Some("admin"));
        assert_eq!(bc.password.as_deref(), Some("s3cret"));
        assert!(bc.accept_invalid_certs);
    }

    #[test]
    fn bc_server_params_reject_wrong_typed_or_relative_fields() {
        for params in [
            serde_json::json!({ "serverUrl": 7049 }),
            serde_json::json!({ "company": false }),
            serde_json::json!({ "username": 1 }),
            serde_json::json!({ "password": [] }),
            serde_json::json!({ "acceptInvalidCerts": "yes" }),
            serde_json::json!({ "outputDir": "relative/path" }),
        ] {
            assert!(
                parse_bc_server_params(&params, "snapshots").is_err(),
                "malformed BC params must be rejected: {params}"
            );
        }
    }
}
