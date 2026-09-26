//! Native DAP server for AL debugging.
//!
//! Speaks DAP over stdio to Zed, and translates to BC REST + SignalR internally.
//! Replaces the EditorServices.Host proxy entirely.
//!
//! Flow:
//! 1. Zed sends DAP initialize → we respond with capabilities
//! 2. Zed sends launch/attach → we compile, publish, connect SignalR
//! 3. Zed sends setBreakpoints → we call AddBreakpointAsync on SignalR
//! 4. BC sends events (stopped, etc.) → we translate to DAP events
//! 5. Zed sends continue/step → we call ContinueAsync on SignalR
//! 6. Zed sends variables/evaluate → we call GetVariablesAsync/GetWatchNodeAsync
//!
//! The request handlers are grouped by what they act on: [`session`] for
//! initialize, launch, attach and disconnect, [`breakpoints`], [`execution`]
//! for stepping and continuing, [`stack`], [`variables`], [`events`] for the
//! BC push-event forwarder, and [`browser`] for the debug-session URL. This
//! file holds the state those share, the DAP message builders and the stdio
//! loop.

pub mod breakpoints;
pub mod browser;
pub mod events;
pub mod execution;
pub mod requests;
pub mod session;
pub mod stack;
pub mod variables;

#[cfg(test)]
mod test_support;

pub use browser::build_debug_browser_url;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{self, BufReader};
use tokio::sync::{watch, Mutex};
use tracing::{debug, error, info, warn};

use crate::dap::bc_debug::{BcDebugConfig, BcDebugSession};
use crate::dap::framing::{read_dap_body, write_dap_frame};
use crate::dap::Result;

/// BC `ObjectTypeWrapper` enum values (integer encoding used by SignalR hub).
///
/// Source: EditorServices.Protocol.dll reverse-engineering.
/// Newtonsoft.Json defaults to integer enum serialization, so these are
/// transmitted as integers in SignalR JSON messages.
pub mod bc_object_type {
    pub const TABLE: i32 = 1;
    pub const REPORT: i32 = 3;
    pub const CODEUNIT: i32 = 5;
    pub const XMLPORT: i32 = 6;
    pub const PAGE: i32 = 8;
    pub const QUERY: i32 = 9;
    pub const PAGE_EXTENSION: i32 = 14;
    pub const TABLE_EXTENSION: i32 = 15;
    pub const ENUM: i32 = 16;
    pub const ENUM_EXTENSION: i32 = 17;
    pub const REPORT_EXTENSION: i32 = 22;
    pub const UNKNOWN: i32 = -1;
}

/// Map an AL object kind string (from the file index) to a BC `ObjectTypeWrapper` integer.
///
/// The kind strings come from the workspace file index (lowercased AL object type names).
/// Returns `bc_object_type::UNKNOWN` for unrecognised kinds.
pub fn kind_to_object_type(kind: &str) -> i32 {
    match kind.to_lowercase().as_str() {
        "table" => bc_object_type::TABLE,
        "report" => bc_object_type::REPORT,
        "codeunit" => bc_object_type::CODEUNIT,
        "xmlport" => bc_object_type::XMLPORT,
        "page" => bc_object_type::PAGE,
        "query" => bc_object_type::QUERY,
        "pageextension" => bc_object_type::PAGE_EXTENSION,
        "tableextension" => bc_object_type::TABLE_EXTENSION,
        "enum" => bc_object_type::ENUM,
        "enumextension" => bc_object_type::ENUM_EXTENSION,
        "reportextension" => bc_object_type::REPORT_EXTENSION,
        _ => bc_object_type::UNKNOWN,
    }
}

/// Object info resolved from the workspace symbol index.
/// Used to map file paths to BC object types and IDs for breakpoints.
#[derive(Debug, Clone)]
pub struct ResolvedObject {
    /// BC ObjectTypeWrapper value — use `bc_object_type` constants.
    pub object_type: i32,
    pub object_id: i32,
}

const VARIABLE_HANDLE_BASE: i64 = 1 << 18;
const MAX_VARIABLE_HANDLES: usize = 65_536;
const SCOPE_GLOBALS: i64 = 1;
const SCOPE_LOCALS: i64 = 2;

#[derive(Debug, Clone)]
struct VariableHandle {
    frame_id: i64,
    parent_path: String,
    /// BC sometimes supplies children inline and sometimes requires an
    /// `ExpandNode` request. `None` retains that distinction for lazy loading.
    nodes: Option<Vec<serde_json::Value>>,
}

