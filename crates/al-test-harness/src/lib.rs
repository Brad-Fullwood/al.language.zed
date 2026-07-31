//! End-to-end test client for the AL language server.
//!
//! Spawns the `al-lsp` binary over stdio and speaks the LSP protocol,
//! providing a high-level API for end-to-end testing of every capability.
//!
//! # Transport
//!
//! Stdio only — `LspClient::spawn` launches al-lsp as a child process. The
//! daemon mode speaks a different line-delimited JSON-RPC protocol via
//! [`al_protocol::DaemonClient`], so the two transports cannot share a
//! single client. Tests that need to exercise the daemon should drive
//! `DaemonClient` directly.
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

use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, Mutex};

pub use protocol::*;

fn find_binary() -> PathBuf {
    // Explicit override wins. Coverage runs set this to the INSTRUMENTED binary
    // so the al-lsp subprocess contributes to coverage (see scripts/coverage.sh);
    // CI / Zed packaging can also pin an exact path here.
    if let Some(path) = std::env::var_os("AL_LSP_BIN") {
        return PathBuf::from(path);
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let binary_name = format!("al-lsp{}", std::env::consts::EXE_SUFFIX);
    let debug_bin = workspace_root.join("target/debug").join(&binary_name);
    if debug_bin.exists() {
        return debug_bin;
    }

    let release_bin = workspace_root.join("target/release").join(&binary_name);
    if release_bin.exists() {
        return release_bin;
    }

    PathBuf::from(binary_name)
}

/// Locate a built workspace binary by name — `target/debug` then
/// `target/release`, falling back to the bare name (PATH lookup). Used by the
/// e2e smoke tests that spawn `al-lsp mcp` and `al-explorer` as subprocesses.
pub fn workspace_binary(name: &str) -> PathBuf {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let binary_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    for profile in ["debug", "release"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join(&binary_name);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(binary_name)
}

/// Path to the built `al-lsp` binary (honors the `AL_LSP_BIN` override used by
/// coverage runs).
pub fn al_lsp_binary() -> PathBuf {
    find_binary()
}

/// Path to the built `al-explorer` CLI/TUI binary.
pub fn al_explorer_binary() -> PathBuf {
    workspace_binary("al-explorer")
}

/// Stop any daemon already serving `project_dir`, so the next CLI call starts a
/// fresh one from the current `target/debug/al-lsp`.
///
/// The daemon is a per-project singleton that outlives the test that started
/// it. Without this, a test whose result depends on how `al-lsp` was *built* —
/// for example the `semantic` feature — silently measures whichever binary an
/// earlier run happened to leave resident, so the same command passes or fails
/// depending on working-tree history. Returns whether the shutdown command
/// succeeded; "no daemon was running" is a success.
pub fn stop_project_daemon(project_dir: &Path) -> bool {
    std::process::Command::new(al_explorer_binary())
        .arg("daemon-shutdown")
        .current_dir(project_dir)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Stdio mode owns the language-server child process.
enum Lifecycle {
    Stdio(Child),
}

type Writer = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;

fn response_array(method: &str, result: Value) -> Vec<Value> {
    if result.is_null() {
        return Vec::new();
    }
    result
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{method} returned an invalid response: {result}"))
}

pub struct LspClient {
    writer: Option<Writer>,
    lifecycle: Lifecycle,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    notifications: mpsc::Receiver<(String, Value)>,
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

        let mut cmd = tokio::process::Command::new(&binary);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(&root_path);
        // Only force RUST_LOG=debug if the test author hasn't already set it.
        // Allows quiet CI runs via `RUST_LOG=warn cargo test -p al-test-harness`.
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "debug");
        }
        let mut child = cmd.spawn()?;

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
        // Bound notification buffering so a misbehaving server flooding
        // $/progress or window/logMessage cannot grow memory unbounded. 10k is
        // huge for a test session — well above the largest legitimate burst
        // we've seen. On overflow `read_loop` logs+drops the notification
        // rather than backpressuring (which would stall the reader and break
        // request/response routing).
        let (notif_tx, notif_rx) = mpsc::channel(10_000);

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
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
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

            // Auto-dismiss server-to-client requests that arrive during init.
            // The server blocks on window/showMessageRequest (e.g. "download packages?")
            // until it receives a response; reply with null so we don't deadlock.
            while let Ok((method, params)) = self.notifications.try_recv() {
                if let Some(req_id) = params.get("__server_req_id__") {
                    tracing::debug!(method = %method, "harness: auto-dismissing server request during init");
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": serde_json::Value::Null
                    });
                    let writer = self
                        .writer
                        .as_mut()
                        .ok_or("writer closed while replying to server initialization request")?;
                    send_message(writer, &response).await?;
                } else {
                    self.buffered_notifications.push((method, params));
                }
            }

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
    /// timeout (configurable via `AL_TEST_DIAG_TIMEOUT_MS`) so tests don't
    /// hang if the server never publishes. A missing acknowledgement fails the
    /// caller: allowing the test to continue would make every later assertion
    /// capable of observing the pre-open workspace snapshot and passing
    /// without exercising the requested document.
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

        self.notify("textDocument/didOpen", params)
            .await
            .unwrap_or_else(|error| {
                panic!("textDocument/didOpen notify failed for {uri}: {error}")
            });
        assert!(
            self.wait_for_diagnostics(&uri, version, diag_wait_timeout())
                .await,
            "server did not acknowledge textDocument/didOpen for {uri} version {version}"
        );
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

        self.notify("textDocument/didChange", params)
            .await
            .unwrap_or_else(|error| {
                panic!("textDocument/didChange notify failed for {uri}: {error}")
            });
        assert!(
            self.wait_for_diagnostics(&uri, version, diag_wait_timeout())
                .await,
            "server did not acknowledge textDocument/didChange for {uri} version {version}"
        );
    }

    /// Wait for `textDocument/publishDiagnostics` for `uri` and `version`, up to
    /// `timeout`.
    ///
    /// Returns `true` if the diagnostic arrived, `false` on timeout or channel
    /// close. Notifications consumed while waiting are buffered so tests can
    /// still inspect them via `drain_notifications`. Callers treat `false` as
    /// an assertion failure; it is never a successful test acknowledgement.
    async fn wait_for_diagnostics(
        &mut self,
        uri: &str,
        version: i32,
        timeout: tokio::time::Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.notifications.recv()).await {
                Ok(Some((method, params))) => {
                    let is_our_diag = method == "textDocument/publishDiagnostics"
                        && params.get("uri").and_then(|v| v.as_str()) == Some(uri)
                        && params.get("version").and_then(|v| v.as_i64()) == Some(version.into());
                    self.buffered_notifications.push((method, params));
                    if is_our_diag {
                        return true;
                    }
                }
                Ok(None) => {
                    tracing::error!(uri, "wait_for_diagnostics: notification channel closed before publishDiagnostics");
                    return false;
                }
                Err(_) => {
                    tracing::error!(
                        uri,
                        timeout_ms = timeout.as_millis(),
                        "wait_for_diagnostics: timed out — server did not publish diagnostics in time (set AL_TEST_DIAG_TIMEOUT_MS to extend)"
                    );
                    return false;
                }
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

        self.notify("textDocument/didChange", params)
            .await
            .unwrap_or_else(|error| {
                panic!("textDocument/didChange (no_wait) notify failed for {uri}: {error}")
            });
    }

    pub async fn close_file(&mut self, relative_path: &str) {
        let uri = self.file_uri(relative_path);
        self.open_docs.remove(&uri);

        let params = serde_json::json!({
            "textDocument": { "uri": uri }
        });

        self.notify("textDocument/didClose", params)
            .await
            .unwrap_or_else(|error| {
                panic!("textDocument/didClose notify failed for {uri}: {error}")
            });
    }

    pub async fn change_configuration(&mut self, settings: Value) {
        let params = serde_json::json!({
            "settings": settings
        });

        self.notify("workspace/didChangeConfiguration", params)
            .await
            .unwrap_or_else(|error| {
                panic!("workspace/didChangeConfiguration notify failed: {error}")
            });
    }

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
            .expect("prepareRename request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    pub async fn hover(&mut self, relative_path: &str, line: u32, character: u32) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        });

        let result = self
            .request("textDocument/hover", params)
            .await
            .expect("hover request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

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

        let result = self
            .request("textDocument/completion", params)
            .await
            .expect("completion request failed");
        if let Some(items) = result.get("items").and_then(Value::as_array) {
            items.clone()
        } else if let Some(items) = result.as_array() {
            items.clone()
        } else if result.is_null() {
            Vec::new()
        } else {
            panic!("invalid completion response: {result}");
        }
    }

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

        let result = self
            .request("textDocument/definition", params)
            .await
            .expect("definition request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

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

        let result = self
            .request("textDocument/references", params)
            .await
            .expect("references request failed");
        response_array("textDocument/references", result)
    }

    pub async fn document_symbols(&mut self, relative_path: &str) -> Vec<Value> {
        self.try_document_symbols(relative_path)
            .await
            .expect("documentSymbol request failed")
    }

    /// Fallible document-symbol request for protocol tests that intentionally
    /// exercise cancellation or error responses.
    pub async fn try_document_symbols(
        &mut self,
        relative_path: &str,
    ) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        let result = self.request("textDocument/documentSymbol", params).await?;
        Ok(response_array("textDocument/documentSymbol", result))
    }

    pub async fn semantic_tokens(&mut self, relative_path: &str) -> Option<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        let result = self
            .request("textDocument/semanticTokens/full", params)
            .await
            .expect("semanticTokens request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    pub async fn folding_ranges(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({ "textDocument": { "uri": uri } });

        let result = self
            .request("textDocument/foldingRange", params)
            .await
            .expect("foldingRange request failed");
        response_array("textDocument/foldingRange", result)
    }

    pub async fn format(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        });

        let result = self
            .request("textDocument/formatting", params)
            .await
            .expect("formatting request failed");
        response_array("textDocument/formatting", result)
    }

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
            .expect("signatureHelp request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    pub async fn code_lens(&mut self, relative_path: &str) -> Vec<Value> {
        let uri = self.file_uri(relative_path);
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
        });

        let result = self
            .request("textDocument/codeLens", params)
            .await
            .expect("codeLens request failed");
        response_array("textDocument/codeLens", result)
    }

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

        let result = self
            .request("textDocument/codeAction", params)
            .await
            .expect("codeAction request failed");
        response_array("textDocument/codeAction", result)
    }

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

        let result = self
            .request("textDocument/inlayHint", params)
            .await
            .expect("inlayHint request failed");
        response_array("textDocument/inlayHint", result)
    }

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

        let result = self
            .request("textDocument/rename", params)
            .await
            .expect("rename request failed");
        if result.is_null() {
            None
        } else {
            Some(result)
        }
    }

    pub async fn workspace_symbol(&mut self, query: &str) -> Vec<Value> {
        let params = serde_json::json!({ "query": query });

        let result = self
            .request("workspace/symbol", params)
            .await
            .expect("workspace/symbol request failed");
        response_array("workspace/symbol", result)
    }

    /// Includes notifications buffered by internal waits (e.g. `open_file`).
    pub fn drain_notifications(&mut self) -> Vec<(String, Value)> {
        let mut result = std::mem::take(&mut self.buffered_notifications);
        while let Ok(notif) = self.notifications.try_recv() {
            result.push(notif);
        }
        result
    }

    pub fn drain_diagnostics(&mut self) -> HashMap<String, Vec<Value>> {
        let mut result: HashMap<String, Vec<Value>> = HashMap::new();
        for (method, params) in self.drain_notifications() {
            if method == "textDocument/publishDiagnostics" {
                let uri = params["uri"]
                    .as_str()
                    .expect("publishDiagnostics missing uri")
                    .to_string();
                let diags = params["diagnostics"]
                    .as_array()
                    .cloned()
                    .expect("publishDiagnostics missing diagnostics array");
                result.insert(uri, diags);
            }
        }
        result
    }

    pub async fn shutdown(mut self) {
        let result = self
            .request_without_params("shutdown")
            .await
            .expect("LSP shutdown request failed");
        assert!(
            result.is_null(),
            "LSP shutdown response must be null, got {result}"
        );
        self.notify("exit", serde_json::json!(null))
            .await
            .expect("LSP exit notification failed");
        // Drop writer to signal EOF
        self.writer.take();

        let Lifecycle::Stdio(child) = &mut self.lifecycle;
        // A timeout is a failed lifecycle contract, not successful cleanup.
        let status =
            match tokio::time::timeout(tokio::time::Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => panic!("failed to wait for al-lsp shutdown: {error}"),
                Err(_) => {
                    let kill_error = child.kill().await.err();
                    panic!(
                        "al-lsp did not exit within 3 seconds after shutdown/exit{}",
                        kill_error
                            .map(|error| format!("; forced kill also failed: {error}"))
                            .unwrap_or_default()
                    );
                }
            };
        assert!(
            status.success(),
            "al-lsp exited unsuccessfully after shutdown: {status}"
        );
    }
}

