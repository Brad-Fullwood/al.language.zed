//! Debug Adapter Protocol implementation for AL/Business Central.
//!
//! This crate implements a DAP server that bridges between Zed (the DAP client)
//! and a .NET subprocess that communicates with Business Central servers.
//!
//! Architecture:
//! ```text
//! Zed/Client ──DAP──► al-dap (Rust) ──JSON-RPC/stdio──► AlDap.exe (.NET) ──BC API──► Business Central
//! ```
//!
//! The Rust side handles DAP protocol framing (Content-Length headers) on
//! stdin/stdout to Zed. The .NET side handles actual BC server communication
//! using Microsoft's deployment DLLs.

pub mod bc_client;
pub mod protocol;

use std::sync::atomic::{AtomicU64, Ordering};

use al_discovery::AlToolchain;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tracing::{debug, error, info, warn};

use bc_client::BcBridge;
use protocol::*;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during DAP operation.
#[derive(Debug, Error)]
pub enum DapError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Bridge spawn failed: {0}")]
    BridgeSpawnFailed(String),

    #[error("Bridge process died unexpectedly")]
    BridgeDied,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Operation timed out")]
    Timeout,

    #[error("Not connected to a Business Central server")]
    NotConnected,
}

// ---------------------------------------------------------------------------
// DAP server
// ---------------------------------------------------------------------------

/// Run the DAP server over stdio.
///
/// Reads Content-Length framed DAP messages from stdin, dispatches to handlers,
/// and writes DAP responses/events to stdout. The server runs until the client
/// sends a `disconnect` request or stdin is closed.
pub async fn run_dap_server(toolchain: &AlToolchain) -> Result<(), DapError> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_dap_server_on(toolchain, stdin, stdout).await
}

