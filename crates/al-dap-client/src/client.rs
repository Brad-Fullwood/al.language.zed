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

use crate::framing::{ensure_seq, read_dap_body, write_dap_frame};
use crate::protocol::{DapEvent, DapMessage, DapResponse};
use crate::{DapError, Result};

/// Low-level DAP client that manages a subprocess.
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
        let request = crate::protocol::DapRequest {
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
}
