//! Edge-case tests for the in-memory LSP transport helpers.
//!
//! These tests do not require a running `al-lsp` binary. They use in-memory
//! `tokio::io::duplex` pipes to talk directly to the internal read loop.

use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};

/// Write a single LSP message (Content-Length framed JSON) to `writer`.
async fn write_lsp_message(writer: &mut (impl AsyncWriteExt + Unpin), body: &str) {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await.unwrap();
    writer.write_all(body.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

/// A message with no Content-Length must be silently discarded.
/// The read_loop must continue processing subsequent valid messages,
/// not hang or panic.
#[tokio::test]
async fn read_loop_skips_missing_content_length() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    server_write
        .write_all(b"X-Junk: yes\r\n\r\n")
        .await
        .unwrap();
    let notif = r#"{"jsonrpc":"2.0","method":"test/ping","params":{"ok":true}}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop timed out after skipping malformed header — loop may have exited");

    assert!(
        received.is_some(),
        "read_loop must continue after a missing Content-Length"
    );
    let (method, _) = received.unwrap();
    assert_eq!(method, "test/ping");

    assert!(pending_map.try_lock().is_ok());
}

#[tokio::test]
async fn read_loop_skips_invalid_json_body() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let garbage = b"not { valid ] json at all !!!";
    let header = format!("Content-Length: {}\r\n\r\n", garbage.len());
    server_write.write_all(header.as_bytes()).await.unwrap();
    server_write.write_all(garbage).await.unwrap();
    server_write.flush().await.unwrap();

    let notif = r#"{"jsonrpc":"2.0","method":"test/alive","params":{}}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop timed out after invalid JSON — loop may have exited");

    assert!(
        received.is_some(),
        "read_loop must continue after invalid JSON body"
    );
    let (method, _) = received.unwrap();
    assert_eq!(method, "test/alive");
}

/// If the server sends a response with an ID that is not in the pending map
/// (e.g., duplicate response, stale ID) it must be silently discarded.
/// The loop must not panic or deadlock.
#[tokio::test]
async fn read_loop_drops_unknown_response_id() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let stale = r#"{"jsonrpc":"2.0","id":99999,"result":{"stale":true}}"#;
    write_lsp_message(&mut server_write, stale).await;

    let notif = r#"{"jsonrpc":"2.0","method":"test/still-alive","params":{}}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop hung after receiving response for unknown ID");

    assert!(
        received.is_some(),
        "notification must be delivered after stale response ID"
    );
    let (method, _) = received.unwrap();
    assert_eq!(method, "test/still-alive");
}

/// A Content-Length: 0 causes `read_exact` to read 0 bytes, producing an
/// empty slice.  `serde_json::from_slice(b"")` returns an error, so the
/// message is skipped.  The loop must not panic or exit.
#[tokio::test]
async fn read_loop_skips_zero_content_length() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    server_write
        .write_all(b"Content-Length: 0\r\n\r\n")
        .await
        .unwrap();
    server_write.flush().await.unwrap();

    let notif = r#"{"jsonrpc":"2.0","method":"test/post-zero","params":{}}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop exited after zero-length body");

    assert!(received.is_some());
    let (method, _) = received.unwrap();
    assert_eq!(method, "test/post-zero");
}

/// Content lengths above the harness's 64 MiB cap are rejected before reading
/// or allocating the body.
#[tokio::test]
async fn read_loop_rejects_oversized_body() {
    let (mut server_write, client_read) = tokio::io::duplex(65536);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let huge_length = 65 * 1024 * 1024usize;
    let header = format!("Content-Length: {huge_length}\r\n\r\n");
    server_write.write_all(header.as_bytes()).await.unwrap();
    server_write.flush().await.unwrap();

    let result = timeout(Duration::from_secs(3), notif_rx.recv()).await;

    assert!(
        result.is_ok(),
        "read_loop must reject the oversized body promptly"
    );
    assert!(
        result.unwrap().is_none(),
        "channel should close after read_loop exits on EOF"
    );
}

/// The server sends a notification without a `params` field.
/// `msg.get("params").cloned().unwrap_or(Value::Null)` must provide Null.
#[tokio::test]
async fn read_loop_defaults_missing_notification_params_to_null() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let notif = r#"{"jsonrpc":"2.0","method":"test/no-params"}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop hung on notification with no params");

    let (method, params) = received.unwrap();
    assert_eq!(method, "test/no-params");
    assert!(
        params.is_null(),
        "missing params must default to JSON null, got: {params:?}"
    );
}