#[derive(Debug)]
struct VariableHandleStore {
    next: i64,
    handles: HashMap<i64, VariableHandle>,
}

impl Default for VariableHandleStore {
    fn default() -> Self {
        Self {
            next: VARIABLE_HANDLE_BASE,
            handles: HashMap::new(),
        }
    }
}

impl VariableHandleStore {
    fn create(&mut self, handle: VariableHandle) -> Option<i64> {
        if self.handles.len() >= MAX_VARIABLE_HANDLES {
            return None;
        }
        let reference = self.next;
        self.next = self.next.checked_add(1)?;
        self.handles.insert(reference, handle);
        Some(reference)
    }

    fn get(&self, reference: i64) -> Option<VariableHandle> {
        self.handles.get(&reference).cloned()
    }

    fn reset(&mut self) {
        self.next = VARIABLE_HANDLE_BASE;
        self.handles.clear();
    }
}

fn scope_reference(group: i64, frame_id: i64) -> Option<i64> {
    let frame_id = u16::try_from(frame_id).ok()?;
    Some((group << 16) | i64::from(frame_id))
}

fn decode_scope_reference(reference: i64) -> Option<(i64, i64)> {
    if !(0..VARIABLE_HANDLE_BASE).contains(&reference) {
        return None;
    }
    let group = reference >> 16;
    if !matches!(group, SCOPE_GLOBALS | SCOPE_LOCALS) {
        return None;
    }
    Some((group, reference & 0xffff))
}

/// A parsed `setBreakpoints` request entry: `(line, condition)`. `condition`
/// is `""` when the client sent none.
type BpRequest = (i64, String);

/// `setBreakpoints` requests queued per source path while no debug session
/// exists yet.
type PendingBreakpoints = HashMap<String, Vec<BpRequest>>;

/// Decides whether a launch or attach may spend the user's Business Central
/// credential on the server its configuration names. `Err` carries the
/// refusal the client is shown.
///
/// The configuration is the debug scenario the editor read, which a cloned
/// repository can supply in `.zed/debug.json`. al-lsp passes
/// `al_project::trust::authorize_cached_credential` in here, the same rule the
/// daemon's `debug` method applies.
pub type TargetAuthorizer =
    Arc<dyn Fn(&BcDebugConfig) -> std::result::Result<(), String> + Send + Sync>;

/// Shared state and host callbacks for the native DAP server.
pub(crate) struct NativeDapState<F, R, P, C, A> {
    /// Single monotonic sequence counter shared between handlers and the
    /// background event-forwarding task. DAP requires non-decreasing seq
    /// values across all messages sent to the client.
    seq: Arc<AtomicU64>,
    session: Arc<Mutex<Option<Arc<BcDebugSession>>>>,
    debug_config: Arc<Mutex<Option<BcDebugConfig>>>,
    breakpoints: Arc<Mutex<HashMap<String, Vec<i64>>>>,
    /// `setBreakpoints` requests received before a debug session exists
    /// (i.e. during DAP configuration, right after `initialized`). DAP
    /// clients send these before `launch`/`attach` completes, so they can't
    /// be resolved against BC yet. Keyed by source path, replaced wholesale
    /// on each `setBreakpoints` call for that path (mirrors BC's own
    /// full-replace semantics). Drained and applied once a session starts
    /// (`apply_pending_breakpoints`), which also emits `breakpoint` events
    /// updating verification for the client's initially-unverified rows.
    pending_breakpoints: Arc<Mutex<PendingBreakpoints>>,
    /// True once `DebugAdapterConfigurationDone` has been accepted by BC for
    /// the current session. Current BC online rejects it until
    /// `OnAttachedToConnection` fires, so a rejected first attempt must be
    /// retried by the background event-forwarding task rather than left
    /// permanently unconfigured.
    configured: Arc<Mutex<bool>>,
    variable_handles: Arc<Mutex<VariableHandleStore>>,
    /// Cancellation channel for the background event-forwarding task.
    /// When a new debug session starts we send a new value so the old task exits.
    cancel_tx: watch::Sender<u64>,
    cancel_rx: watch::Receiver<u64>,
    /// Channel for the BC-event forwarding task to send pre-serialized DAP
    /// event bytes to the main loop (bounded to 1024 messages).
    dap_event_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    project_root: String,
    /// Runs before any compile, token or request on `launch`/`attach`.
    authorize_target: TargetAuthorizer,
    acquire_token: F,
    resolve_object: R,
    resolve_path: P,
    /// Compile the project; `Ok(build log)` on success, `Err(log)` on failure.
    /// Injected by the caller (al-lsp) so this crate never names the build /
    /// emit pipeline.
    compile: C,
    /// Locate the deploy `.app` for a project root. Injected for the same
    /// reason as `compile`.
    find_app: A,
}

