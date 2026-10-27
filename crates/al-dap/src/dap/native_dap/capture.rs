//! Snapshot capture over the native DAP: `attach` with `"snapshot": "capture"`.
//!
//! The session records instead of debugging. `setBreakpoints` lines become
//! snappoints, `configurationDone` arms the capture on the server through the
//! shared [`snapshot_capture`](crate::dap::bc_debug::snapshot_capture) module, a poll task reports status changes as
//! Output events, and `disconnect` finishes the session and saves the zip.
//! No SignalR connection is opened, so stepping, stack and variables requests
//! are refused. Replay is a launch with `snapshotFileName`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use al_bc::snapshot::state::{SessionRecord, SessionStore, SnappointRecord};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::dap::bc_debug::snapshot_capture::{
    self as capture, FinishResult, StartRequest, VersionInfo,
};
use crate::dap::bc_debug::snapshot_debugger::guard_production;
use crate::dap::bc_debug::BcDebugConfig;
use crate::dap::Result;

use super::{make_event, write_dap, NativeDapState, ResolvedObject};

const THREAD_ID: i64 = 1;
const THREAD_NAME: &str = "Snapshot capture";
const RECORD_ONLY: &str =
    "A snapshot capture only records. Replay it with a launch that sets snapshotFileName.";
const FIXED_ONCE_ARMED: &str =
    "Snappoints are fixed once the capture is armed. Disconnect and attach again to change them.";

/// A `setBreakpoints` line resolved to the snappoint it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSnappoint {
    pub object_type: i32,
    pub object_id: i32,
    /// 0-based, as on the wire.
    pub line0: u16,
    /// The line is in a source from a package, so the environment's BaseApp
    /// version is compared with the package's.
    pub in_package_source: bool,
    pub package_version: Option<String>,
    pub alpackages_base_app: Option<String>,
}

/// Maps a source path and a 1-based line to a snappoint. `Err` carries the
/// message the client shows on the unverified breakpoint. Injected by the
/// caller (al-lsp) because it needs the workspace index.
pub type SnappointResolver =
    Arc<dyn Fn(&str, i64) -> std::result::Result<CaptureSnappoint, String> + Send + Sync>;

/// Values a test replaces. Production uses the defaults.
#[derive(Clone)]
pub(crate) struct CaptureSeams {
    /// Session store directory. `None` is the per-user default location.
    pub store_dir: Option<PathBuf>,
    /// Automation API base. `None` derives it from the configuration.
    pub automation_base: Option<String>,
    pub poll_interval: Duration,
}

impl Default for CaptureSeams {
    fn default() -> Self {
        CaptureSeams {
            store_dir: None,
            automation_base: None,
            poll_interval: Duration::from_secs(10),
        }
    }
}

struct Entry {
    point: SnappointRecord,
    resolved: CaptureSnappoint,
}

/// What `attach` fixed for the capture.
pub(super) struct Capture {
    config: BcDebugConfig,
    allow_production: bool,
    output_dir: Option<PathBuf>,
    client_type: String,
    verbosity: String,
    session_id: i64,
    user_id: Option<String>,
    /// Per source path, in request order. A later `setBreakpoints` for the
    /// same path replaces the set.
    sources: BTreeMap<String, Vec<Entry>>,
    armed: bool,
    pub(super) record: Option<Arc<Mutex<SessionRecord>>>,
    poll: Option<JoinHandle<()>>,
}

pub(crate) struct CaptureState {
    resolver: SnappointResolver,
    seams: CaptureSeams,
    pub(super) slot: Arc<Mutex<Option<Capture>>>,
}

