//! The loopback redirect server that receives the authorization code, and
//! launching the browser that sends it there.

use super::encoding::{html_escape, percent_decode};
use super::OAuthError;

/// Read an HTTP request from an async stream, looping until the header
/// terminator `\r\n\r\n` is seen or the 8 KiB buffer limit is reached.
/// This handles TCP segmentation where a single `read()` may not deliver
/// the full request line containing the query string.
pub(super) async fn read_http_request<R: tokio::io::AsyncRead + Unpin>(
    stream: R,
) -> Result<String, OAuthError> {
    use tokio::io::AsyncReadExt;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = reader
            .read(&mut tmp)
            .await
            .map_err(|e| OAuthError::Protocol {
                error: "read_failed".into(),
                description: format!("Failed to read HTTP request: {e}"),
            })?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 8192 {
            break;
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(buf).map_err(|e| OAuthError::Protocol {
        error: "invalid_utf8".into(),
        description: format!("HTTP request is not valid UTF-8: {e}"),
    })
}

pub(super) async fn wait_for_auth_callback(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, OAuthError> {
    use tokio::io::AsyncWriteExt;

    let (stream, _) = listener.accept().await?;

    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = read_http_request(read_half).await?;

    // Parse first line: GET /?code=...&state=... HTTP/1.1
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split('?').nth(1).unwrap_or("");
    let params = parse_query_string(query);

    let (status_line, body) = if params.contains_key("error") {
        let err = params.get("error").map(|s| s.as_str()).unwrap_or("unknown");
        let desc = params
            .get("error_description")
            .map(|s| s.as_str())
            .unwrap_or("");
        (
            "HTTP/1.1 400 Bad Request",
            format!(
                "<html><body style=\"font-family:system-ui;display:flex;justify-content:center;\
                 align-items:center;height:100vh;margin:0\">\
                 <div style=\"text-align:center\">\
                 <h2 style=\"color:#c00\">Sign-in failed</h2>\
                 <p>{}: {}</p>\
                 <p style=\"color:#666\">You can close this tab.</p>\
                 </div></body></html>",
                html_escape(err),
                html_escape(desc)
            ),
        )
    } else {
        (
            "HTTP/1.1 200 OK",
            "<html><body style=\"font-family:system-ui;display:flex;justify-content:center;\
             align-items:center;height:100vh;margin:0\">\
             <div style=\"text-align:center\">\
             <h2>Signed in successfully</h2>\
             <p style=\"color:#666\">You can close this tab and return to your editor.</p>\
             </div></body></html>"
                .to_string(),
        )
    };

    let response = format!(
        "{status_line}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    if let Err(e) = write_half.write_all(response.as_bytes()).await {
        tracing::warn!("OAuth callback: failed to write HTTP response to browser: {e}");
    }
    if let Err(e) = write_half.shutdown().await {
        tracing::debug!("OAuth callback: shutdown of browser socket failed: {e}");
    }

    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        return Err(if err == "access_denied" {
            OAuthError::Denied
        } else {
            OAuthError::Protocol {
                error: err.clone(),
                description: percent_decode(&desc),
            }
        });
    }

    let state = params.get("state").map(|s| s.as_str()).unwrap_or("");
    if state != expected_state {
        return Err(OAuthError::Protocol {
            error: "state_mismatch".into(),
            description: "CSRF state parameter mismatch".into(),
        });
    }

    params
        .get("code")
        .cloned()
        .ok_or_else(|| OAuthError::Protocol {
            error: "missing_code".into(),
            description: "No authorization code in redirect".into(),
        })
}

/// Parse a URL query string into key-value pairs.
/// Both keys and values are percent-decoded.
pub(super) fn parse_query_string(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next().unwrap_or("");
            Some((percent_decode(key), percent_decode(val)))
        })
        .collect()
}