/// Run the native DAP server on stdio.
///
/// `authorize_target` decides whether the launch configuration's server may
/// receive the user's credential, before anything is compiled or sent.
/// `acquire_token` is a callback to get an OAuth access token for the given tenant.
/// `resolve_object` maps a file path to its AL object type + ID using the workspace index.
/// `resolve_path` is the reverse: given a BC (ObjectType, ObjectNumber) returns the source file.
/// Both are provided by the caller (al-lsp binary) since they depend on `crate::symbols`.
pub async fn run_native_dap<F, Fut, R, P, C, CompileFut, A>(
    project_root: &str,
    authorize_target: TargetAuthorizer,
    acquire_token: F,
    resolve_object: R,
    resolve_path: P,
    compile: C,
    find_app: A,
) -> Result<()>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    let (cancel_tx, cancel_rx) = watch::channel(0u64);
    let (dap_event_tx, mut dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);

    let state = NativeDapState {
        seq: Arc::new(AtomicU64::new(1)),
        session: Arc::new(Mutex::new(None)),
        debug_config: Arc::new(Mutex::new(None)),
        breakpoints: Arc::new(Mutex::new(HashMap::new())),
        pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
        configured: Arc::new(Mutex::new(false)),
        variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
        cancel_tx,
        cancel_rx,
        dap_event_tx,
        project_root: project_root.to_string(),
        authorize_target,
        acquire_token,
        resolve_object,
        resolve_path,
        compile,
        find_app,
    };

    let mut stdout = io::stdout();

    // Read stdin in a dedicated task that forwards each parsed DAP request
    // body to a channel. The main loop then `select!`s over client requests and
    // BC push events — both channel `recv()`s, which are cancel-safe — so a
    // `stopped`/`output` event queued while the adapter is parked (e.g. after a
    // `continue`, when the client sends nothing and waits for `stopped`) is
    // written to stdout immediately instead of stalling until the next client
    // request. Selecting on `read_dap_body` directly would risk cancelling a
    // partial read and desyncing the framing, so the blocking read lives in its
    // own never-cancelled task.
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    tokio::spawn(async move {
        let mut stdin = BufReader::new(io::stdin());
        loop {
            match read_dap_body(&mut stdin).await {
                Ok(body) => {
                    if req_tx.send(body).await.is_err() {
                        break; // main loop gone
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    error!("Read error: {e}");
                    break;
                }
            }
        }
    });

    loop {
        let body = tokio::select! {
            // A BC push event queued by the forwarder — write it right away.
            Some(frame) = dap_event_rx.recv() => {
                if let Err(e) = write_dap_frame(&mut stdout, &frame).await {
                    warn!("Failed to write BC event to Zed: {e}");
                }
                continue;
            }
            // A client request from the stdin-reader task.
            maybe_body = req_rx.recv() => match maybe_body {
                Some(b) => b,
                None => break, // stdin closed (EOF) or read error
            },
        };

        let msg: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                warn!("Bad JSON from client: {e}");
                continue;
            }
        };

        let command = msg
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let request_seq = msg.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        let arguments = msg
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        debug!("DAP request: {command} (seq={request_seq})");

        if state
            .handle_request(&mut stdout, &command, request_seq, &arguments)
            .await?
        {
            break;
        }
    }

    info!("Native DAP server shutting down");
    Ok(())
}

fn make_response(
    seq: &AtomicU64,
    request_seq: i64,
    command: &str,
    success: bool,
    body: Option<serde_json::Value>,
    message: Option<String>,
) -> serde_json::Value {
    let s = seq.fetch_add(1, Ordering::Relaxed);
    let mut resp = serde_json::json!({
        "seq": s,
        "type": "response",
        "request_seq": request_seq,
        "requestSeq": request_seq,
        "success": success,
        "command": command,
    });
    if let Some(b) = body {
        resp["body"] = b;
    }
    if let Some(m) = message {
        resp["message"] = serde_json::json!(m);
    }
    resp
}

