//! Low-level DAP client.
//!
//! `DapClient` spawns a subprocess, sends DAP requests over stdin,
//! and reads responses/events from stdout. A background task reads
//! from the child's stdout, parses DAP frames, patches missing `seq`,
//! and routes messages: responses go to a per-request oneshot channel,
//! events go to an mpsc channel for the caller to drain.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::BufWriter;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::framing::{ensure_seq, read_dap_body, write_dap_frame};
use super::protocol::{DapEvent, DapMessage, DapResponse};
use super::{DapError, Result};

/// Low-level DAP client that manages a subprocess.
///
/// Not `Sync` because `events_rx` is an `mpsc::UnboundedReceiver` which is
/// single-consumer. Use from a single task only; share via `Arc<Mutex<DapClient>>`
/// if cross-task access is needed.
pub struct DapClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    seq_counter: AtomicI64,
    /// Pending response waiters: request_seq → oneshot sender.
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<DapResponse>>>>,
    /// Events received from the adapter.
    events_rx: mpsc::UnboundedReceiver<DapEvent>,
    /// Background reader task handle.
    reader_task: tokio::task::JoinHandle<()>,
}

impl DapClient {
    /// Spawn a subprocess and start reading DAP messages from its stdout.
    pub fn spawn(binary: &Path, args: &[&str]) -> Result<Self> {
        let mut child = tokio::process::Command::new(binary)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| DapError::SpawnFailed(format!("{}: {}", binary.display(), e)))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DapError::SpawnFailed("child stdin/stdout not available".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DapError::SpawnFailed("child stdin/stdout not available".to_string()))?;