/// Open `url` in the user's browser. See [`al_types::browser`] for why Windows
/// does not go through `cmd /c start`.
pub(super) fn open_browser(url: &str) -> bool {
    match al_types::open_in_browser(url) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!("open_browser: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_string_parsing() {
        let params = parse_query_string("code=abc123&state=xyz&session_state=foo");
        assert_eq!(params.get("code").unwrap(), "abc123");
        assert_eq!(params.get("state").unwrap(), "xyz");
    }

    #[test]
    fn query_string_percent_decodes_values() {
        // Simulate a real OAuth redirect where error_description is percent-encoded
        let params = parse_query_string(
            "error=access_denied&error_description=The%20user%20denied%20access%2E",
        );
        assert_eq!(params.get("error").unwrap(), "access_denied");
        assert_eq!(
            params.get("error_description").unwrap(),
            "The user denied access."
        );
    }

    #[test]
    fn query_string_decodes_plus_as_space() {
        let params = parse_query_string("msg=hello+world");
        assert_eq!(params.get("msg").unwrap(), "hello world");
    }

    #[tokio::test]
    async fn read_http_request_handles_complete_request() {
        use tokio::io::AsyncWriteExt;

        // Set up a loopback pair: write a full HTTP request, read it back
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let write_task = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = b"GET /?code=AUTH_CODE&state=STATE HTTP/1.1\r\nHost: localhost\r\n\r\n";
            client.write_all(req).await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result = read_http_request(stream).await.unwrap();

        write_task.await.unwrap();

        assert!(result.contains("GET /?code=AUTH_CODE"));
        assert!(result.contains("\r\n\r\n"));
    }

    /// Drive `wait_for_auth_callback` against a loopback listener: a client
    /// connects and sends a single GET line carrying `query`, then the callback
    /// processes it. Returns the callback's result.
    async fn run_callback_with_query(
        query: &str,
        expected_state: &str,
    ) -> Result<String, OAuthError> {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let q = query.to_string();
        let writer = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = format!("GET /?{q} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            client.write_all(req.as_bytes()).await.unwrap();
            // Drain the HTTP response so the callback's write_all succeeds.
            use tokio::io::AsyncReadExt;
            let mut sink = Vec::new();
            let _ = client.read_to_end(&mut sink).await;
        });
        let res = wait_for_auth_callback(&listener, expected_state).await;
        writer.await.unwrap();
        res
    }

    #[tokio::test]
    async fn callback_extracts_code_when_state_matches() {
        let code = run_callback_with_query("code=AUTH123&state=GOOD", "GOOD")
            .await
            .expect("happy path yields the code");
        assert_eq!(code, "AUTH123");
    }

    #[tokio::test]
    async fn callback_rejects_state_mismatch() {
        // CSRF defence: a code arriving with the wrong state must be refused.
        let err = run_callback_with_query("code=AUTH123&state=ATTACKER", "EXPECTED")
            .await
            .unwrap_err();
        match err {
            OAuthError::Protocol { error, .. } => assert_eq!(error, "state_mismatch"),
            other => panic!("expected state_mismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn callback_maps_access_denied_to_denied() {
        let err = run_callback_with_query("error=access_denied&error_description=nope", "GOOD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, OAuthError::Denied),
            "access_denied must map to Denied, got {err:?}"
        );
    }

    #[tokio::test]
    async fn callback_surfaces_other_oauth_errors() {
        let err =
            run_callback_with_query("error=invalid_scope&error_description=Bad%20scope", "GOOD")
                .await
                .unwrap_err();
        match err {
            OAuthError::Protocol { error, description } => {
                assert_eq!(error, "invalid_scope");
                assert_eq!(description, "Bad scope", "description is percent-decoded");
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn callback_errors_when_code_missing() {
        // No error, state matches, but no code present → missing_code.
        let err = run_callback_with_query("state=GOOD&session_state=x", "GOOD")
            .await
            .unwrap_err();
        match err {
            OAuthError::Protocol { error, .. } => assert_eq!(error, "missing_code"),
            other => panic!("expected missing_code, got {other:?}"),
        }
    }
}