fn make_event(seq: &AtomicU64, event: &str, body: Option<serde_json::Value>) -> serde_json::Value {
    let s = seq.fetch_add(1, Ordering::Relaxed);
    let mut evt = serde_json::json!({
        "seq": s,
        "type": "event",
        "event": event,
    });
    if let Some(b) = body {
        evt["body"] = b;
    }
    evt
}

async fn write_dap<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &serde_json::Value,
) -> std::result::Result<(), std::io::Error> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_dap_frame(writer, &body).await
}

fn json_array(value: serde_json::Value) -> Vec<serde_json::Value> {
    value.as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_response_includes_both_request_seq_keys() {
        let seq = AtomicU64::new(0);
        let resp = make_response(&seq, 42, "initialize", true, None, None);
        assert_eq!(
            resp.get("request_seq").and_then(|v| v.as_i64()),
            Some(42),
            "response missing snake_case `request_seq`"
        );
        assert_eq!(
            resp.get("requestSeq").and_then(|v| v.as_i64()),
            Some(42),
            "response missing camelCase `requestSeq`"
        );
    }

    #[test]
    fn kind_to_object_type_keys_are_real_language_data_keywords() {
        // The bc_object_type::* INTEGERS are Microsoft BC DAP wire constants
        // (legitimately hardcoded). The KEYWORDS that select them are AL language
        // facts — assert each still resolves in language_data, so a future keyword
        // rename that orphans a map entry (live objects then silently → UNKNOWN)
        // fails here instead of in production.
        for kw in [
            "table",
            "report",
            "codeunit",
            "xmlport",
            "page",
            "query",
            "pageextension",
            "tableextension",
            "enum",
            "enumextension",
            "reportextension",
        ] {
            assert_ne!(
                kind_to_object_type(kw),
                bc_object_type::UNKNOWN,
                "{kw} should map to a known BC DAP object type"
            );
            assert!(
                al_syntax::language_data::object_type_by_keyword(kw).is_some(),
                "{kw} must be a real AL object keyword in language_data"
            );
        }
    }

    #[test]
    fn kind_to_object_type_maps_all_known_kinds() {
        // Each AL object kind string from the file index must map to its BC
        // ObjectTypeWrapper integer. A regression here silently sends BC the
        // wrong object type for breakpoints (breakpoints land in the wrong
        // object or are rejected).
        assert_eq!(kind_to_object_type("table"), bc_object_type::TABLE);
        assert_eq!(kind_to_object_type("report"), bc_object_type::REPORT);
        assert_eq!(kind_to_object_type("codeunit"), bc_object_type::CODEUNIT);
        assert_eq!(kind_to_object_type("xmlport"), bc_object_type::XMLPORT);
        assert_eq!(kind_to_object_type("page"), bc_object_type::PAGE);
        assert_eq!(kind_to_object_type("query"), bc_object_type::QUERY);
        assert_eq!(
            kind_to_object_type("pageextension"),
            bc_object_type::PAGE_EXTENSION
        );
        assert_eq!(
            kind_to_object_type("tableextension"),
            bc_object_type::TABLE_EXTENSION
        );
        assert_eq!(kind_to_object_type("enum"), bc_object_type::ENUM);
        assert_eq!(
            kind_to_object_type("enumextension"),
            bc_object_type::ENUM_EXTENSION
        );
        assert_eq!(
            kind_to_object_type("reportextension"),
            bc_object_type::REPORT_EXTENSION
        );
    }

    #[test]
    fn kind_to_object_type_is_case_insensitive() {
        // The file index may surface mixed-case kinds; matching is documented as
        // lowercased so "Codeunit", "CODEUNIT" and "codeunit" must all resolve.
        assert_eq!(kind_to_object_type("Codeunit"), bc_object_type::CODEUNIT);
        assert_eq!(kind_to_object_type("TABLE"), bc_object_type::TABLE);
        assert_eq!(
            kind_to_object_type("PageExtension"),
            bc_object_type::PAGE_EXTENSION
        );
    }

    #[test]
    fn kind_to_object_type_unknown_yields_unknown_sentinel() {
        // An unrecognised kind must map to the UNKNOWN sentinel (-1), never
        // accidentally collide with a valid type.
        assert_eq!(kind_to_object_type(""), bc_object_type::UNKNOWN);
        assert_eq!(kind_to_object_type("controladdin"), bc_object_type::UNKNOWN);
        assert_eq!(kind_to_object_type("not-a-type"), bc_object_type::UNKNOWN);
        assert_eq!(bc_object_type::UNKNOWN, -1);
    }

    #[test]
    fn make_response_failure_carries_message_and_no_body() {
        // A failed DAP response must report success=false and surface the error
        // message; it must NOT carry a body. Zed shows `message` to the user.
        let seq = AtomicU64::new(0);
        let resp = make_response(
            &seq,
            3,
            "launch",
            false,
            None,
            Some("Compilation failed".to_string()),
        );
        assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            resp.get("message").and_then(|v| v.as_str()),
            Some("Compilation failed")
        );
        assert!(
            resp.get("body").is_none(),
            "failure response must not include a body"
        );
        assert_eq!(resp.get("type").and_then(|v| v.as_str()), Some("response"));
        assert_eq!(resp.get("command").and_then(|v| v.as_str()), Some("launch"));
    }

    #[test]
    fn make_response_success_carries_body_and_no_message() {
        let seq = AtomicU64::new(0);
        let resp = make_response(
            &seq,
            1,
            "threads",
            true,
            Some(serde_json::json!({"threads": []})),
            None,
        );
        assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(true));
        assert!(
            resp.get("body").is_some(),
            "success response must have body"
        );
        assert!(
            resp.get("message").is_none(),
            "success response must not carry an error message"
        );
    }

    #[test]
    fn make_response_seq_is_monotonic() {
        // DAP requires non-decreasing seq across all messages. Each call must
        // consume the next value from the shared counter.
        let seq = AtomicU64::new(5);
        let first = make_response(&seq, 0, "a", true, None, None);
        let second = make_response(&seq, 0, "b", true, None, None);
        assert_eq!(first.get("seq").and_then(|v| v.as_u64()), Some(5));
        assert_eq!(second.get("seq").and_then(|v| v.as_u64()), Some(6));
    }

    #[test]
    fn make_event_with_and_without_body() {
        let seq = AtomicU64::new(10);
        let with_body = make_event(
            &seq,
            "stopped",
            Some(serde_json::json!({"reason": "breakpoint"})),
        );
        assert_eq!(
            with_body.get("type").and_then(|v| v.as_str()),
            Some("event")
        );
        assert_eq!(
            with_body.get("event").and_then(|v| v.as_str()),
            Some("stopped")
        );
        assert_eq!(
            with_body
                .get("body")
                .and_then(|b| b.get("reason"))
                .and_then(|v| v.as_str()),
            Some("breakpoint")
        );
        assert_eq!(with_body.get("seq").and_then(|v| v.as_u64()), Some(10));

        let without_body = make_event(&seq, "initialized", None);
        assert!(
            without_body.get("body").is_none(),
            "event with no body must omit the body key"
        );
        assert_eq!(without_body.get("seq").and_then(|v| v.as_u64()), Some(11));
    }

    #[test]
    fn json_array_non_array_yields_empty() {
        assert!(json_array(serde_json::json!(null)).is_empty());
        assert!(json_array(serde_json::json!({ "Name": "x" })).is_empty());
    }

    #[test]
    fn variable_scope_references_match_editorservices_wire_contract() {
        assert_eq!(scope_reference(SCOPE_GLOBALS, 7), Some((1 << 16) | 7));
        assert_eq!(scope_reference(SCOPE_LOCALS, 7), Some((2 << 16) | 7));
        assert_eq!(
            decode_scope_reference((1 << 16) | 7),
            Some((SCOPE_GLOBALS, 7))
        );
        assert_eq!(
            decode_scope_reference((2 << 16) | 7),
            Some((SCOPE_LOCALS, 7))
        );
        assert_eq!(scope_reference(SCOPE_LOCALS, 65_536), None);
        assert_eq!(decode_scope_reference(VARIABLE_HANDLE_BASE), None);
    }

    #[tokio::test]
    async fn write_dap_emits_content_length_framed_body() {
        let seq = AtomicU64::new(1);
        let resp = make_response(&seq, 7, "threads", true, None, None);
        let body = serde_json::to_vec(&resp).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        crate::dap::framing::write_dap_frame(&mut buf, &body)
            .await
            .unwrap();

        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.starts_with(&format!("Content-Length: {}\r\n\r\n", body.len())),
            "frame must lead with an accurate Content-Length header: {text:?}"
        );
        assert!(
            text.contains("\"command\":\"threads\""),
            "serialised body must follow the header: {text:?}"
        );
    }
}