/// Run the DAP server on arbitrary read/write streams.
///
/// This is the core implementation, separated from stdio for testability.
pub async fn run_dap_server_on<R, W>(
    toolchain: &AlToolchain,
    reader: R,
    writer: W,
) -> Result<(), DapError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    let mut session = DapSession::new(toolchain.clone());

    info!("DAP server starting");

    loop {
        let msg = match protocol::read_message(&mut reader).await {
            Ok(msg) => msg,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!("DAP client disconnected (EOF)");
                break;
            }
            Err(e) => {
                error!("Failed to read DAP message: {e}");
                return Err(DapError::IoError(e));
            }
        };

        let request: DapRequest = match serde_json::from_value(msg) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to parse DAP request: {e}");
                continue;
            }
        };

        debug!("← DAP request: {} (seq={})", request.command, request.seq);

        let (response, events) = session.handle_request(&request).await;

        // Write the response
        let response_json = serde_json::to_value(&response)
            .map_err(|e| DapError::ProtocolError(format!("Failed to serialize response: {e}")))?;
        protocol::write_message(&mut writer, &response_json).await?;
        debug!(
            "→ DAP response: {} success={}",
            response.command, response.success
        );

        // Write any events
        for event in &events {
            let event_json = serde_json::to_value(event)
                .map_err(|e| DapError::ProtocolError(format!("Failed to serialize event: {e}")))?;
            protocol::write_message(&mut writer, &event_json).await?;
            debug!("→ DAP event: {}", event.event);
        }

        // Check for disconnect
        if request.command == "disconnect" {
            info!("DAP server shutting down (disconnect request)");
            break;
        }
    }

    // Clean up bridge if it exists
    if let Some(bridge) = session.bridge.take() {
        let _ = bridge.disconnect().await;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Internal session state for the DAP server.
struct DapSession {
    toolchain: AlToolchain,
    bridge: Option<BcBridge>,
    seq: AtomicU64,
    initialized: bool,
    launch_config: Option<AlLaunchConfig>,
}

impl DapSession {
    fn new(toolchain: AlToolchain) -> Self {
        Self {
            toolchain,
            bridge: None,
            seq: AtomicU64::new(1),
            initialized: false,
            launch_config: None,
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn make_response(
        &self,
        request: &DapRequest,
        success: bool,
        body: Option<serde_json::Value>,
        message: Option<String>,
    ) -> DapResponse {
        DapResponse {
            seq: self.next_seq(),
            type_: "response".to_string(),
            request_seq: request.seq,
            success,
            command: request.command.clone(),
            message,
            body,
        }
    }

    fn make_event(
        &self,
        event: &str,
        body: Option<serde_json::Value>,
    ) -> DapEvent {
        DapEvent {
            seq: self.next_seq(),
            type_: "event".to_string(),
            event: event.to_string(),
            body,
        }
    }

    fn success_response(
        &self,
        request: &DapRequest,
        body: Option<serde_json::Value>,
    ) -> DapResponse {
        self.make_response(request, true, body, None)
    }

    fn error_response(&self, request: &DapRequest, message: &str) -> DapResponse {
        self.make_response(request, false, None, Some(message.to_string()))
    }

    // -----------------------------------------------------------------------
    // Request dispatch
    // -----------------------------------------------------------------------

    async fn handle_request(&mut self, request: &DapRequest) -> (DapResponse, Vec<DapEvent>) {
        let mut events = Vec::new();

        let response = match request.command.as_str() {
            "initialize" => self.handle_initialize(request),
            "launch" => {
                let resp = self.handle_launch(request).await;
                if resp.success {
                    // After a successful launch, the DAP spec says we should
                    // send an "initialized" event.
                    events.push(self.make_event("initialized", None));
                }
                resp
            }
            "configurationDone" => self.handle_configuration_done(request),
            "setBreakpoints" => self.handle_set_breakpoints(request).await,
            "threads" => self.handle_threads(request).await,
            "stackTrace" => self.handle_stack_trace(request).await,
            "scopes" => self.handle_scopes(request).await,
            "variables" => self.handle_variables(request).await,
            "continue" => self.handle_continue(request).await,
            "next" => self.handle_next(request).await,
            "stepIn" => self.handle_step_in(request).await,
            "stepOut" => self.handle_step_out(request).await,
            "evaluate" => self.handle_evaluate(request).await,
            "disconnect" => self.handle_disconnect(request).await,
            other => {
                warn!("Unsupported DAP command: {other}");
                self.error_response(request, &format!("Unsupported command: {other}"))
            }
        };

        (response, events)
    }

    // -----------------------------------------------------------------------
    // Individual handlers
    // -----------------------------------------------------------------------

    fn handle_initialize(&mut self, request: &DapRequest) -> DapResponse {
        self.initialized = true;
        let capabilities = Capabilities::default();
        let body = serde_json::to_value(capabilities).unwrap_or_default();
        self.success_response(request, Some(body))
    }

    async fn handle_launch(&mut self, request: &DapRequest) -> DapResponse {
        let config: AlLaunchConfig = match request
            .arguments
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            Some(c) => c,
            None => {
                return self.error_response(
                    request,
                    "Missing or invalid launch configuration. Required: server, serverInstance",
                );
            }
        };

        info!(
            "Launching debug session: {}:{} tenant={}",
            config.server, config.server_instance, config.tenant
        );

        // Spawn the .NET bridge
        let bridge = match BcBridge::spawn(&self.toolchain).await {
            Ok(b) => b,
            Err(e) => {
                return self.error_response(
                    request,
                    &format!("Failed to spawn debug bridge: {e}"),
                );
            }
        };

        // Connect to BC
        if let Err(e) = bridge.connect(&config).await {
            let _ = bridge.disconnect().await;
            return self.error_response(
                request,
                &format!("Failed to connect to Business Central: {e}"),
            );
        }

        self.bridge = Some(bridge);
        self.launch_config = Some(config);

        self.success_response(request, None)
    }

    fn handle_configuration_done(&self, request: &DapRequest) -> DapResponse {
        self.success_response(request, None)
    }

    async fn handle_set_breakpoints(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let args: SetBreakpointsArguments = match request
            .arguments
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            Some(a) => a,
            None => {
                return self.error_response(request, "Missing setBreakpoints arguments");
            }
        };

        let file = args.source.path.as_deref().unwrap_or("");

        match bridge.set_breakpoints(file, args.breakpoints).await {
            Ok(breakpoints) => {
                let body = serde_json::json!({ "breakpoints": breakpoints });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Failed to set breakpoints: {e}")),
        }
    }

    async fn handle_threads(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        match bridge.threads().await {
            Ok(threads) => {
                let body = serde_json::json!({ "threads": threads });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Failed to get threads: {e}")),
        }
    }

    async fn handle_stack_trace(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let thread_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("threadId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.stack_trace(thread_id).await {
            Ok(frames) => {
                let body = serde_json::json!({
                    "stackFrames": frames,
                    "totalFrames": frames.len(),
                });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Failed to get stack trace: {e}")),
        }
    }

    async fn handle_scopes(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let frame_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("frameId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.scopes(frame_id).await {
            Ok(scopes) => {
                let body = serde_json::json!({ "scopes": scopes });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Failed to get scopes: {e}")),
        }
    }

    async fn handle_variables(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let reference = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("variablesReference"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.variables(reference).await {
            Ok(variables) => {
                let body = serde_json::json!({ "variables": variables });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Failed to get variables: {e}")),
        }
    }

    async fn handle_continue(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let thread_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("threadId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.continue_execution(thread_id).await {
            Ok(()) => {
                let body = serde_json::json!({ "allThreadsContinued": true });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Continue failed: {e}")),
        }
    }

    async fn handle_next(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let thread_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("threadId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.step_over(thread_id).await {
            Ok(()) => self.success_response(request, None),
            Err(e) => self.error_response(request, &format!("Next (step over) failed: {e}")),
        }
    }

    async fn handle_step_in(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let thread_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("threadId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.step_in(thread_id).await {
            Ok(()) => self.success_response(request, None),
            Err(e) => self.error_response(request, &format!("Step in failed: {e}")),
        }
    }

    async fn handle_step_out(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let thread_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("threadId"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        match bridge.step_out(thread_id).await {
            Ok(()) => self.success_response(request, None),
            Err(e) => self.error_response(request, &format!("Step out failed: {e}")),
        }
    }

    async fn handle_evaluate(&self, request: &DapRequest) -> DapResponse {
        let bridge = match &self.bridge {
            Some(b) => b,
            None => return self.error_response(request, "Not connected"),
        };

        let expression = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("expression"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let frame_id = request
            .arguments
            .as_ref()
            .and_then(|v| v.get("frameId"))
            .and_then(|v| v.as_u64());

        match bridge.evaluate(expression, frame_id).await {
            Ok(result) => {
                let body = serde_json::json!({
                    "result": result,
                    "variablesReference": 0,
                });
                self.success_response(request, Some(body))
            }
            Err(e) => self.error_response(request, &format!("Evaluate failed: {e}")),
        }
    }

    async fn handle_disconnect(&mut self, request: &DapRequest) -> DapResponse {
        if let Some(bridge) = self.bridge.take() {
            let _ = bridge.disconnect().await;
        }
        self.launch_config = None;
        self.success_response(request, None)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -----------------------------------------------------------------------
    // DAP message serialization/deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_dap_request_serialization() {
        let request = DapRequest {
            seq: 1,
            type_: "request".to_string(),
            command: "initialize".to_string(),
            arguments: Some(json!({"clientID": "zed", "adapterID": "al"})),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["seq"], 1);
        assert_eq!(json["type"], "request");
        assert_eq!(json["command"], "initialize");
        assert_eq!(json["arguments"]["clientID"], "zed");
    }

    #[test]
    fn test_dap_request_deserialization() {
        let json = json!({
            "seq": 5,
            "type": "request",
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": "/src/hello.al" },
                "breakpoints": [{ "line": 10 }]
            }
        });

        let request: DapRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.seq, 5);
        assert_eq!(request.type_, "request");
        assert_eq!(request.command, "setBreakpoints");
        assert!(request.arguments.is_some());
    }

    #[test]
    fn test_dap_request_without_arguments() {
        let json = json!({
            "seq": 3,
            "type": "request",
            "command": "disconnect"
        });

        let request: DapRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.command, "disconnect");
        assert!(request.arguments.is_none());
    }

    #[test]
    fn test_dap_response_serialization() {
        let response = DapResponse {
            seq: 1,
            type_: "response".to_string(),
            request_seq: 5,
            success: true,
            command: "initialize".to_string(),
            message: None,
            body: Some(json!({"supportsConfigurationDoneRequest": true})),
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["type"], "response");
        assert_eq!(json["request_seq"], 5);
        assert_eq!(json["success"], true);
        assert_eq!(json["command"], "initialize");
        // `message` should be absent when None
        assert!(json.get("message").is_none());
    }

    #[test]
    fn test_dap_response_error() {
        let response = DapResponse {
            seq: 2,
            type_: "response".to_string(),
            request_seq: 7,
            success: false,
            command: "launch".to_string(),
            message: Some("Connection refused".to_string()),
            body: None,
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["success"], false);
        assert_eq!(json["message"], "Connection refused");
        assert!(json.get("body").is_none());
    }

    #[test]
    fn test_dap_event_serialization() {
        let event = DapEvent {
            seq: 10,
            type_: "event".to_string(),
            event: "stopped".to_string(),
            body: Some(json!({
                "reason": "breakpoint",
                "threadId": 1,
                "allThreadsStopped": true
            })),
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "event");
        assert_eq!(json["event"], "stopped");
        assert_eq!(json["body"]["reason"], "breakpoint");
    }

    #[test]
    fn test_dap_event_without_body() {
        let event = DapEvent {
            seq: 11,
            type_: "event".to_string(),
            event: "initialized".to_string(),
            body: None,
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "initialized");
        assert!(json.get("body").is_none());
    }

    // -----------------------------------------------------------------------
    // Launch config deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_launch_config_full() {
        let json = json!({
            "server": "http://localhost",
            "serverInstance": "BC",
            "tenant": "default",
            "authentication": "UserPassword",
            "breakpointOnError": "All",
            "launchBrowser": true,
            "startupObjectId": 22,
            "startupObjectType": "Page"
        });

        let config: AlLaunchConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.server, "http://localhost");
        assert_eq!(config.server_instance, "BC");
        assert_eq!(config.tenant, "default");
        assert_eq!(config.authentication, "UserPassword");
        assert_eq!(config.breakpoint_on_error, Some("All".to_string()));
        assert_eq!(config.launch_browser, Some(true));
        assert_eq!(config.startup_object_id, Some(22));
        assert_eq!(config.startup_object_type, Some("Page".to_string()));
    }

    #[test]
    fn test_launch_config_minimal() {
        let json = json!({
            "server": "http://myserver",
            "serverInstance": "Production"
        });

        let config: AlLaunchConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.server, "http://myserver");
        assert_eq!(config.server_instance, "Production");
        assert_eq!(config.tenant, "default");
        assert_eq!(config.authentication, "UserPassword");
        assert!(config.breakpoint_on_error.is_none());
        assert!(config.launch_browser.is_none());
        assert!(config.startup_object_id.is_none());
    }

    #[test]
    fn test_launch_config_aad_auth() {
        let json = json!({
            "server": "https://cloud.bc.dynamics.com",
            "serverInstance": "SaaS",
            "authentication": "AAD",
            "tenant": "my-tenant-id"
        });

        let config: AlLaunchConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.authentication, "AAD");
        assert_eq!(config.tenant, "my-tenant-id");
    }

    // -----------------------------------------------------------------------
    // Capabilities
    // -----------------------------------------------------------------------

    #[test]
    fn test_capabilities_default() {
        let caps = Capabilities::default();
        assert!(caps.supports_configuration_done_request);
        assert!(caps.supports_evaluate_for_hovers);
        assert!(!caps.supports_step_back);
        assert!(caps.supports_set_variable);
        assert!(!caps.supports_restart_request);
        assert!(caps.supports_conditional_breakpoints);
        assert!(caps.supports_hit_conditional_breakpoints);
        assert!(caps.supports_log_points);
    }

    #[test]
    fn test_capabilities_serialization_roundtrip() {
        let caps = Capabilities::default();
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["supportsConfigurationDoneRequest"], true);
        assert_eq!(json["supportsEvaluateForHovers"], true);
        assert_eq!(json["supportsStepBack"], false);

        let caps2: Capabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps2.supports_configuration_done_request, caps.supports_configuration_done_request);
        assert_eq!(caps2.supports_step_back, caps.supports_step_back);
    }

    // -----------------------------------------------------------------------
    // Protocol types
    // -----------------------------------------------------------------------

    #[test]
    fn test_source_breakpoint_serialization() {
        let bp = SourceBreakpoint {
            line: 42,
            column: Some(5),
            condition: Some("x > 10".to_string()),
            hit_condition: None,
            log_message: Some("Hit line 42".to_string()),
        };

        let json = serde_json::to_value(&bp).unwrap();
        assert_eq!(json["line"], 42);
        assert_eq!(json["column"], 5);
        assert_eq!(json["condition"], "x > 10");
        assert!(json.get("hitCondition").is_none()); // None fields omitted
        assert_eq!(json["logMessage"], "Hit line 42");
    }

    #[test]
    fn test_breakpoint_response() {
        let bp = Breakpoint {
            id: Some(1),
            verified: true,
            message: None,
            line: Some(42),
            column: None,
            source: Some(Source {
                name: Some("Hello.al".to_string()),
                path: Some("/src/Hello.al".to_string()),
            }),
        };

        let json = serde_json::to_value(&bp).unwrap();
        assert_eq!(json["verified"], true);
        assert_eq!(json["line"], 42);
        assert_eq!(json["source"]["name"], "Hello.al");
    }

    #[test]
    fn test_thread_serialization() {
        let thread = Thread {
            id: 1,
            name: "Main Thread".to_string(),
        };
        let json = serde_json::to_value(&thread).unwrap();
        assert_eq!(json["id"], 1);
        assert_eq!(json["name"], "Main Thread");
    }

    #[test]
    fn test_stack_frame_serialization() {
        let frame = StackFrame {
            id: 100,
            name: "OnRun".to_string(),
            source: Some(Source {
                name: Some("Hello.al".to_string()),
                path: Some("/project/src/Hello.al".to_string()),
            }),
            line: 15,
            column: 1,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["id"], 100);
        assert_eq!(json["name"], "OnRun");
        assert_eq!(json["line"], 15);
    }

    #[test]
    fn test_scope_serialization() {
        let scope = Scope {
            name: "Locals".to_string(),
            variables_reference: 1001,
            expensive: false,
        };
        let json = serde_json::to_value(&scope).unwrap();
        assert_eq!(json["name"], "Locals");
        assert_eq!(json["variablesReference"], 1001);
        assert_eq!(json["expensive"], false);
    }

    #[test]
    fn test_variable_serialization() {
        let var = Variable {
            name: "CustomerNo".to_string(),
            value: "10000".to_string(),
            type_: Some("Code[20]".to_string()),
            variables_reference: 0,
        };
        let json = serde_json::to_value(&var).unwrap();
        assert_eq!(json["name"], "CustomerNo");
        assert_eq!(json["value"], "10000");
        assert_eq!(json["type"], "Code[20]");
        assert_eq!(json["variablesReference"], 0);
    }

    #[test]
    fn test_variable_without_type() {
        let var = Variable {
            name: "Result".to_string(),
            value: "true".to_string(),
            type_: None,
            variables_reference: 0,
        };
        let json = serde_json::to_value(&var).unwrap();
        assert!(json.get("type").is_none());
    }

    // -----------------------------------------------------------------------
    // DapError display
    // -----------------------------------------------------------------------

    #[test]
    fn test_dap_error_display() {
        let err = DapError::IoError(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(err.to_string().contains("IO error"));
        assert!(err.to_string().contains("refused"));

        let err = DapError::ProtocolError("bad framing".to_string());
        assert_eq!(err.to_string(), "Protocol error: bad framing");

        let err = DapError::BridgeSpawnFailed("dotnet not found".to_string());
        assert!(err.to_string().contains("dotnet not found"));

        let err = DapError::BridgeDied;
        assert_eq!(err.to_string(), "Bridge process died unexpectedly");

        let err = DapError::ConnectionFailed("auth failed".to_string());
        assert!(err.to_string().contains("auth failed"));

        let err = DapError::Timeout;
        assert_eq!(err.to_string(), "Operation timed out");

        let err = DapError::NotConnected;
        assert!(err.to_string().contains("Not connected"));
    }

    // -----------------------------------------------------------------------
    // Content-Length framing
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_message() {
        let msg = json!({"seq": 1, "type": "request", "command": "initialize"});
        let encoded = protocol::encode_message(&msg);
        let encoded_str = String::from_utf8(encoded.clone()).unwrap();

        assert!(encoded_str.starts_with("Content-Length: "));
        assert!(encoded_str.contains("\r\n\r\n"));

        // Verify the content length is correct
        let parts: Vec<&str> = encoded_str.splitn(2, "\r\n\r\n").collect();
        let header = parts[0];
        let body = parts[1];
        let claimed_length: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(claimed_length, body.len());
    }

    #[tokio::test]
    async fn test_read_write_message_roundtrip() {
        let original = json!({
            "seq": 42,
            "type": "request",
            "command": "launch",
            "arguments": {"server": "http://localhost"}
        });

        // Write to a buffer
        let mut buf = Vec::new();
        protocol::write_message(&mut buf, &original).await.unwrap();

        // Read it back
        let mut reader = BufReader::new(buf.as_slice());
        let decoded = protocol::read_message(&mut reader).await.unwrap();

        assert_eq!(decoded["seq"], 42);
        assert_eq!(decoded["type"], "request");
        assert_eq!(decoded["command"], "launch");
        assert_eq!(decoded["arguments"]["server"], "http://localhost");
    }

    #[tokio::test]
    async fn test_read_multiple_messages() {
        let msg1 = json!({"seq": 1, "type": "request", "command": "initialize"});
        let msg2 = json!({"seq": 2, "type": "request", "command": "launch"});
        let msg3 = json!({"seq": 3, "type": "request", "command": "disconnect"});

        let mut buf = Vec::new();
        protocol::write_message(&mut buf, &msg1).await.unwrap();
        protocol::write_message(&mut buf, &msg2).await.unwrap();
        protocol::write_message(&mut buf, &msg3).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let d1 = protocol::read_message(&mut reader).await.unwrap();
        let d2 = protocol::read_message(&mut reader).await.unwrap();
        let d3 = protocol::read_message(&mut reader).await.unwrap();

        assert_eq!(d1["seq"], 1);
        assert_eq!(d2["seq"], 2);
        assert_eq!(d3["seq"], 3);
        assert_eq!(d1["command"], "initialize");
        assert_eq!(d2["command"], "launch");
        assert_eq!(d3["command"], "disconnect");
    }

    #[tokio::test]
    async fn test_read_message_eof() {
        let buf: &[u8] = b"";
        let mut reader = BufReader::new(buf);
        let result = protocol::read_message(&mut reader).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn test_read_message_missing_content_length() {
        let buf: &[u8] = b"X-Custom: foo\r\n\r\n{}";
        let mut reader = BufReader::new(buf);
        let result = protocol::read_message(&mut reader).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_read_message_invalid_json() {
        let buf: &[u8] = b"Content-Length: 11\r\n\r\nnot valid {";
        let mut reader = BufReader::new(buf);
        let result = protocol::read_message(&mut reader).await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // SetBreakpointsArguments
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_breakpoints_args_deserialization() {
        let json = json!({
            "source": { "path": "/project/src/Hello.al" },
            "breakpoints": [
                { "line": 10 },
                { "line": 20, "condition": "x > 5" },
                { "line": 30, "logMessage": "Reached line 30" }
            ]
        });

        let args: SetBreakpointsArguments = serde_json::from_value(json).unwrap();
        assert_eq!(args.source.path, Some("/project/src/Hello.al".to_string()));
        assert_eq!(args.breakpoints.len(), 3);
        assert_eq!(args.breakpoints[0].line, 10);
        assert!(args.breakpoints[0].condition.is_none());
        assert_eq!(args.breakpoints[1].condition, Some("x > 5".to_string()));
        assert_eq!(
            args.breakpoints[2].log_message,
            Some("Reached line 30".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Bridge protocol types
    // -----------------------------------------------------------------------

    #[test]
    fn test_bridge_request_serialization() {
        let req = BridgeRequest {
            id: 1,
            method: "connect".to_string(),
            params: Some(json!({"server": "http://localhost"})),
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["id"], 1);
        assert_eq!(json["method"], "connect");
        assert_eq!(json["params"]["server"], "http://localhost");
    }

    #[test]
    fn test_bridge_request_without_params() {
        let req = BridgeRequest {
            id: 2,
            method: "ping".to_string(),
            params: None,
        };

        let json_str = serde_json::to_string(&req).unwrap();
        // params should not appear in the serialized output
        assert!(!json_str.contains("params"));
    }

    #[test]
    fn test_bridge_response_deserialization() {
        let json = json!({
            "id": 1,
            "result": { "status": "ok" }
        });

        let resp: BridgeResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.id, 1);
        assert_eq!(resp.result["status"], "ok");
    }

    // -----------------------------------------------------------------------
    // DapMessage (raw)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dap_message_type_detection() {
        let json = json!({
            "seq": 1,
            "type": "request",
            "command": "initialize"
        });

        let msg: DapMessage = serde_json::from_value(json).unwrap();
        assert_eq!(msg.type_, "request");
        assert_eq!(msg.seq, 1);
    }

    // -----------------------------------------------------------------------
    // DAP server session (initialize handler)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_dap_initialize_response() {
        // Build a fake initialize request
        let init_request = json!({
            "seq": 1,
            "type": "request",
            "command": "initialize",
            "arguments": {
                "clientID": "zed",
                "adapterID": "al"
            }
        });

        // Encode it with Content-Length framing
        let mut input = protocol::encode_message(&init_request);

        // Also send a disconnect so the server loop terminates
        let disconnect_request = json!({
            "seq": 2,
            "type": "request",
            "command": "disconnect"
        });
        input.extend(protocol::encode_message(&disconnect_request));

        let mut output = Vec::new();

        // Create a dummy toolchain
        let toolchain = dummy_toolchain();

        run_dap_server_on(&toolchain, input.as_slice(), &mut output)
            .await
            .unwrap();

        // Parse the output — should contain at least the initialize response
        let mut reader = BufReader::new(output.as_slice());

        let init_response = protocol::read_message(&mut reader).await.unwrap();
        assert_eq!(init_response["type"], "response");
        assert_eq!(init_response["command"], "initialize");
        assert_eq!(init_response["success"], true);
        assert_eq!(init_response["request_seq"], 1);

        // Check capabilities in the body
        let body = &init_response["body"];
        assert_eq!(body["supportsConfigurationDoneRequest"], true);
        assert_eq!(body["supportsEvaluateForHovers"], true);
        assert_eq!(body["supportsStepBack"], false);
    }

    #[tokio::test]
    async fn test_dap_unsupported_command() {
        let request = json!({
            "seq": 1,
            "type": "request",
            "command": "magicUnicorn"
        });
        let mut input = protocol::encode_message(&request);

        let disconnect = json!({
            "seq": 2,
            "type": "request",
            "command": "disconnect"
        });
        input.extend(protocol::encode_message(&disconnect));

        let mut output = Vec::new();
        let toolchain = dummy_toolchain();

        run_dap_server_on(&toolchain, input.as_slice(), &mut output)
            .await
            .unwrap();

        let mut reader = BufReader::new(output.as_slice());
        let response = protocol::read_message(&mut reader).await.unwrap();
        assert_eq!(response["success"], false);
        assert!(response["message"]
            .as_str()
            .unwrap()
            .contains("Unsupported command"));
    }

    #[tokio::test]
    async fn test_dap_configuration_done() {
        let init = json!({"seq": 1, "type": "request", "command": "initialize"});
        let config_done = json!({"seq": 2, "type": "request", "command": "configurationDone"});
        let disconnect = json!({"seq": 3, "type": "request", "command": "disconnect"});

        let mut input = protocol::encode_message(&init);
        input.extend(protocol::encode_message(&config_done));
        input.extend(protocol::encode_message(&disconnect));

        let mut output = Vec::new();
        let toolchain = dummy_toolchain();

        run_dap_server_on(&toolchain, input.as_slice(), &mut output)
            .await
            .unwrap();

        let mut reader = BufReader::new(output.as_slice());

        // initialize response
        let r1 = protocol::read_message(&mut reader).await.unwrap();
        assert_eq!(r1["command"], "initialize");
        assert_eq!(r1["success"], true);

        // configurationDone response
        let r2 = protocol::read_message(&mut reader).await.unwrap();
        assert_eq!(r2["command"], "configurationDone");
        assert_eq!(r2["success"], true);
    }

    #[tokio::test]
    async fn test_dap_disconnect_cleans_up() {
        let init = json!({"seq": 1, "type": "request", "command": "initialize"});
        let disconnect = json!({"seq": 2, "type": "request", "command": "disconnect"});

        let mut input = protocol::encode_message(&init);
        input.extend(protocol::encode_message(&disconnect));

        let mut output = Vec::new();
        let toolchain = dummy_toolchain();

        let result = run_dap_server_on(&toolchain, input.as_slice(), &mut output).await;
        assert!(result.is_ok());

        let mut reader = BufReader::new(output.as_slice());
        let _r1 = protocol::read_message(&mut reader).await.unwrap();

        let r2 = protocol::read_message(&mut reader).await.unwrap();
        assert_eq!(r2["command"], "disconnect");
        assert_eq!(r2["success"], true);
    }

    #[tokio::test]
    async fn test_dap_eof_terminates_gracefully() {
        // Send just one message, then EOF (no disconnect)
        let init = json!({"seq": 1, "type": "request", "command": "initialize"});
        let input = protocol::encode_message(&init);

        let mut output = Vec::new();
        let toolchain = dummy_toolchain();

        let result = run_dap_server_on(&toolchain, input.as_slice(), &mut output).await;
        assert!(result.is_ok()); // Should not error on EOF
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn dummy_toolchain() -> AlToolchain {
        AlToolchain {
            alc: std::path::PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: std::path::PathBuf::from("/nonexistent/CodeAnalysis.dll"),
            analyzers: al_discovery::AnalyzerPaths {
                code_cop: std::path::PathBuf::from("/nonexistent/CodeCop.dll"),
                app_source_cop: std::path::PathBuf::from("/nonexistent/AppSourceCop.dll"),
                ui_cop: std::path::PathBuf::from("/nonexistent/UICop.dll"),
                per_tenant_cop: std::path::PathBuf::from("/nonexistent/PerTenantCop.dll"),
                common: std::path::PathBuf::from("/nonexistent/Common.dll"),
            },
            dotnet_root: std::path::PathBuf::from("/nonexistent"),
            version: "0.0.0.0".to_string(),
        }
    }
}
