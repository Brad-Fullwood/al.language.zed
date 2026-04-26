//! AL LSP Test Harness
//!
//! Spawns the `al-lsp` binary over stdio and speaks the LSP protocol,
//! providing a high-level API for end-to-end testing of every capability.
//!
//! # Transport Abstraction
//!
//! The harness supports two transports:
//! - **Stdio** (`LspClient::spawn`): spawns al-lsp as a child process
//! - **Socket** (`LspClient::connect`): connects to a running al-lsp daemon
//!
//! Both share the same JSON-RPC protocol and `LspClient` API.
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

/// Return the path to the bundled test AL project.
///
/// Used in every e2e test file — centralised here to avoid copy-paste drift.
pub fn test_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/test_al_project")
}

/// Return the path to an external AL test project from the environment, if set
/// and valid.
///
/// Checks `AL_TEST_PROJECT_PATH` and confirms it contains `app.json`.
/// Returns `None` (with a message to stderr) if the variable is absent or the
/// project root cannot be found.  Used by `zed_simulation`, `data_driven`, and
/// `performance` test files.
pub fn test_project_from_env() -> Option<PathBuf> {
    let path = std::env::var("AL_TEST_PROJECT_PATH")
        .ok()
        .map(PathBuf::from)?;
    if path.join("app.json").exists() {
        Some(path)
    } else {
        None
    }
}
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
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

/// Lifecycle management for the server connection.
///
/// Stdio mode owns a child process; daemon mode connects to an existing server.
enum Lifecycle {
    /// al-lsp spawned as a child process, communicating over stdio.
    Stdio(Child),
    /// Connected to al-lsp daemon over Unix socket.
    /// Full implementation in T303 when daemon mode exists.
    #[allow(dead_code)]
    Daemon,
}

/// Writer half of the transport — abstracted so stdio and socket share the same code path.
type Writer = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

/// An LSP client that communicates with al-lsp over stdio or Unix socket.
pub struct LspClient {
    writer: Option<Writer>,
    lifecycle: Lifecycle,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    notifications: mpsc::UnboundedReceiver<(String, Value)>,
    /// Notifications consumed by internal waits that should still be visible to tests.
    buffered_notifications: Vec<(String, Value)>,
    root_path: PathBuf,
    open_docs: HashMap<String, i32>, // uri -> version
}

impl LspClient {
    /// Spawn al-lsp as a child process and perform the initialize handshake.
    ///
    /// This is the standard entry point for tests and Zed integration.
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

        let stdin = child
            .stdin
            .take()
            .ok_or("al-lsp child stdin not available")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("al-lsp child stdout not available")?;

        let mut client = Self::from_transport(
            Box::new(stdin),
            BufReader::new(stdout),
            Lifecycle::Stdio(child),
            root_path,
        );