        let seq_counter = AtomicI64::new(1);
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<DapResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        // Background task: read DAP frames from stdout, dispatch responses and events
        let pending_clone = pending.clone();
        let reader_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stdout);
            let patch_counter = AtomicI64::new(1000); // separate counter for seq patching
            loop {
                match read_dap_body(&mut reader).await {
                    Ok(body) => {
                        let patched = ensure_seq(&body, &patch_counter);
                        match DapMessage::parse(&patched) {
                            Ok(DapMessage::Response(resp)) => {
                                let mut pending = pending_clone.lock().await;
                                if let Some(sender) = pending.remove(&resp.request_seq) {
                                    let _ = sender.send(resp);
                                } else {
                                    tracing::debug!(
                                        request_seq = resp.request_seq,
                                        command = %resp.command,
                                        "Unmatched DAP response"
                                    );
                                }
                            }
                            Ok(DapMessage::Event(event)) => {
                                let _ = events_tx.send(event);
                            }
                            Ok(DapMessage::Request(req)) => {
                                // Reverse requests from adapter (e.g., runInTerminal)
                                tracing::debug!(command = %req.command, "Ignoring reverse request from adapter");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to parse DAP message");
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                        tracing::debug!("DAP subprocess stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Error reading DAP frame");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            seq_counter,
            pending,
            events_rx,
            reader_task,
        })
    }

    /// Send a DAP request and wait for the matching response.
    pub async fn send_request(
        &mut self,
        command: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<DapResponse> {
        self.send_request_timeout(command, arguments, Duration::from_secs(30))
            .await
    }

    /// Send a DAP request with a custom timeout.
    pub async fn send_request_timeout(
        &mut self,
        command: &str,
        arguments: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<DapResponse> {
        let seq = self.seq_counter.fetch_add(1, Ordering::Relaxed);
        let request = super::protocol::DapRequest {
            seq,
            type_: "request".to_string(),
            command: command.to_string(),
            arguments,
        };

        // Register response waiter before sending
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(seq, tx);

        // Send the request
        let body = serde_json::to_vec(&request)
            .map_err(|e| DapError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        write_dap_frame(&mut self.stdin, &body).await?;

        // Wait for response with timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if response.success {
                    Ok(response)
                } else {
                    Err(DapError::DapProtocolError {
                        command: command.to_string(),
                        message: response
                            .message
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    })
                }
            }
            Ok(Err(_)) => Err(DapError::DapProtocolError {
                command: command.to_string(),
                message: "Response channel closed (subprocess may have died)".to_string(),
            }),
            Err(_) => {
                // Clean up pending entry on timeout
                self.pending.lock().await.remove(&seq);
                Err(DapError::Timeout(timeout))
            }
        }
    }

    /// Drain all pending events (non-blocking).
    pub fn drain_events(&mut self) -> Vec<DapEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events_rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// Wait for the next event (blocking).
    pub async fn next_event(&mut self) -> Option<DapEvent> {
        self.events_rx.recv().await
    }

    /// Wait for a specific event by name, with timeout.
    pub async fn wait_for_event(
        &mut self,
        event_name: &str,
        timeout: Duration,
    ) -> Result<DapEvent> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.events_rx.recv()).await {
                Ok(Some(event)) if event.event == event_name => return Ok(event),
                Ok(Some(_)) => continue, // wrong event, keep waiting
                Ok(None) => {
                    return Err(DapError::DapProtocolError {
                        command: format!("wait_for_event({event_name})"),
                        message: "Event channel closed".to_string(),
                    })
                }
                Err(_) => return Err(DapError::Timeout(timeout)),
            }
        }
    }

    /// Kill the subprocess.
    pub async fn kill(&mut self) -> Result<()> {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.reader_task.abort();
        Ok(())
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        // Best-effort kill to prevent orphan processes
        let _ = self.child.start_kill();
        self.reader_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // We can't easily test DapClient::spawn() in unit tests since it needs
    // a real DAP subprocess. These tests verify the type system and basic
    // construction. Integration tests with a mock subprocess would go in
    // a separate test file.

    #[test]
    fn spawn_nonexistent_binary_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let result = DapClient::spawn(&PathBuf::from("/nonexistent/binary"), &[]);
            assert!(result.is_err());
            match result {
                Err(DapError::SpawnFailed(msg)) => {
                    assert!(msg.contains("/nonexistent/binary"), "got: {msg}");
                }
                Err(other) => panic!("Expected SpawnFailed, got: {other}"),
                Ok(_) => panic!("Expected error"),
            }
        });
    }

    #[tokio::test]
    async fn spawn_echo_and_kill() {
        // Spawn a simple process that we can kill
        let result = DapClient::spawn(&PathBuf::from("/usr/bin/cat"), &[]);
        if let Ok(mut client) = result {
            // Just verify we can kill it without panicking
            client.kill().await.unwrap();
        }
        // If cat doesn't exist (unlikely), that's ok — skip
    }

    // ----------------------------------------------------------------------
    // Mock DAP adapter harness.
    //
    // These tests exercise the REAL `DapClient::spawn` seam: a POSIX shell
    // script acts as a fake DAP adapter, reading `Content-Length`-framed
    // requests from stdin and emitting framed responses/events on stdout.
    //
    // `DapClient::spawn(binary, args)` forwards `args` straight to the
    // subprocess, so each `args` entry below is one scripted "step" the
    // fake adapter performs (read a request + reply, or emit an event).
    //
    // The client's `seq_counter` starts at 1 and increments by 1 per
    // request, so the adapter mirrors that with its own step counter `i`
    // to echo the correct `request_seq` without parsing stdin's payload.
    // ----------------------------------------------------------------------

    /// Body of the fake DAP adapter shell script.
    ///
    /// `read-*` steps consume exactly one request frame (so the client's
    /// `write_dap_frame` always completes before a reply is emitted, and so
    /// the pipe stays open); `event:*` steps emit an event without reading.
    const FAKE_ADAPTER: &str = r#"#!/bin/sh
export LC_ALL=C
i=1

read_frame() {
  len=0
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -dc '0-9A-Za-z:; ')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=$(printf '%s' "$line" | tr -dc '0-9') ;;
    esac
  done
  if [ -n "$len" ] && [ "$len" -gt 0 ] 2>/dev/null; then
    dd bs=1 count="$len" >/dev/null 2>&1
  fi
}

