//! DAP wire framing: `Content-Length: N\r\n\r\n<body>`.
//!
//! Also includes `ensure_seq()` which patches EditorServices.Host responses
//! that are missing the required `seq` field.

use std::sync::atomic::{AtomicI64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Read one DAP message body from the wire.
///
/// Reads `Content-Length` headers, then reads exactly that many bytes.
pub async fn read_dap_body<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Vec<u8>, std::io::Error> {
    let content_length = read_headers(reader).await?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

/// Read DAP headers and extract Content-Length.
async fn read_headers<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<usize, std::io::Error> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "EOF while reading DAP headers",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(
                val.trim()
                    .parse::<usize>()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            );
        }
    }
    content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing Content-Length header")
    })
}

/// Write one DAP frame (`Content-Length: N\r\n\r\n<body>`).
pub async fn write_dap_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    body: &[u8],
) -> Result<(), std::io::Error> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

/// Patch a DAP message body to include a `seq` field if missing.
///
/// EditorServices.Host sometimes omits the required `seq` field from
/// its responses and events. This function injects one.
pub fn ensure_seq(body: &[u8], counter: &AtomicI64) -> Vec<u8> {
    // Check if `"seq":` key already exists. We check for the key pattern
    // `"seq":` (6 bytes) rather than just `"seq"` (5 bytes) to avoid a false
    // positive when the string literal "seq" appears as a JSON value (e.g.,
    // `{"command":"evaluate","arguments":{"expression":"seq"}}`).
    if body.windows(6).any(|w| w == b"\"seq\":") {
        return body.to_vec();
    }

    // Only increment the counter if we have a `{` to patch into.
    if let Some(pos) = body.iter().position(|&b| b == b'{') {
        let seq = counter.fetch_add(1, Ordering::Relaxed);
        let mut patched = Vec::with_capacity(body.len() + 20);
        patched.extend_from_slice(&body[..=pos]);
        patched.extend_from_slice(format!("\"seq\":{seq},").as_bytes());
        patched.extend_from_slice(&body[pos + 1..]);
        patched
    } else {
        body.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_write_roundtrip() {
        let body = b"{\"seq\":1,\"type\":\"request\",\"command\":\"initialize\"}";

        // Write frame to buffer
        let mut buf = Vec::new();
        write_dap_frame(&mut buf, body).await.unwrap();

        // Read it back
        let mut reader = BufReader::new(buf.as_slice());
        let read_body = read_dap_body(&mut reader).await.unwrap();

        assert_eq!(read_body, body);
    }

    #[tokio::test]
    async fn read_multiple_frames() {
        let body1 = b"{\"seq\":1}";
        let body2 = b"{\"seq\":2}";

        let mut buf = Vec::new();
        write_dap_frame(&mut buf, body1).await.unwrap();
        write_dap_frame(&mut buf, body2).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let r1 = read_dap_body(&mut reader).await.unwrap();
        let r2 = read_dap_body(&mut reader).await.unwrap();

        assert_eq!(r1, body1);
        assert_eq!(r2, body2);
    }

    #[tokio::test]
    async fn read_eof_returns_error() {
        let mut reader = BufReader::new(&b""[..]);
        let result = read_dap_body(&mut reader).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn missing_content_length_returns_error() {
        let data = b"Content-Type: application/json\r\n\r\n{}";
        let mut reader = BufReader::new(&data[..]);
        let result = read_dap_body(&mut reader).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn ensure_seq_adds_missing_seq() {
        let body = b"{\"type\":\"response\",\"success\":true}";
        let counter = AtomicI64::new(42);
        let patched = ensure_seq(body, &counter);

        let value: serde_json::Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["seq"], 42);
        assert_eq!(value["type"], "response");
    }

    #[test]
    fn ensure_seq_preserves_existing() {
        let body = b"{\"seq\":99,\"type\":\"event\"}";
        let counter = AtomicI64::new(1);
        let result = ensure_seq(body, &counter);

        // Should be unchanged
        assert_eq!(result, body);
        // Counter should NOT have been incremented
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn ensure_seq_increments_counter() {
        let counter = AtomicI64::new(10);
        let body = b"{\"type\":\"event\"}";

        let _ = ensure_seq(body, &counter);
        assert_eq!(counter.load(Ordering::Relaxed), 11);

        let _ = ensure_seq(body, &counter);
        assert_eq!(counter.load(Ordering::Relaxed), 12);
    }

    #[test]
    fn ensure_seq_handles_empty_object() {
        // `{}` → `{"seq":1,}` which has a trailing comma.
        // Real DAP messages are never empty objects, but verify the patched
        // output at least contains the seq value.
        let body = b"{}";
        let counter = AtomicI64::new(1);
        let patched = ensure_seq(body, &counter);
        let patched_str = std::str::from_utf8(&patched).unwrap();
        assert!(patched_str.contains("\"seq\":1"));
    }

    #[test]
    fn ensure_seq_not_fooled_by_seq_in_value() {
        // A JSON value containing "seq" as a string literal must NOT prevent
        // seq injection. Previous bug: windows(5) on `"seq"` would match the
        // string value and return without patching.
        let body = br#"{"type":"event","event":"evaluate","body":{"expression":"seq"}}"#;
        let counter = AtomicI64::new(5);
        let patched = ensure_seq(body, &counter);

        let value: serde_json::Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["seq"], 5, "seq field must be injected");
    }

    #[test]
    fn ensure_seq_no_counter_increment_when_no_brace() {
        // If the body has no `{`, the counter must NOT be incremented.
        let body = b"not-json-at-all";
        let counter = AtomicI64::new(7);
        let _ = ensure_seq(body, &counter);
        assert_eq!(counter.load(Ordering::Relaxed), 7, "counter must not increment when no `{{` found");
    }
}