impl CaptureState {
    pub fn new(resolver: SnappointResolver) -> Self {
        CaptureState {
            resolver,
            seams: CaptureSeams::default(),
            slot: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn with_seams(mut self, seams: CaptureSeams) -> Self {
        self.seams = seams;
        self
    }
}

impl Default for CaptureState {
    fn default() -> Self {
        CaptureState::new(Arc::new(|_, _| {
            Err("snapshot capture is not available in this adapter".to_string())
        }))
    }
}

fn store_for(seams: &CaptureSeams) -> std::result::Result<SessionStore, String> {
    match &seams.store_dir {
        Some(dir) => Ok(SessionStore::at(dir.clone())),
        None => SessionStore::default_location()
            .ok_or_else(|| "no local data directory for the snapshot session store".to_string()),
    }
}

fn http_client(config: &BcDebugConfig) -> std::result::Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(config.accept_invalid_certs)
        .build()
        .map_err(|e| e.to_string())
}

fn str_arg<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn size_text(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    R: Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    /// Route a request to the active capture. `Some(done)` when the capture
    /// answered, `None` when no capture is active or the request is not its own.
    pub(super) async fn dispatch_capture<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        command: &str,
        request_seq: i64,
        arguments: &serde_json::Value,
    ) -> Result<Option<bool>> {
        if self.capture.slot.lock().await.is_none() {
            return Ok(None);
        }
        match command {
            "setBreakpoints" => {
                self.capture_set_breakpoints(out, request_seq, command, arguments)
                    .await?
            }
            "configurationDone" => {
                self.capture_configuration_done(out, request_seq, command)
                    .await?
            }
            "threads" => {
                self.reply_body(
                    out,
                    request_seq,
                    command,
                    serde_json::json!({ "threads": [{ "id": THREAD_ID, "name": THREAD_NAME }] }),
                )
                .await?
            }
            "stackTrace" | "scopes" | "variables" | "evaluate" | "continue" | "next" | "stepIn"
            | "stepOut" | "pause" => {
                self.reply_failure(out, request_seq, command, RECORD_ONLY)
                    .await?
            }
            "disconnect" | "terminate" => {
                self.capture_disconnect(out, request_seq, command).await?;
                return Ok(Some(true));
            }
            _ => return Ok(None),
        }
        Ok(Some(false))
    }

    /// `attach` with `"snapshot": "capture"`. Nothing reaches the server yet:
    /// the token is read to fail early, and the attach call waits for
    /// `configurationDone` so every `setBreakpoints` is in.
    pub(super) async fn handle_capture_attach<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        if self
            .capture
            .slot
            .lock()
            .await
            .as_ref()
            .is_some_and(|c| c.armed)
        {
            return self
                .reply_failure(
                    out,
                    request_seq,
                    command,
                    "A snapshot capture is already running in this debug session.",
                )
                .await;
        }
        let config = BcDebugConfig::from_dap_args(arguments);
        if let Err(error) = config.validate_native() {
            return self.reply_failure(out, request_seq, command, error).await;
        }
        if let Err(error) = (self.authorize_target)(&config) {
            return self.reply_failure(out, request_seq, command, error).await;
        }
        let allow_production = arguments
            .get("allowProduction")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if let Err(error) = guard_production(&config, allow_production) {
            return self
                .reply_failure(out, request_seq, command, error.to_string())
                .await;
        }
        let parsed = (|| -> std::result::Result<_, String> {
            let break_on_next = str_arg(arguments, "breakOnNext").ok_or_else(|| {
                "breakOnNext is required: WebClient, WebServiceClient, Background or ClientService"
                    .to_string()
            })?;
            let (_, client_type) = capture::parse_client_type(break_on_next)?;
            let (_, verbosity) = capture::parse_verbosity(
                str_arg(arguments, "snapshotVerbosity").unwrap_or("SnapPoint"),
            )?;
            let session_id = match arguments.get("sessionId") {
                None | Some(serde_json::Value::Null) => -1,
                Some(v) => v
                    .as_i64()
                    .filter(|id| *id >= 0)
                    .ok_or("sessionId must be a non-negative integer")?,
            };
            let output_dir = str_arg(arguments, "snapshotOutputPath").map(PathBuf::from);
            capture::resolve_output_dir(Path::new(&self.project_root), output_dir.as_deref())
                .map_err(|e| e.to_string())?;
            Ok((client_type, verbosity, session_id, output_dir))
        })();
        let (client_type, verbosity, session_id, output_dir) = match parsed {
            Ok(v) => v,
            Err(error) => return self.reply_failure(out, request_seq, command, error).await,
        };