emit() {
  b="$1"
  printf 'Content-Length: %d\r\n\r\n%s' "${#b}" "$b"
}

for action in "$@"; do
  case "$action" in
    read-ok)
      read_frame
      emit "{\"seq\":$((1000+i)),\"type\":\"response\",\"request_seq\":$i,\"success\":true,\"command\":\"test\",\"body\":{\"ok\":true,\"n\":$i}}"
      i=$((i+1))
      ;;
    read-fail)
      read_frame
      emit "{\"seq\":$((1000+i)),\"type\":\"response\",\"request_seq\":$i,\"success\":false,\"command\":\"test\",\"message\":\"boom\"}"
      i=$((i+1))
      ;;
    read-hang)
      read_frame
      sleep 30
      i=$((i+1))
      ;;
    read-unmatched)
      read_frame
      emit "{\"seq\":$((1000+i)),\"type\":\"response\",\"request_seq\":9999,\"success\":true,\"command\":\"ghost\"}"
      emit "{\"seq\":$((2000+i)),\"type\":\"response\",\"request_seq\":$i,\"success\":true,\"command\":\"test\",\"body\":{\"ok\":true}}"
      i=$((i+1))
      ;;
    read-parseerr)
      read_frame
      emit "this-is-not-valid-json{{{"
      emit "{\"seq\":$((2000+i)),\"type\":\"response\",\"request_seq\":$i,\"success\":true,\"command\":\"test\",\"body\":{\"ok\":true}}"
      i=$((i+1))
      ;;
    read-reverse)
      read_frame
      emit "{\"seq\":$((1000+i)),\"type\":\"request\",\"command\":\"runInTerminal\",\"arguments\":{\"x\":1}}"
      emit "{\"seq\":$((2000+i)),\"type\":\"response\",\"request_seq\":$i,\"success\":true,\"command\":\"test\",\"body\":{\"ok\":true}}"
      i=$((i+1))
      ;;
    read-noseq)
      read_frame
      emit "{\"type\":\"response\",\"request_seq\":$i,\"success\":true,\"command\":\"test\",\"body\":{\"ok\":true}}"
      i=$((i+1))
      ;;
    event:*)
      name=${action#event:}
      emit "{\"seq\":$((3000+i)),\"type\":\"event\",\"event\":\"$name\",\"body\":{\"k\":\"$name\"}}"
      i=$((i+1))
      ;;
    hang)
      sleep 30
      ;;
    *)
      ;;
  esac
