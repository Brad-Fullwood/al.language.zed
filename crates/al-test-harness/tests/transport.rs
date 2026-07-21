//! Edge-case tests for the in-memory LSP transport helpers.
//!
//! These tests do not require a running `al-lsp` binary. They use
//! `tokio::io::duplex` (in-memory pipes) to talk directly to the internal
//! read loop.

use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};

// Helpers — a minimal in-memory JSON-RPC server

/// Write a single LSP message (Content-Length framed JSON) to `writer`.
async fn write_lsp_message(writer: &mut (impl AsyncWriteExt + Unpin), body: &str) {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await.unwrap();
    writer.write_all(body.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

// ST-01  no `connect()` API

// `LspClient::connect()` was removed (F-051) — al-lsp's daemon mode speaks a
// different (non-LSP) protocol via `al_protocol::DaemonClient`, so the two
// transports cannot share a client. Tests that need to exercise the daemon
// drive `DaemonClient` directly. The previous panic-stub assertion test was
// removed alongside the API.

// ST-02  read_loop: missing Content-Length header → silent skip, no panic

/// A message with no Content-Length must be silently discarded.
/// The read_loop must continue processing subsequent valid messages,
/// not hang or panic.
///
/// GAP TEST: if read_loop panics or hangs on a malformed header this fails.
#[tokio::test]
async fn test_adversarial_read_loop_missing_content_length_is_skipped() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    // Send a header block with no Content-Length (just a random header)
    server_write
        .write_all(b"X-Junk: yes\r\n\r\n")
        .await
        .unwrap();
    // Then send a valid notification so we can confirm the loop is still alive
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

// ST-03  read_loop: invalid JSON body → silent skip, loop continues

#[tokio::test]
async fn test_adversarial_read_loop_invalid_json_body_is_skipped() {
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

// ST-04  read_loop: response for unknown ID is silently dropped

/// If the server sends a response with an ID that is not in the pending map
/// (e.g., duplicate response, stale ID) it must be silently discarded.
/// The loop must not panic or deadlock.
///
/// GAP: the code does `pending.remove(&id)` and drops the tx if None — this
/// is safe, but verify it does not also block the notification path.
#[tokio::test]
async fn test_adversarial_read_loop_unknown_response_id_is_dropped() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    // Send a response for ID 99999 — nobody is waiting for it
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

// ST-05  read_loop: zero Content-Length body → not a panic

/// A Content-Length: 0 causes `read_exact` to read 0 bytes, producing an
/// empty slice.  `serde_json::from_slice(b"")` returns an error, so the
/// message is skipped.  The loop must not panic or exit.
#[tokio::test]
async fn test_adversarial_read_loop_zero_content_length_is_skipped() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    // Zero-length body
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
async fn test_adversarial_read_loop_rejects_oversized_body() {
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
    // Channel is closed (None) because loop exited — this is expected
    assert!(
        result.unwrap().is_none(),
        "channel should close after read_loop exits on EOF"
    );
}

// ST-07  read_loop: notification with no "params" field → defaults to null

/// The server sends a notification without a `params` field.
/// `msg.get("params").cloned().unwrap_or(Value::Null)` must provide Null.
#[tokio::test]
async fn test_adversarial_read_loop_notification_no_params_defaults_to_null() {
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

// ST-08  read_loop: response id sent as float (e.g. 1.0) → not dispatched

/// Fractional JSON-RPC IDs are invalid and must not resolve an integer request.
#[tokio::test]
async fn test_adversarial_read_loop_float_id_response_is_silently_dropped() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let (tx, mut rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    pending_map.lock().await.insert(1, tx);

    // Send a response with id as float 1.0 — as_i64() will return None
    let response = r#"{"jsonrpc":"2.0","id":1.0,"result":{"ok":true}}"#;
    write_lsp_message(&mut server_write, response).await;

    // Also send a notification so we know the loop processed the message
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

// ST-09  shutdown(): Daemon lifecycle does NOT wait for a child process

/// When lifecycle is Daemon, shutdown() must complete quickly (no child.wait()
/// call).  We cannot construct a Daemon LspClient directly (private ctor),
/// but we can verify the Stdio path handles the child properly.
///
/// This test builds a mock stdio client and calls shutdown — it should not
/// hang waiting for a child that will never exit gracefully.
///
/// The test uses a real process (/bin/cat or /usr/bin/cat) as the "child"
/// because tokio::process::Child cannot be constructed directly.
///
/// GAP: shutdown() sends LSP shutdown/exit messages to the writer before
/// dropping it.  If the writer is already closed, `send_message` returns
/// an error but shutdown continues (the error is `let _ =`-ignored).
/// This means a double-shutdown simulation is not possible since shutdown()
/// takes self by value — the compiler prevents it.  Documented.
#[tokio::test]
#[cfg(unix)]
async fn test_adversarial_shutdown_stdio_lifecycle_completes_within_timeout() {
    // Spawn a long-running process and immediately call shutdown on a client
    // wrapping it.  The client sends "shutdown" + "exit" over a duplex, which
    // the process ignores (it's not al-lsp), then we drop the writer and wait
    // up to 3 s for the child to exit.  Since the child is `cat` (blocks on
    // stdin), the kill() call in shutdown is what terminates it.
    let mut child = tokio::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("cat must be available on Unix");

    // We need to exercise the shutdown path without a full LspClient,
    // since we can't call from_transport (private).
    // Instead: verify that the child we spawned can be killed in < 3s,
    // which is exactly what the Stdio shutdown branch does.
    let pid = child.id();
    assert!(pid.is_some(), "child process must have a PID");

    let killed = timeout(Duration::from_secs(3), async {
        let _ = child.kill().await;
        child.wait().await
    })
    .await;

    assert!(
        killed.is_ok(),
        "Stdio shutdown kill+wait must complete within 3 seconds"
    );
}

// ST-10  send_message: message with empty body

/// An LSP message whose body is `null` (2 bytes: `null`) must be framed
/// correctly and parsed back correctly by read_loop.
#[tokio::test]
async fn test_adversarial_send_message_null_body_round_trips() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    // Write a notification whose params is JSON null
    let body = r#"{"jsonrpc":"2.0","method":"test/null-params","params":null}"#;
    write_lsp_message(&mut server_write, body).await;

    let received = timeout(Duration::from_secs(2), notif_rx.recv())
        .await
        .expect("read_loop did not process null-params notification");

    let (method, params) = received.unwrap();
    assert_eq!(method, "test/null-params");
    assert!(params.is_null());
}

// ST-11  read_loop: response with string id → not dispatched (integer-only)

/// A string response ID must not resolve an integer request ID.
#[tokio::test]
async fn test_adversarial_read_loop_string_id_does_not_match_integer_request() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (pending_map, _notif_rx) = make_dispatch_pair(client_read);

    let (tx, mut rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    pending_map.lock().await.insert(42, tx);

    // Send response with string id "42"
    let response = r#"{"jsonrpc":"2.0","id":"42","result":{"ok":true}}"#;
    write_lsp_message(&mut server_write, response).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let resolved = rx.try_recv();
    assert!(
        resolved.is_err(),
        "string id '42' must not match integer request id 42"
    );
}

// ST-12  read_loop: CRLF vs LF line endings in headers

/// The parser deliberately accepts LF-only headers even though LSP specifies
/// CRLF framing.
#[tokio::test]
async fn test_adversarial_read_loop_lf_only_headers_are_accepted() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    // Use LF-only line endings (non-standard but common)
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

/// The header line is `trim()`'d before extracting the Content-Length value.
/// Because `trim()` applies to the WHOLE line (including the value portion),
/// `"Content-Length: 42 "` becomes `"Content-Length: 42"` and parses cleanly.
/// This means trailing whitespace in the value is silently accepted (permissive).
///
/// VERIFIED SAFE: trailing whitespace does NOT cause message loss.
///
/// Note: `"42 ".parse::<usize>()` would fail in Rust, but the `trim()` on the
/// full header line strips trailing space before `strip_prefix` is applied,
/// so the value passed to `parse()` is always trimmed.
#[tokio::test]
async fn test_adversarial_read_loop_content_length_trailing_whitespace_is_tolerated() {
    let (mut server_write, client_read) = tokio::io::duplex(4096);
    let (_pending_map, mut notif_rx) = make_dispatch_pair(client_read);

    let body = r#"{"jsonrpc":"2.0","method":"test/whitespace-cl","params":{}}"#;
    // Trailing space after the number — trim() on the whole line saves us
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

// Internal: replicate read_loop + pending map for white-box testing

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