        client.initialize().await?;
        Ok(client)
    }

    /// Connect to a running al-lsp daemon over a Unix socket.
    ///
    /// Requires al-lsp to be running in daemon mode (see T303).
    /// The daemon handles project discovery from the `project_root`.
    #[allow(dead_code)]
    pub async fn connect(
        _socket_path: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let _root_path = project_root.as_ref().to_path_buf();
        // T303: Full implementation when daemon mode exists.
        // Will:
        // 1. Connect to Unix socket at socket_path
        // 2. Split into read/write halves
        // 3. Call Self::from_transport(writer, reader, Lifecycle::Daemon, root_path)
        // 4. Run initialize handshake
        unimplemented!(
            "Daemon transport not yet implemented. \
             Requires al-lsp daemon mode (T303)."
        )
    }

    /// Construct an LspClient from transport halves.
    ///
    /// This is the shared constructor used by both `spawn` and `connect`.
    /// The reader is consumed by a background task; the writer is stored
    /// for sending requests and notifications.
    fn from_transport(
        writer: Writer,
        reader: impl tokio::io::AsyncBufRead + Unpin + Send + 'static,
        lifecycle: Lifecycle,
        root_path: PathBuf,
    ) -> Self {
        let pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        // Spawn reader task — generic over the concrete reader type
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            read_loop(reader, pending_clone, notif_tx).await;
        });

        LspClient {
            writer: Some(writer),
            lifecycle,
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_rx,
            buffered_notifications: Vec::new(),
            root_path,
            open_docs: HashMap::new(),
        }
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
                "name": self.root_path.file_name().and_then(|n| n.to_str()).unwrap_or("project")
            }]
        });

        let result = self.request("initialize", params).await?;

        // Send initialized notification — triggers async workspace init
        self.notify("initialized", serde_json::json!({})).await?;

        // Wait for workspace initialization by polling workspace/symbol.
        // The server loads packages asynchronously; completions/hover won't work
        // until symbols are available. We probe until we get results or timeout.
        //
        // Timeout is configurable via `AL_TEST_INIT_TIMEOUT` (seconds). Defaults to
        // 60s — large AL projects with many .app dependencies can take >30s to index.
        let init_timeout_secs = std::env::var("AL_TEST_INIT_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        let deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(init_timeout_secs);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "Timed out after {init_timeout_secs}s waiting for workspace init \
                     (workspace/symbol returned empty); set AL_TEST_INIT_TIMEOUT to extend"
                )
                .into());
            }
            // Use empty query to check if any workspace symbols are loaded.
            // Track the id before the request so we can clean up the pending
            // entry if the request times out (avoids a pending-map leak).
            let probe_id = self.next_id.load(Ordering::SeqCst);
            let probe = self
                .request("workspace/symbol", serde_json::json!({ "query": "" }))
                .await;
            match probe {
                Ok(val) => {
                    if let Some(arr) = val.as_array() {
                        if !arr.is_empty() {
                            break; // Workspace has scanned files
                        }
                    }
                }
                Err(_) => {
                    // On timeout or error the oneshot sender is dropped but the
                    // pending map entry may still hold the id.  Remove it so the
                    // slot does not leak across iterations.
                    self.pending.lock().await.remove(&probe_id);
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

        if let Err(e) = self.notify("textDocument/didOpen", params).await {
            tracing::warn!("textDocument/didOpen notify failed: {e}");
        }
        self.wait_for_diagnostics(&uri, tokio::time::Duration::from_secs(5))
            .await;
    }

    /// Send a text change to an already-open file (simulates Zed keystroke).
    ///
    /// Uses `TextDocumentSyncKind::Full` — sends the complete new content,
    /// exactly as Zed does. Increments the document version and waits for
    /// `publishDiagnostics` to confirm the server processed the change.
    pub async fn change_file(&mut self, relative_path: &str, new_content: &str) {
        let uri = self.file_uri(relative_path);
        let version = self.open_docs.get(&uri).copied().unwrap_or(1) + 1;
        self.open_docs.insert(uri.clone(), version);

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "version": version
            },
            "contentChanges": [{
                "text": new_content
            }]
        });

        if let Err(e) = self.notify("textDocument/didChange", params).await {
            tracing::warn!("textDocument/didChange notify failed: {e}");
        }
        self.wait_for_diagnostics(&uri, tokio::time::Duration::from_secs(5))
            .await;
    }

    /// Wait for `textDocument/publishDiagnostics` for `uri`, up to `timeout`.
    ///
    /// Notifications consumed while waiting are buffered so tests can still
    /// inspect them via `drain_notifications`.
    async fn wait_for_diagnostics(&mut self, uri: &str, timeout: tokio::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.notifications.recv()).await {
                Ok(Some((method, params))) => {
                    let is_our_diag = method == "textDocument/publishDiagnostics"
                        && params.get("uri").and_then(|v| v.as_str()) == Some(uri);
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

    /// Send a text change WITHOUT waiting for diagnostics (simulates rapid typing).
    ///
    /// Use this for testing rapid-fire edits where you don't want to wait
    /// for the server to process each one.
    pub async fn change_file_no_wait(&mut self, relative_path: &str, new_content: &str) {
        let uri = self.file_uri(relative_path);
        let version = self.open_docs.get(&uri).copied().unwrap_or(1) + 1;
        self.open_docs.insert(uri.clone(), version);

        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "version": version
            },
            "contentChanges": [{
                "text": new_content
            }]
        });

        if let Err(e) = self.notify("textDocument/didChange", params).await {
            tracing::warn!("textDocument/didChange (no_wait) notify failed: {e}");
        }
    }

    /// Close a file (simulates Zed closing a tab).
    pub async fn close_file(&mut self, relative_path: &str) {
        let uri = self.file_uri(relative_path);
        self.open_docs.remove(&uri);

        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });

        if let Err(e) = self.notify("textDocument/didClose", params).await {
            tracing::warn!("textDocument/didClose notify failed: {e}");
        }
    }

    /// Send configuration change (simulates Zed settings update).
    pub async fn change_configuration(&mut self, settings: Value) {
        let params = serde_json::json!({
            "settings": settings
        });

        if let Err(e) = self
            .notify("workspace/didChangeConfiguration", params)
            .await
        {
            tracing::warn!("workspace/didChangeConfiguration notify failed: {e}");
        }
    }

    /// Prepare rename — check if a position is renamable and get the range.
    pub async fn prepare_rename(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self
            .request("textDocument/prepareRename", params)
            .await
            .ok()?;
        if result.is_null() {
            None
        } else {
            Some(result)
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
                if result.is_null() {
                    None
                } else {
                    Some(result)
                }
            }
            Err(e) => {
                tracing::warn!(uri = %uri, line, character, error = %e, "hover request failed");
                None
            }
        }
    }

    /// Get completions at a position.
    pub async fn completion(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Vec<Value> {
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
    pub async fn definition(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self.request("textDocument/definition", params).await.ok()?; // test helper: LSP errors are non-fatal
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    /// Find references.
    pub async fn references(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Vec<Value> {
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

        let result = self
            .request("textDocument/semanticTokens/full", params)
            .await
            .ok()?; // test helper: LSP errors are non-fatal
        if result.is_null() {
            None
        } else {
            Some(result)
        }
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
    pub async fn signature_help(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
    ) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self
            .request("textDocument/signatureHelp", params)
            .await
            .ok()?; // test helper: LSP errors are non-fatal
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    /// Get code actions.
    pub async fn code_actions(
        &mut self,
        relative_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Vec<Value> {
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
    pub async fn inlay_hints(
        &mut self,
        relative_path: &str,
        start_line: u32,
        end_line: u32,
    ) -> Vec<Value> {
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
    pub async fn rename(
        &mut self,
        relative_path: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name
        });

        let result = self.request("textDocument/rename", params).await.ok()?; // test helper: LSP errors are non-fatal
        if result.is_null() {
            None
        } else {
            Some(result)
        }
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
                let diags = params["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                result.insert(uri, diags);
            }
        }
        result
    }

    /// Shutdown the server gracefully.
    pub async fn shutdown(mut self) {
        let _ = self.request("shutdown", serde_json::json!(null)).await;
        let _ = self.notify("exit", serde_json::json!(null)).await;
        // Drop writer to signal EOF
        self.writer.take();

        match &mut self.lifecycle {
            Lifecycle::Stdio(child) => {
                // Wait with timeout to avoid hanging if the server doesn't exit
                let _ =
                    tokio::time::timeout(tokio::time::Duration::from_secs(3), child.wait()).await;
                // Kill if still running
                let _ = child.kill().await;
            }
            Lifecycle::Daemon => {
                // Socket close (writer drop above) is sufficient.
                // No child process to manage.
            }
        }
    }

    // -- Internal --

    /// Build a file:// URI from a relative path. Public for test assertions.
    pub fn file_uri(&self, relative_path: &str) -> String {
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

    async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let writer = self.writer.as_mut().ok_or("writer closed")?;
        send_message(writer, &msg).await?;

        let response = tokio::time::timeout(tokio::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| format!("timeout waiting for response to {method} (id={id})"))?
            .map_err(|_| "channel closed")?;

        if let Some(error) = response.get("error") {
            return Err(format!("LSP error: {}", error).into());
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let writer = self.writer.as_mut().ok_or("writer closed")?;
        send_message(writer, &msg).await?;
        Ok(())
    }
}

async fn send_message(
    writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    msg: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read JSON-RPC messages from the transport and dispatch them.
///
/// Generic over the reader type so both stdio (BufReader<ChildStdout>) and
/// socket (BufReader<OwnedReadHalf>) use the same code with zero dynamic dispatch.
///
/// Exposed as `pub` so `transport.rs` tests can drive it directly without a
/// copy-paste duplicate.
pub async fn read_loop(
    mut reader: impl tokio::io::AsyncBufRead + Unpin,
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    notif_tx: mpsc::UnboundedSender<(String, Value)>,
) {
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
                content_length = len_str.parse().ok(); // non-numeric Content-Length is skipped (handled below)
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