/// Fractional JSON-RPC IDs are invalid and must not resolve an integer request.
#[tokio::test]
async fn read_loop_drops_fractional_response_id() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let (tx, mut rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    pending_map.lock().await.insert(1, tx);

    let response = r#"{"jsonrpc":"2.0","id":1.0,"result":{"ok":true}}"#;
    write_lsp_message(&mut server_write, response).await;

    let notif = r#"{"jsonrpc":"2.0","method":"test/after-float-id","params":{}}"#;
    write_lsp_message(&mut server_write, notif).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .ok()
        .flatten();
    assert!(
        received.is_some(),
        "loop must continue after float-id response"
    );

    let resolved = rx.try_recv();
    assert!(
        resolved.is_err(),
        "fractional id 1.0 must not match integer request id 1"
    );
}

/// An LSP message whose body is `null` (2 bytes: `null`) must be framed
/// correctly and parsed back correctly by read_loop.
#[tokio::test]
async fn send_message_null_body_round_trips() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let body = r#"{"jsonrpc":"2.0","method":"test/null-params","params":null}"#;
    write_lsp_message(&mut server_write, body).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop did not process null-params notification");

    let (method, params) = received.unwrap();
    assert_eq!(method, "test/null-params");
    assert!(params.is_null());
}

/// A string response ID must not resolve an integer request ID.
#[tokio::test]
async fn read_loop_string_id_does_not_match_integer_request() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, _notif_rx) = make_dispatch_pair(client_read);

    let (tx, mut rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    pending_map.lock().await.insert(42, tx);

    let response = r#"{"jsonrpc":"2.0","id":"42","result":{"ok":true}}"#;
    write_lsp_message(&mut server_write, response).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resolved = rx.try_recv();
    assert!(
        resolved.is_err(),
        "string id '42' must not match integer request id 42"
    );
}

/// The parser deliberately accepts LF-only headers even though LSP specifies
/// CRLF framing.
#[tokio::test]
async fn read_loop_accepts_lf_only_headers() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let body = r#"{"jsonrpc":"2.0","method":"test/lf-only","params":{}}"#;
    let header = format!("Content-Length: {}\n\n", body.len()); // LF not CRLF
    server_write.write_all(header.as_bytes()).await.unwrap();
    server_write.write_all(body.as_bytes()).await.unwrap();
    server_write.flush().await.unwrap();

    let received = timeout(Duration::from_secs(2), notif_rx.recv()).await;
    let (method, _) = received
        .expect("read_loop timed out on LF-only header")
        .expect("notification channel closed");
    assert_eq!(method, "test/lf-only");
}

/// Trailing whitespace in a Content-Length header is accepted.
#[tokio::test]
async fn read_loop_accepts_content_length_with_trailing_whitespace() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let body = r#"{"jsonrpc":"2.0","method":"test/whitespace-cl","params":{}}"#;
    let header = format!("Content-Length: {} \r\n\r\n", body.len());
    server_write.write_all(header.as_bytes()).await.unwrap();
    server_write.write_all(body.as_bytes()).await.unwrap();
    server_write.flush().await.unwrap();

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop hung — expected whitespace-cl to be delivered");

    let (method, _) = received.unwrap();
    assert_eq!(
        method, "test/whitespace-cl",
        "trailing whitespace in Content-Length value must be tolerated (line trim() covers it)"
    );
}

/// Construct a (pending_map, notification_rx) pair backed by `read_loop`
/// running on `reader`.  This replicates what `from_transport` does internally,
/// so we can test the loop's behavior without needing a public constructor.
type PendingMap = std::sync::Arc<
    tokio::sync::Mutex<
        std::collections::HashMap<i64, tokio::sync::oneshot::Sender<serde_json::Value>>,
    >,
>;
type NotifRx = tokio::sync::mpsc::Receiver<(String, serde_json::Value)>;

fn make_dispatch_pair(
    reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> (PendingMap, NotifRx) {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::io::BufReader;
    use tokio::sync::{mpsc, Mutex};

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let (notif_tx, notif_rx) = mpsc::channel(10_000);
    let pending_clone = pending.clone();

    tokio::spawn(async move {
        al_test_harness::read_loop(BufReader::new(reader), pending_clone, notif_tx).await;
    });

    (pending, notif_rx)
}
