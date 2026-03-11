//! AL LSP Test Harness
//!
//! Spawns the `al-lsp` binary over stdio and speaks the LSP protocol,
//! providing a high-level API for end-to-end testing of every capability.
//!
//! # Usage
//! ```no_run
//! use al_test_harness::LspClient;
//!
//! #[tokio::test]
//! async fn test_hover() {
//!     let mut client = LspClient::spawn("path/to/project").await.unwrap();
//!     client.open_file("src/test.al", "codeunit 50100 Test { }").await;
//!     let hover = client.hover("src/test.al", 0, 0).await;
//!     assert!(hover.is_some());
//!     client.shutdown().await;
//! }
//! ```

mod protocol;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex};

pub use protocol::*;

/// Find the al-lsp binary, checking debug build first.
fn find_binary() -> PathBuf {
    // Check cargo target directory
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let debug_bin = workspace_root.join("target/debug/al-lsp");
    if debug_bin.exists() {
        return debug_bin;
    }

    let release_bin = workspace_root.join("target/release/al-lsp");
    if release_bin.exists() {
        return release_bin;
    }

    // Fall back to PATH
    PathBuf::from("al-lsp")
}

/// An LSP client that communicates with al-lsp over stdio.
pub struct LspClient {
    stdin: Option<ChildStdin>,
    child: Child,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    notifications: mpsc::UnboundedReceiver<(String, Value)>,
    /// Notifications consumed by internal waits that should still be visible to tests.
    buffered_notifications: Vec<(String, Value)>,
    root_path: PathBuf,
    open_docs: HashMap<String, i32>, // uri -> version
}

