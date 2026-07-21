//! REST calls to the BC dev endpoint (publish + metadata).
//! Split out of the former monolithic `bc_debug.rs` (pure move, no behavior change).

use std::path::Path;

use reqwest::header::AUTHORIZATION;
use tracing::info;

use crate::dap::{DapError, Result};

use super::session_config::BcDebugConfig;
use super::wire::percent_encode_url;

pub async fn publish_app(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    app_path: &Path,
) -> Result<()> {
    let base = config.base_url();
    let url = format!(
        "{base}/apps?tenant={}&SchemaUpdateMode={}&DependencyPublishingOption={}",
        percent_encode_url(&config.tenant),
        config.schema_update_mode,
        config.dependency_publishing_option
    );

    info!("Publishing package to {url}");

    let app_bytes = tokio::fs::read(app_path)
        .await
        .map_err(|e| DapError::PublishFailed(format!("Failed to read .app file: {e}")))?;

    let file_name = app_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("app.app")
        .to_string();

    let part = reqwest::multipart::Part::bytes(app_bytes)
        .file_name(file_name)
        .mime_str("application/octet-stream")
        .map_err(|e| DapError::PublishFailed(format!("MIME error: {e}")))?;

    let form = reqwest::multipart::Form::new().part("file", part);

    let resp = http
        .post(&url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| DapError::PublishFailed(format!("Publish request failed: {e}")))?;

    if resp.status().is_success() {
        info!("Package published successfully");
        Ok(())
    } else {
        let status = resp.status();
        let body = al_bc::bc_client::sanitize_error_body(
            &resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<body read failed: {e}>")),
        );
        Err(DapError::PublishFailed(format!(
            "Publish failed (HTTP {status}): {body}"
        )))
    }
}

pub async fn get_metadata(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
) -> Result<serde_json::Value> {
    let base = config.base_url();
    let url = format!(
        "{base}/metadata?tenant={}",
        percent_encode_url(&config.tenant)
    );

    let resp = http
        .get(&url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|e| DapError::ConnectionFailed(format!("Metadata request failed: {e}")))?;

    if resp.status().is_success() {
        resp.json()
            .await
            .map_err(|e| DapError::ConnectionFailed(format!("Bad metadata response: {e}")))
    } else {
        let status = resp.status();
        let body = al_bc::bc_client::sanitize_error_body(
            &resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<body read failed: {e}>")),
        );
        Err(DapError::ConnectionFailed(format!(
            "Metadata failed (HTTP {status}): {body}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- publish_app / get_metadata over wiremock (HTTP seam) ----------------
    //
    // Both functions take an injected `&reqwest::Client` and build their URL
    // from `BcDebugConfig::base_url()`. By pointing an OnPrem config at a
    // wiremock `MockServer` (server=http://127.0.0.1, port=<mock port>,
    // server_instance=BC) we drive the REAL request path and assert on the
    // request shape BC receives + the response parsing branches the code takes.

    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build an OnPrem config whose `base_url()` resolves to the wiremock
    /// server's root + `/BC/dev`. `mock.uri()` is e.g. `http://127.0.0.1:PORT`,
    /// which we split into `server` (host) and `port`.
    fn config_for_mock(mock: &MockServer) -> BcDebugConfig {
        let uri = mock.uri(); // http://127.0.0.1:PORT
        let url = url::Url::parse(&uri).expect("valid mock uri");
        let port = url.port().expect("mock uri has port");
        let server = format!(
            "{}://{}",
            url.scheme(),
            url.host_str().expect("mock uri has host")
        );
        BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some(server),
            server_instance: Some("BC".to_string()),
            port,
            tenant: "default".to_string(),
            ..BcDebugConfig::default()
        }
    }

    #[tokio::test]
    async fn publish_app_posts_multipart_with_auth_and_query_params() {
        // The publish path must POST to {base}/apps with the tenant,
        // SchemaUpdateMode and DependencyPublishingOption query params, a
        // Bearer auth header, and a multipart body — and treat 2xx as success.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .and(query_param("tenant", "default"))
            .and(query_param("SchemaUpdateMode", "Synchronize"))
            .and(query_param("DependencyPublishingOption", "Default"))
            .and(header(AUTHORIZATION, "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = config_for_mock(&server);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"fake-app-bytes").unwrap();

        let http = reqwest::Client::new();
        publish_app(&http, &cfg, "test-token", tmp.path())
            .await
            .expect("2xx publish must succeed");
        // `.expect(1)` + drop verifies exactly one matching request arrived.
    }

    #[tokio::test]
    async fn publish_app_surfaces_http_error_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_string("App already published with newer version"),
            )
            .mount(&server)
            .await;

        let cfg = config_for_mock(&server);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"bytes").unwrap();

        let err = publish_app(&reqwest::Client::new(), &cfg, "tok", tmp.path())
            .await
            .expect_err("409 must fail");
        match err {
            DapError::PublishFailed(m) => {
                assert!(m.contains("409"), "status in message: {m}");
                assert!(
                    m.contains("App already published"),
                    "server body in message: {m}"
                );
            }
            other => panic!("expected PublishFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_app_missing_file_errors_before_request() {
        // A non-existent .app path must fail at the read step with PublishFailed
        // and never hit the server (mounting nothing + .expect default = 0).
        let server = MockServer::start().await;
        let cfg = config_for_mock(&server);
        let err = publish_app(
            &reqwest::Client::new(),
            &cfg,
            "tok",
            Path::new("/nonexistent/does-not-exist.app"),
        )
        .await
        .expect_err("missing file must fail");
        match err {
            DapError::PublishFailed(m) => {
                assert!(m.contains("Failed to read .app file"), "read error: {m}");
            }
            other => panic!("expected PublishFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_metadata_parses_json_with_auth_and_tenant_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/BC/dev/metadata"))
            .and(query_param("tenant", "default"))
            .and(header(AUTHORIZATION, "Bearer meta-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"platform": "26.0", "applications": []})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let cfg = config_for_mock(&server);
        let body = get_metadata(&reqwest::Client::new(), &cfg, "meta-token")
            .await
            .expect("2xx metadata must parse");
        assert_eq!(body["platform"], "26.0");
        assert!(body["applications"].is_array());
    }

    #[tokio::test]
    async fn get_metadata_non_2xx_becomes_connection_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/BC/dev/metadata"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized: token expired"))
            .mount(&server)
            .await;

        let cfg = config_for_mock(&server);
        let err = get_metadata(&reqwest::Client::new(), &cfg, "tok")
            .await
            .expect_err("401 must fail");
        match err {
            DapError::ConnectionFailed(m) => {
                assert!(m.contains("401"), "status in message: {m}");
                assert!(m.contains("token expired"), "body in message: {m}");
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_metadata_invalid_json_body_becomes_connection_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/BC/dev/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_string("this is not json {"))
            .mount(&server)
            .await;

        let cfg = config_for_mock(&server);
        let err = get_metadata(&reqwest::Client::new(), &cfg, "tok")
            .await
            .expect_err("invalid JSON must fail");
        match err {
            DapError::ConnectionFailed(m) => {
                assert!(m.contains("Bad metadata response"), "parse error: {m}");
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
    }
}