        self.emit_output(
            out,
            "console",
            format!("Authenticating to tenant {}...\r\n", config.tenant),
        )
        .await?;
        let token = match (self.acquire_token)(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Authentication failed: {e}"),
                    )
                    .await;
            }
        };
        let user_id = match str_arg(arguments, "userId") {
            None => None,
            Some(user) => {
                let http = match http_client(&config) {
                    Ok(h) => h,
                    Err(e) => return self.reply_failure(out, request_seq, command, e).await,
                };
                match capture::resolve_user(
                    &http,
                    &config,
                    &token,
                    self.capture.seams.automation_base.as_deref(),
                    user,
                )
                .await
                {
                    Ok(id) => Some(id),
                    Err(e) => {
                        return self
                            .reply_failure(out, request_seq, command, e.to_string())
                            .await
                    }
                }
            }
        };

        *self.debug_config.lock().await = Some(config.clone());
        let mut state = Capture {
            config,
            allow_production,
            output_dir,
            client_type: client_type.to_string(),
            verbosity: verbosity.to_string(),
            session_id,
            user_id,
            sources: BTreeMap::new(),
            armed: false,
            record: None,
            poll: None,
        };
        // Breakpoints the client sent before the attach wait in the shared
        // pending map. They go through the resolver now and each row is told
        // its real state with a `breakpoint` event.
        let pending = std::mem::take(&mut *self.pending_breakpoints.lock().await);
        let mut events = Vec::new();
        for (path, requests) in pending {
            let (entries, results) = self.resolve_lines(&path, &requests);
            for mut bp in results {
                bp["source"] = serde_json::json!({ "path": &path });
                events.push(make_event(
                    &self.seq,
                    "breakpoint",
                    Some(serde_json::json!({ "reason": "changed", "breakpoint": bp })),
                ));
            }
            state.sources.insert(path, entries);
        }
        *self.capture.slot.lock().await = Some(state);

        self.reply_ok(out, request_seq, command).await?;
        for event in &events {
            write_dap(out, event).await?;
        }
        self.emit_output(
            out,
            "console",
            "Snapshot capture: set breakpoints on the lines to record, then the capture arms when the configuration finishes.\r\n",
        )
        .await?;
        Ok(())
    }

    /// Resolve `(line, condition)` pairs to entries and the DAP breakpoint
    /// results, one per request.
    fn resolve_lines(
        &self,
        path: &str,
        requests: &[(i64, String)],
    ) -> (Vec<Entry>, Vec<serde_json::Value>) {
        let mut entries = Vec::new();
        let mut results = Vec::new();
        for (line, condition) in requests {
            match (self.capture.resolver)(path, *line) {
                Ok(resolved) => {
                    entries.push(Entry {
                        point: SnappointRecord {
                            object_type: resolved.object_type,
                            object_id: resolved.object_id,
                            line0: resolved.line0,
                            condition: (!condition.is_empty()).then(|| condition.clone()),
                        },
                        resolved,
                    });
                    results.push(serde_json::json!({ "verified": true, "line": line }));
                }
                Err(message) => results.push(serde_json::json!({
                    "verified": false, "line": line, "message": message,
                })),
            }
        }
        (entries, results)
    }

    async fn capture_set_breakpoints<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let path = arguments
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let requests: Vec<(i64, String)> = arguments
            .get("breakpoints")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|bp| {
                        (
                            bp.get("line")
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or(1),
                            bp.get("condition")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut guard = self.capture.slot.lock().await;
        let Some(state) = guard.as_mut() else {
            return Ok(());
        };
        let results = if state.armed {
            requests
                .iter()
                .map(|(line, _)| {
                    serde_json::json!({ "verified": false, "line": line, "message": FIXED_ONCE_ARMED })
                })
                .collect()
        } else {
            let (entries, results) = self.resolve_lines(&path, &requests);
            state.sources.insert(path, entries);
            results
        };
        drop(guard);
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({ "breakpoints": results }),
        )
        .await
    }

    async fn capture_configuration_done<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let mut guard = self.capture.slot.lock().await;
        let Some(state) = guard.as_mut() else {
            return Ok(());
        };
        if state.armed {
            drop(guard);
            return self.reply_ok(out, request_seq, command).await;
        }
        let config = state.config.clone();
        let points: Vec<SnappointRecord> = state
            .sources
            .values()
            .flatten()
            .map(|e| e.point.clone())
            .collect();
        let mut versions: Vec<VersionInfo> = Vec::new();
        for entry in state.sources.values().flatten() {
            if entry.resolved.in_package_source {
                let info = VersionInfo {
                    package_version: entry.resolved.package_version.clone(),
                    alpackages_base_app: entry.resolved.alpackages_base_app.clone(),
                };
                if !versions.iter().any(|v| {
                    v.package_version == info.package_version
                        && v.alpackages_base_app == info.alpackages_base_app
                }) {
                    versions.push(info);
                }
            }
        }
        let request = (
            state.client_type.clone(),
            state.verbosity.clone(),
            state.session_id,
            state.user_id.clone(),
            state.allow_production,
        );
        drop(guard);

        let token = match (self.acquire_token)(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Authentication failed: {e}"),
                    )
                    .await;
            }
        };
        let http = match http_client(&config) {
            Ok(h) => h,
            Err(e) => return self.reply_failure(out, request_seq, command, e).await,
        };
        let store = match store_for(&self.capture.seams) {
            Ok(s) => s,
            Err(e) => return self.reply_failure(out, request_seq, command, e).await,
        };
        let started = capture::start(
            &http,
            &token,
            &store,
            StartRequest {
                config: &config,
                configuration_name: None,
                project_root: Path::new(&self.project_root),
                client_type: &request.0,
                verbosity: &request.1,
                session_id: request.2,
                user_id: request.3,
                snappoints: points,
                allow_production: request.4,
            },
        )
        .await;
        let started = match started {
            Ok(s) => s,
            Err(e) => {
                self.emit_output(out, "stderr", format!("Snapshot attach failed: {e}\r\n"))
                    .await?;
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Snapshot attach failed: {e}"),
                    )
                    .await;
            }
        };
        let context = started.record.debugging_context.clone();
        let attach_kind = started.attach_kind;
        let save_error = started.save_error.clone();
        let record = Arc::new(Mutex::new(started.record));
        let poll = self.spawn_status_poll(record.clone(), config.clone(), request.4);
        if let Some(state) = self.capture.slot.lock().await.as_mut() {
            state.armed = true;
            state.record = Some(record);
            state.poll = Some(poll);
        }
        self.reply_ok(out, request_seq, command).await?;

        for info in &versions {
            for warning in capture::environment_version_warnings(
                &http,
                &config,
                &token,
                self.capture.seams.automation_base.as_deref(),
                info,
            )
            .await
            {
                self.emit_output(out, "console", format!("{warning}\r\n"))
                    .await?;
            }
        }
        if let Some(error) = save_error {
            self.emit_output(out, "stderr", format!("{error}\r\n"))
                .await?;
        }
        self.emit_output(
            out,
            "console",
            format!(
                "Snapshot capture armed. Debugging context {context}, attach kind {attach_kind}. \
                 Snappoints are fixed now. Disconnect to finish and save the snapshot, or run `al-explorer snapshot finish`.\r\n"
            ),
        )
        .await?;
        Ok(())
    }

    /// Poll the server and report status changes through the event channel
    /// the BC event forwarder also uses. A fresh token is read for each call.
    fn spawn_status_poll(
        &self,
        record: Arc<Mutex<SessionRecord>>,
        config: BcDebugConfig,
        allow_production: bool,
    ) -> JoinHandle<()> {
        let acquire = self.acquire_token.clone();
        let events = self.dap_event_tx.clone();
        let seq = self.seq.clone();
        let seams = self.capture.seams.clone();
        tokio::spawn(async move {
            let say = |category: &str, text: String| {
                let event = make_event(
                    &seq,
                    "output",
                    Some(serde_json::json!({ "category": category, "output": text })),
                );
                if let Ok(body) = serde_json::to_vec(&event) {
                    let _ = events.try_send(body);
                }
            };
            let Ok(http) = http_client(&config) else {
                return;
            };
            let mut last_status: Option<String> = None;
            let mut last_error: Option<String> = None;
            let mut warned = false;
            loop {
                tokio::time::sleep(seams.poll_interval).await;
                let token = match acquire(config.tenant.clone()).await {
                    Ok(t) => t,
                    Err(e) => {
                        let text = format!("Snapshot status: authentication failed: {e}\r\n");
                        if last_error.as_deref() != Some(&text) {
                            say("stderr", text.clone());
                            last_error = Some(text);
                        }
                        continue;
                    }
                };
                let Ok(store) = store_for(&seams) else {
                    return;
                };
                let outcome = {
                    let mut record = record.lock().await;
                    capture::status(
                        &http,
                        &token,
                        &store,
                        &config,
                        allow_production,
                        &mut record,
                    )
                    .await
                };
                match outcome {
                    Ok(o) => {
                        last_error = None;
                        if last_status.as_deref() != Some(&o.status) {
                            say("console", format!("Snapshot status: {}\r\n", o.status));
                            last_status = Some(o.status.clone());
                        }
                        if let Some(warning) = o.warning.filter(|_| !warned) {
                            warned = true;
                            say("important", format!("{warning}\r\n"));
                        }
                        if let Some(error) = o.save_error {
                            say("stderr", format!("{error}\r\n"));
                        }
                    }
                    Err(e) => {
                        let text = format!("Snapshot status failed: {e}\r\n");
                        if last_error.as_deref() != Some(&text) {
                            say("stderr", text.clone());
                            last_error = Some(text);
                        }
                    }
                }
            }
        })
    }

    async fn capture_disconnect<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
    ) -> Result<()> {
        let taken = self.capture.slot.lock().await.take();
        let Some(mut state) = taken else {
            self.reply_ok(out, request_seq, command).await?;
            return Ok(());
        };
        if let Some(poll) = state.poll.take() {
            poll.abort();
            let _ = poll.await;
        }
        if let Some(record) = state.record.take() {
            let mut record = record.lock().await;
            match self.finish_capture(&state, &mut record).await {
                Ok(text) => self.emit_output(out, "console", format!("{text}\r\n")).await?,
                Err(error) => {
                    self.emit_output(
                        out,
                        "stderr",
                        format!(
                            "Snapshot finish failed: {error}. The session record stays in the store, so `al-explorer snapshot finish` can retry.\r\n"
                        ),
                    )
                    .await?
                }
            }
        } else {
            self.emit_output(
                out,
                "console",
                "Snapshot capture ended before it armed. Nothing recorded.\r\n",
            )
            .await?;
        }
        self.reply_ok(out, request_seq, command).await?;
        write_dap(out, &make_event(&self.seq, "terminated", None)).await?;
        Ok(())
    }

    async fn finish_capture(
        &self,
        state: &Capture,
        record: &mut SessionRecord,
    ) -> std::result::Result<String, String> {
        let token = (self.acquire_token)(state.config.tenant.clone())
            .await
            .map_err(|e| format!("authentication failed: {e}"))?;
        let http = http_client(&state.config)?;
        let store = store_for(&self.capture.seams)?;
        let outcome = capture::finish(
            &http,
            &token,
            &store,
            &state.config,
            state.allow_production,
            record,
            state.output_dir.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;
        let mut text = match outcome.result {
            FinishResult::Empty => "Snapshot finished. Nothing recorded.".to_string(),
            FinishResult::Saved { zip, bytes, .. } => format!(
                "Snapshot saved to {} ({}). Replay it with a launch that sets snapshotFileName.",
                zip.display(),
                size_text(bytes)
            ),
        };
        if let Some(error) = outcome.save_error {
            text.push_str(&format!(" {error}"));
        }
        Ok(text)
    }
}