impl LspClient {
    /// Spawn al-lsp and perform the initialize handshake.
    pub async fn spawn(project_root: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let binary = find_binary();
        let root_path = project_root.as_ref().to_path_buf();

        tracing::info!(binary = %binary.display(), root = %root_path.display(), "spawning al-lsp");

        let mut child = tokio::process::Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(&root_path)
            .env("RUST_LOG", "debug")
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        // Spawn reader task
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            read_loop(stdout, pending_clone, notif_tx).await;
        });

        let mut client = LspClient {
            stdin: Some(stdin),
            child,
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_rx,
            buffered_notifications: Vec::new(),
            root_path,
            open_docs: HashMap::new(),
        };

        // Send initialize
        client.initialize().await?;

        Ok(client)
    }

    /// Send initialize request and initialized notification.
    async fn initialize(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        let root_uri = format!("file://{}", self.root_path.display());
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "completion": { "completionItem": { "snippetSupport": false } },
                    "definition": {},
                    "references": {},
                    "documentSymbol": {},
                    "formatting": {},
                    "semanticTokens": {
                        "requests": { "full": true },
                        "tokenTypes": [
                            "keyword", "type", "string", "number", "comment",
                            "operator", "property", "variable", "function",
                            "parameter", "enumMember", "namespace"
                        ],
                        "tokenModifiers": [],
                        "formats": ["relative"]
                    },
                    "rename": { "prepareSupport": true },
                    "codeAction": {},
                    "signatureHelp": {},
                    "inlayHint": {},
                    "foldingRange": {}
                },
                "workspace": {
                    "workspaceFolders": true,
                    "symbol": {}
                }
            },
            "workspaceFolders": [{
                "uri": root_uri,
                "name": self.root_path.file_name().unwrap().to_str().unwrap()
            }]
        });

        let result = self.request("initialize", params).await?;

        // Send initialized notification — triggers async workspace init
        self.notify("initialized", serde_json::json!({})).await?;

        // Wait for workspace initialization by polling workspace/symbol.
        // The server loads packages asynchronously; completions/hover won't work
        // until symbols are available. We probe until we get results or timeout.
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("Timed out waiting for workspace init");
                break;
            }
            // Use empty query to check if any workspace symbols are loaded
            let probe = self.request("workspace/symbol", serde_json::json!({ "query": "" })).await;
            if let Ok(val) = probe {
                if let Some(arr) = val.as_array() {
                    if !arr.is_empty() {
                        break; // Workspace has scanned files
                    }
                }
            }
        }

        Ok(result)
    }

    /// Open a file in the server and wait until the server has processed it.
    ///
    /// Waits for `textDocument/publishDiagnostics` for the opened URI, which
    /// signals that parsing + linting are complete. Falls back to a 5-second
    /// timeout so tests don't hang if the server never publishes.
    pub async fn open_file(&mut self, relative_path: &str, content: &str) {
        let uri = self.file_uri(relative_path);
        let version = 1;
        self.open_docs.insert(uri.clone(), version);

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": "al",
                "version": version,
                "text": content
            }
        });

        self.notify("textDocument/didOpen", params).await.unwrap();

        // Wait for publishDiagnostics for this URI (signals server has processed the file)
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, self.notifications.recv()).await {
                Ok(Some((method, params))) => {
                    let is_our_diag = method == "textDocument/publishDiagnostics"
                        && params.get("uri").and_then(|v| v.as_str()) == Some(uri.as_str());
                    // Buffer the notification so tests can still see it
                    self.buffered_notifications.push((method, params));
                    if is_our_diag {
                        break;
                    }
                }
                Ok(None) => break, // Channel closed
                Err(_) => break,   // Timeout
            }
        }
    }

    /// Get hover info at a position.
    pub async fn hover(&mut self, relative_path: &str, line: u32, character: u32) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        match self.request("textDocument/hover", params).await {
            Ok(result) => {
                if result.is_null() { None } else { Some(result) }
            }
            Err(e) => {
                tracing::warn!(uri = %uri, line, character, error = %e, "hover request failed");
                None
            }
        }
    }

    /// Get completions at a position.
    pub async fn completion(&mut self, relative_path: &str, line: u32, character: u32) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        match self.request("textDocument/completion", params).await {
            Ok(result) => {
                if let Some(items) = result.get("items").and_then(|v| v.as_array()) {
                    items.clone()
                } else if let Some(arr) = result.as_array() {
                    arr.clone()
                } else {
                    vec![]
                }
            }
            Err(_) => vec![],
        }
    }

    /// Go to definition.
    pub async fn definition(&mut self, relative_path: &str, line: u32, character: u32) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self.request("textDocument/definition", params).await.ok()?;
        if result.is_null() { None } else { Some(result) }
    }

    /// Find references.
    pub async fn references(&mut self, relative_path: &str, line: u32, character: u32) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true }
        });

        match self.request("textDocument/references", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Get document symbols.
    pub async fn document_symbols(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        match self.request("textDocument/documentSymbol", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Get semantic tokens.
    pub async fn semantic_tokens(&mut self, relative_path: &str) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        let result = self.request("textDocument/semanticTokens/full", params).await.ok()?;
        if result.is_null() { None } else { Some(result) }
    }

    /// Get folding ranges.
    pub async fn folding_ranges(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        match self.request("textDocument/foldingRange", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Format document.
    pub async fn format(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        });

        match self.request("textDocument/formatting", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Get signature help.
    pub async fn signature_help(&mut self, relative_path: &str, line: u32, character: u32) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self.request("textDocument/signatureHelp", params).await.ok()?;
        if result.is_null() { None } else { Some(result) }
    }

    /// Get code actions.
    pub async fn code_actions(&mut self, relative_path: &str, start_line: u32, end_line: u32) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": end_line, "character": 0 }
            },
            "context": { "diagnostics": [] }
        });

        match self.request("textDocument/codeAction", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Get inlay hints.
    pub async fn inlay_hints(&mut self, relative_path: &str, start_line: u32, end_line: u32) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": end_line, "character": 0 }
            }
        });

        match self.request("textDocument/inlayHint", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Rename symbol.
    pub async fn rename(&mut self, relative_path: &str, line: u32, character: u32, new_name: &str) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name
        });

        let result = self.request("textDocument/rename", params).await.ok()?;
        if result.is_null() { None } else { Some(result) }
    }

    /// Workspace symbol search.
    pub async fn workspace_symbol(&mut self, query: &str) -> Vec<Value> {
        let params = serde_json::json!({ "query": query });

        match self.request("workspace/symbol", params).await {
            Ok(result) => result.as_array().cloned().unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    /// Drain all pending notifications. Returns (method, params) pairs.
    /// Includes notifications buffered by internal waits (e.g. `open_file`).
    pub fn drain_notifications(&mut self) -> Vec<(String, Value)> {
        let mut result = std::mem::take(&mut self.buffered_notifications);
        while let Ok(notif) = self.notifications.try_recv() {
            result.push(notif);
        }
        result
    }

    /// Get all published diagnostics (drains notification queue).
    pub fn drain_diagnostics(&mut self) -> HashMap<String, Vec<Value>> {
        let mut result: HashMap<String, Vec<Value>> = HashMap::new();
        for (method, params) in self.drain_notifications() {
            if method == "textDocument/publishDiagnostics" {
                let uri = params["uri"].as_str().unwrap_or("").to_string();
                let diags = params["diagnostics"].as_array().cloned().unwrap_or_default();
                result.insert(uri, diags);
            }
        }
        result
    }

    /// Shutdown the server gracefully.
    pub async fn shutdown(mut self) {
        let _ = self.request("shutdown", serde_json::json!(null)).await;
        let _ = self.notify("exit", serde_json::json!(null)).await;
        // Drop stdin to signal EOF to the server and reader task
        self.stdin.take();
        // Wait with timeout to avoid hanging if the server doesn't exit
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(3),
            self.child.wait(),
        )
        .await;
        // Kill if still running
        let _ = self.child.kill().await;
    }

    // -- Internal --

    fn file_uri(&self, relative_path: &str) -> String {
        let full_path = self.root_path.join(relative_path);
        // Use percent-encoding for path components to match how tower-lsp's
        // Url type encodes URIs (e.g., spaces become %20).
        let encoded: String = full_path
            .to_str()
            .unwrap_or("")
            .bytes()
            .flat_map(|b| {
                if b == b' ' {
                    vec![b'%', b'2', b'0']
                } else {
                    vec![b]
                }
            })
            .map(|b| b as char)
            .collect();
        format!("file://{}", encoded)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        send_message(stdin, &msg).await?;

        let response = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            rx,
        )
        .await
        .map_err(|_| format!("timeout waiting for response to {method} (id={id})"))?
        .map_err(|_| "channel closed")?;

        if let Some(error) = response.get("error") {
            return Err(format!("LSP error: {}", error).into());
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        send_message(stdin, &msg).await?;
        Ok(())
    }
}

async fn send_message(stdin: &mut ChildStdin, msg: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(body.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    notif_tx: mpsc::UnboundedSender<(String, Value)>,
) {
    let mut reader = BufReader::new(stdout);
    let mut header_buf = String::new();

    loop {
        // Read headers
        let mut content_length: Option<usize> = None;
        loop {
            header_buf.clear();
            match reader.read_line(&mut header_buf).await {
                Ok(0) => return, // EOF
                Ok(_) => {}
                Err(_) => return,
            }

            let line = header_buf.trim();
            if line.is_empty() {
                break;
            }

            if let Some(len_str) = line.strip_prefix("Content-Length: ") {
                content_length = len_str.parse().ok();
            }
        }

        let content_length = match content_length {
            Some(len) => len,
            None => continue,
        };

        // Read body
        let mut body = vec![0u8; content_length];
        match tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body).await {
            Ok(_) => {}
            Err(_) => return,
        }

        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Dispatch: response (has id) or notification (no id)
        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            // Response to a request
            let mut pending = pending.lock().await;
            if let Some(tx) = pending.remove(&id) {
                let _ = tx.send(msg);
            }
        } else if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
            // Notification from server
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let _ = notif_tx.send((method.to_string(), params));
        }
    }
}