/// Terminates the child when a test exits without calling `shutdown()`.
impl Drop for LspClient {
    fn drop(&mut self) {
        let Lifecycle::Stdio(child) = &mut self.lifecycle;
        if let Err(e) = child.start_kill() {
            tracing::warn!(error = %e, "LspClient::drop: start_kill failed; child may be a zombie");
        }
    }
}

impl LspClient {
    /// Build a file:// URI from a relative path. Public for test assertions.
    pub fn file_uri(&self, relative_path: &str) -> String {
        let full_path = self.root_path.join(relative_path);
        // Encode bytes that would otherwise mis-parse in a URI path, matching
        // tower-lsp Url's encoding closely enough for assertion equality. Path
        // separators ('/') and unreserved characters pass through; everything
        // else is percent-encoded.
        //
        let path_str = full_path
            .to_str()
            .expect("test harness paths must be valid UTF-8");
        let mut encoded = String::new();
        for b in path_str.bytes() {
            let unreserved = b.is_ascii_alphanumeric()
                || b == b'-'
                || b == b'_'
                || b == b'.'
                || b == b'~'
                || b == b'/';
            if unreserved {
                encoded.push(b as char);
            } else {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
        format!("file://{}", encoded)
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.request_with_optional_params(method, Some(params))
            .await
    }

    /// Send a JSON-RPC request whose contract has no `params` member.
    ///
    /// JSON-RPC distinguishes an omitted member from `"params": null`, and
    /// tower-lsp rejects the latter for parameterless LSP methods such as
    /// `shutdown`.
    async fn request_without_params(
        &mut self,
        method: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.request_with_optional_params(method, None).await
    }

    async fn request_with_optional_params(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let mut msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method
        });
        if let Some(params) = params {
            msg["params"] = params;
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // Remove the pending entry on every exit path, including write failure,
        // timeout, channel close, and LSP error.
        let pending_ref = self.pending.clone();
        let cleanup = scopeguard_remove(pending_ref, id);

        let writer = self.writer.as_mut().ok_or("writer closed")?;
        send_message(writer, &msg).await?;

        let response = tokio::time::timeout(request_timeout(), rx)
            .await
            .map_err(|_| {
                format!(
                    "timeout waiting for response to {method} (id={id}); \
                     set AL_TEST_REQUEST_TIMEOUT_MS to extend"
                )
            })?
            .map_err(|_| "channel closed")?;

        // Successful response — read_loop already removed the entry; the
        // cleanup guard's idempotent `.remove(&id)` then becomes a no-op.
        drop(cleanup);

        if let Some(error) = response.get("error") {
            return Err(format!("LSP error: {}", error).into());
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send the LSP `$/cancelRequest` notification with the given
    /// JSON-RPC request id. Used by cancellation tests; tower-lsp drops the
    /// pending future for the matching id (cancels_pending_requests in
    /// tower-lsp 0.20 service.rs).
    pub async fn cancel_request(&mut self, id: i64) -> Result<(), Box<dyn std::error::Error>> {
        self.notify("$/cancelRequest", serde_json::json!({ "id": id }))
            .await
    }

    /// Peek the next request ID that `request()` would assign,
    /// without incrementing. Tests that want to cancel an in-flight
    /// request need to know its id ahead of time.
    pub fn peek_next_request_id(&self) -> i64 {
        self.next_id.load(Ordering::SeqCst)
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
    notif_tx: mpsc::Sender<(String, Value)>,
) {
    let mut header_buf = String::new();

    loop {
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

        // Guard against pathological / malicious headers — a corrupt
        // Content-Length: 4294967295 would otherwise allocate ~4 GiB
        // before we ever look at the bytes. 64 MiB is well above any
        // legitimate LSP message we have ever observed (largest real
        // payloads are document-symbol responses on huge files, ~5 MiB).
        const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
        if content_length > MAX_BODY_BYTES {
            eprintln!(
                "al-test-harness read_loop: Content-Length {content_length} exceeds {MAX_BODY_BYTES} cap, dropping connection"
            );
            return;
        }

        let mut body = vec![0u8; content_length];
        match tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body).await {
            Ok(_) => {}
            Err(_) => return,
        }

        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Dispatch: server request (id+method), response (id only), notification (method only)
        if msg.get("method").is_some() && msg.get("id").is_some() {
            // Server-to-client request (e.g. window/showMessageRequest).
            // Embed the request id into params so the receiver can send a reply.
            let method = msg["method"].as_str().unwrap_or("").to_string();
            let id_value = msg["id"].clone();
            let base_params = msg.get("params").cloned().unwrap_or(Value::Null);
            let params_with_id = match base_params {
                Value::Object(mut map) => {
                    map.insert("__server_req_id__".to_string(), id_value);
                    Value::Object(map)
                }
                _ => serde_json::json!({ "__server_req_id__": id_value }),
            };
            // try_send so a slow consumer can't backpressure the reader.
            if let Err(e) = notif_tx.try_send((method, params_with_id)) {
                tracing::warn!(error = ?e, "harness: dropping server-request notification (channel full)");
            }
        } else if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            // Response to a client request (has id, no method)
            let mut pending = pending.lock().await;
            if let Some(tx) = pending.remove(&id) {
                let _ = tx.send(msg);
            }
        } else if msg.get("id").is_some() && msg.get("method").is_none() {
            // A response carries a non-numeric id (LSP allows string
            // ids per JSON-RPC 2.0 §5). The harness only ever issues numeric
            // ids so a string id here means the server echoed one we didn't
            // send — which is a server bug. Log loudly so a future server
            // change that accidentally rewrites ids surfaces immediately.
            tracing::warn!(
                id = ?msg.get("id"),
                "harness: dropped response with non-numeric id — server returned an id we never issued"
            );
        } else if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
            // Notification from server (has method, no id)
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            if let Err(e) = notif_tx.try_send((method.to_string(), params)) {
                tracing::warn!(error = ?e, method, "harness: dropping server notification (channel full)");
            }
        }
    }
}

/// Per-request timeout, configurable via `AL_TEST_REQUEST_TIMEOUT_MS`.
///
/// Defaults to 10 s.
fn request_timeout() -> tokio::time::Duration {
    let ms = std::env::var("AL_TEST_REQUEST_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000);
    tokio::time::Duration::from_millis(ms)
}

/// Diagnostic-publish wait, configurable via `AL_TEST_DIAG_TIMEOUT_MS`.
/// Defaults to 5 s.
fn diag_wait_timeout() -> tokio::time::Duration {
    let ms = std::env::var("AL_TEST_DIAG_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5_000);
    tokio::time::Duration::from_millis(ms)
}

/// Returns a guard whose `Drop` removes the pending-request
/// entry from the shared map, so that EVERY exit path — write failure,
/// timeout, channel close, LSP error, panic in the calling test — drains
/// the map. Only the happy path triggers a no-op (the read_loop already
/// removed the entry by the time the guard fires; `pending.remove(&id)`
/// for an absent id is fine).
fn scopeguard_remove(
    pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
    id: i64,
) -> impl Drop {
    struct Guard {
        pending: Arc<Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Value>>>>,
        id: i64,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // `Drop` cannot await a Tokio mutex. Remove synchronously when
            // uncontended; otherwise hand cleanup to the active runtime.
            if let Ok(mut guard) = self.pending.try_lock() {
                guard.remove(&self.id);
                return;
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let pending = self.pending.clone();
                let id = self.id;
                handle.spawn(async move {
                    pending.lock().await.remove(&id);
                });
            }
        }
    }
    Guard { pending, id }
}