done
"#;

    /// Write the fake adapter script to a fresh tempdir and return both the
    /// `TempDir` (keep it alive for the spawned child — `sh` reads the script
    /// lazily for the whole run) and the script path.
    fn write_fake_adapter() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fake_dap_adapter.sh");
        std::fs::write(&path, FAKE_ADAPTER).expect("write script");
        (dir, path)
    }

    /// Spawn a `DapClient` backed by the fake adapter running `steps`.
    ///
    /// The script is run via `/bin/sh <script> <steps...>` rather than exec'd
    /// directly: `sh` opens the script read-only, sidestepping the ETXTBSY
    /// fork/exec race that hits freshly-written executables under parallel
    /// tests. This still drives the real `DapClient::spawn` seam.
    fn spawn_fake(steps: &[&str]) -> (tempfile::TempDir, DapClient) {
        let (dir, path) = write_fake_adapter();
        let path_str = path.to_str().expect("utf8 path").to_string();
        let mut args: Vec<&str> = Vec::with_capacity(steps.len() + 1);
        args.push(&path_str);
        args.extend_from_slice(steps);
        let client =
            DapClient::spawn(Path::new("/bin/sh"), &args).expect("spawn fake adapter via /bin/sh");
        (dir, client)
    }

    /// Bound an await so a regressed code path fails fast instead of hanging
    /// the whole test run (used during red/green mutation checks).
    async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(10), fut)
            .await
            .expect("operation did not complete within 10s (likely a regression)")
    }

    #[tokio::test]
    async fn send_request_success_routes_response_by_seq() {
        let (_dir, mut client) = spawn_fake(&["read-ok"]);
        // Default-timeout entry point (`send_request` -> `send_request_timeout`).
        let resp = bounded(client.send_request("test", Some(serde_json::json!({"a": 1}))))
            .await
            .expect("expected success response");
        assert!(resp.success);
        assert_eq!(resp.command, "test");
        // request_seq must match the first request's seq (1) — proves the
        // background reader routed by request_seq through the pending map.
        assert_eq!(resp.request_seq, 1);
        assert_eq!(resp.body.unwrap()["ok"], true);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn send_request_failure_maps_to_protocol_error() {
        let (_dir, mut client) = spawn_fake(&["read-fail"]);
        let err = bounded(client.send_request("test", None))
            .await
            .expect_err("expected protocol error");
        match err {
            DapError::DapProtocolError { command, message } => {
                assert_eq!(command, "test");
                assert_eq!(message, "boom");
            }
            other => panic!("expected DapProtocolError, got {other}"),
        }
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn send_request_timeout_returns_timeout_error() {
        let (_dir, mut client) = spawn_fake(&["read-hang"]);
        // Adapter reads the request then sleeps; we must hit the timeout arm.
        let timeout = Duration::from_millis(250);
        let err = client
            .send_request_timeout("test", None, timeout)
            .await
            .expect_err("expected timeout");
        // The custom timeout parameter must be honored and surfaced verbatim.
        match err {
            DapError::Timeout(d) => assert_eq!(d, timeout),
            other => panic!("expected Timeout, got {other}"),
        }
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn send_request_timeout_honors_custom_duration() {
        // A distinct, small custom timeout proves `send_request_timeout`
        // uses its parameter (not the hard-coded 30s default).
        let (_dir, mut client) = spawn_fake(&["read-hang"]);
        let custom = Duration::from_millis(120);
        let start = std::time::Instant::now();
        let err = client
            .send_request_timeout("test", None, custom)
            .await
            .expect_err("expected timeout");
        assert!(matches!(err, DapError::Timeout(d) if d == custom));
        // Should fire near the custom deadline, well under the 30s default.
        assert!(start.elapsed() < Duration::from_secs(5));
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn reader_patches_missing_seq_via_ensure_seq() {
        // The adapter omits the top-level `seq`. `DapResponse` requires it,
        // so without `ensure_seq` patching in the reader the message would
        // fail to parse and never reach the pending waiter (-> timeout).
        let (_dir, mut client) = spawn_fake(&["read-noseq"]);
        let resp = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("ensure_seq must let a seq-less response parse + route");
        assert!(resp.success);
        assert_eq!(resp.request_seq, 1);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn reader_skips_unmatched_response_then_delivers_match() {
        // First reply carries an unknown request_seq (unmatched -> logged,
        // dropped); the second carries the real one. The waiter must still
        // receive the matching response.
        let (_dir, mut client) = spawn_fake(&["read-unmatched"]);
        let resp = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("matching response must still be delivered");
        assert!(resp.success);
        assert_eq!(resp.command, "test");
        assert_eq!(resp.request_seq, 1);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn reader_skips_parse_error_then_delivers_match() {
        // A malformed frame must be logged and skipped without breaking the
        // stream; the following valid response must still be routed.
        let (_dir, mut client) = spawn_fake(&["read-parseerr"]);
        let resp = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("valid response after a parse error must be delivered");
        assert!(resp.success);
        assert_eq!(resp.request_seq, 1);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn reader_ignores_reverse_request_then_delivers_match() {
        // A reverse request (adapter -> client, e.g. runInTerminal) must be
        // ignored without disrupting response routing.
        let (_dir, mut client) = spawn_fake(&["read-reverse"]);
        let resp = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("response after a reverse request must be delivered");
        assert!(resp.success);
        assert_eq!(resp.request_seq, 1);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_requests_use_independent_seqs() {
        // Two sequential requests must get seq 1 then seq 2, each routed back
        // to the correct waiter.
        let (_dir, mut client) = spawn_fake(&["read-ok", "read-ok"]);
        let r1 = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("first response");
        let r2 = bounded(client.send_request_timeout("test", None, Duration::from_secs(5)))
            .await
            .expect("second response");
        assert_eq!(r1.request_seq, 1);
        assert_eq!(r1.body.unwrap()["n"], 1);
        assert_eq!(r2.request_seq, 2);
        assert_eq!(r2.body.unwrap()["n"], 2);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn next_event_returns_some_then_none_on_eof() {
        // Adapter emits one event then exits (EOF). The reader routes the
        // event to the mpsc channel, then ends and drops `events_tx`, so the
        // next `recv` resolves to None.
        let (_dir, mut client) = spawn_fake(&["event:stopped"]);
        let ev = bounded(client.next_event())
            .await
            .expect("expected an event");
        assert_eq!(ev.event, "stopped");
        // Adapter has exited -> channel closes -> None.
        let none = bounded(client.next_event()).await;
        assert!(none.is_none(), "expected None after adapter EOF");
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn drain_events_empty_when_no_events() {
        // Adapter emits nothing and stays alive; a non-blocking drain returns
        // an empty vec.
        let (_dir, mut client) = spawn_fake(&["hang"]);
        assert!(client.drain_events().is_empty());
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn drain_events_collects_buffered_events() {
        // Adapter emits three events then stays alive; the non-blocking drain
        // accumulates everything the reader has queued.
        let (_dir, mut client) = spawn_fake(&["event:a", "event:b", "event:c", "hang"]);
        let mut all = Vec::new();
        for _ in 0..200 {
            all.extend(client.drain_events());
            if all.len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(all.len(), 3, "drain should collect all buffered events");
        let names: Vec<&str> = all.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_event_filters_until_match() {
        // First event ("output") must be skipped; the loop keeps waiting until
        // the requested "stopped" event arrives.
        let (_dir, mut client) = spawn_fake(&["event:output", "event:stopped", "hang"]);
        let ev = bounded(client.wait_for_event("stopped", Duration::from_secs(5)))
            .await
            .expect("expected stopped event");
        assert_eq!(ev.event, "stopped");
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_event_times_out_when_event_never_arrives() {
        // Only a non-matching event arrives; the adapter then hangs, so the
        // deadline must fire with a Timeout error.
        let (_dir, mut client) = spawn_fake(&["event:output", "hang"]);
        let err = client
            .wait_for_event("stopped", Duration::from_millis(250))
            .await
            .expect_err("expected timeout");
        assert!(matches!(err, DapError::Timeout(_)), "got {err}");
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_event_errors_when_channel_closes() {
        // Adapter emits a non-matching event then exits, closing the channel.
        // wait_for_event must surface a protocol error, not hang.
        let (_dir, mut client) = spawn_fake(&["event:output"]);
        let err = bounded(client.wait_for_event("stopped", Duration::from_secs(5)))
            .await
            .expect_err("expected channel-closed error");
        match err {
            DapError::DapProtocolError { command, message } => {
                assert!(command.contains("stopped"), "got command {command}");
                assert!(message.contains("closed"), "got message {message}");
            }
            other => panic!("expected DapProtocolError, got {other}"),
        }
        client.kill().await.unwrap();
    }

    #[tokio::test]
    async fn kill_terminates_subprocess_and_aborts_reader() {
        // A long-lived adapter must be killable; afterwards the reader task is
        // aborted so its `events_tx` drops and the event channel closes.
        let (_dir, mut client) = spawn_fake(&["hang"]);
        client.kill().await.expect("kill should succeed");
        // Reader aborted -> events channel closed -> next_event yields None.
        let none = bounded(client.next_event()).await;
        assert!(none.is_none(), "event channel should be closed after kill");
    }
}
