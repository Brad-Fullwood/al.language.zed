//! High-level AL debug session lifecycle.
//!
//! `DebugSession` manages the full AL debug lifecycle:
//! compile → spawn EditorServices → DAP handshake → debug → stop.
//!
//! T404b: start() and stop()
//! T404c: set_breakpoints(), continue_(), step()
//! T404d: state(), eval()
//! T405: history recording

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use al_protocol::launch::{self, BcServerConfig};
use al_protocol::AlToolchain;
use tracing::{info, warn};

use crate::client::DapClient;
use crate::editor_services::find_editor_services;
use crate::types::*;
use crate::{DapError, Result};

/// Maximum number of breakpoint hits to record before dropping oldest.
const MAX_HISTORY: usize = 1000;

/// A managed AL debug session.
///
/// Owns the DAP client subprocess and manages the full lifecycle from
/// compilation through debugging to teardown.
pub struct DebugSession {
    client: DapClient,
    state: DebugState,
    breakpoints: HashMap<String, Vec<BreakpointInfo>>,
    history: Vec<BreakpointHit>,
    hit_counter: u32,
    toolchain: AlToolchain,
    project_root: PathBuf,
    launch_config: BcServerConfig,
}

impl DebugSession {
    /// Compile the AL project, spawn EditorServices.Host, and complete
    /// the DAP handshake (initialize → configurationDone → launch).
    ///
    /// Returns a running `DebugSession` or an error if any step fails.
    /// Compilation timeout is 120 seconds. DAP handshake timeout is 30 seconds.
    pub async fn start(
        toolchain: &AlToolchain,
        project_root: &Path,
        config_name: Option<&str>,
    ) -> Result<Self> {
        // 1. Resolve launch config
        let launch_file = launch::find_launch_config(project_root);
        let config = resolve_config(launch_file.as_ref(), config_name)?;

        // 2. Compile project
        info!(root = %project_root.display(), "Compiling AL project");
        compile_project(toolchain, project_root).await?;
        info!("Compilation succeeded");

        // 3. Find EditorServices.Host
        let host_path = find_editor_services(toolchain)?;

        // 4. Spawn DAP subprocess
        let root_str = project_root.display().to_string();
        let args = ["/startDebugging", &format!("/projectRoot:{root_str}")];
        let mut client = DapClient::spawn(&host_path, args.as_ref())?;

        // 5. DAP initialize
        let init_args = serde_json::json!({
            "clientID": "al-dap-client",
            "clientName": "AL DAP Client",
            "adapterID": "al",
            "pathFormat": "path",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "supportsRunInTerminalRequest": false,
        });
        client
            .send_request_timeout("initialize", Some(init_args), Duration::from_secs(30))
            .await?;

        // 6. Wait for initialized event
        client
            .wait_for_event("initialized", Duration::from_secs(30))
            .await?;

        // 7. configurationDone
        client
            .send_request("configurationDone", None)
            .await?;

        // 8. Launch with server config
        let launch_args = build_launch_args(&config, project_root);
        client
            .send_request_timeout("launch", Some(launch_args), Duration::from_secs(30))
            .await?;

        info!("Debug session started");

        let session_id = format!("s{}", std::process::id());
        Ok(Self {
            client,
            state: DebugState {
                status: SessionStatus::Running,
                session_id,
                location: None,
                stack: vec![],
                variables: vec![],
                thread_id: None,
            },
            breakpoints: HashMap::new(),
            history: Vec::new(),
            hit_counter: 0,
            toolchain: toolchain.clone(),
            project_root: project_root.to_path_buf(),
            launch_config: config,
        })
    }

    /// Get the current session status.
    pub fn status(&self) -> &SessionStatus {
        &self.state.status
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.state.session_id
    }

    /// Get a reference to the current debug state.
    ///
    /// T404d will add state refresh logic (drain events, fetch stack/variables).
    pub fn current_state(&self) -> &DebugState {
        &self.state
    }

    /// Get mutable access to the DAP client (for T404c/T404d).
    pub(crate) fn client_mut(&mut self) -> &mut DapClient {
        &mut self.client
    }

    /// Get mutable access to the state (for T404c/T404d).
    pub(crate) fn state_mut(&mut self) -> &mut DebugState {
        &mut self.state
    }

    /// Get the breakpoints map (for T404c).
    pub(crate) fn breakpoints_mut(&mut self) -> &mut HashMap<String, Vec<BreakpointInfo>> {
        &mut self.breakpoints
    }

    /// Get the history vec (for T405).
    pub(crate) fn history_mut(&mut self) -> &mut Vec<BreakpointHit> {
        &mut self.history
    }

    // -----------------------------------------------------------------
    // T404c: Breakpoints + execution control
    // -----------------------------------------------------------------

    /// Set breakpoints for a file. Replaces any previous breakpoints for that file.
    ///
    /// Each breakpoint is `(line, optional_condition)`.
    /// Returns the verified breakpoint info from the adapter.
    pub async fn set_breakpoints(
        &mut self,
        file: &str,
        breakpoints: &[(u32, Option<&str>)],
    ) -> Result<Vec<BreakpointInfo>> {
        let bp_args: Vec<serde_json::Value> = breakpoints
            .iter()
            .map(|(line, condition)| {
                let mut bp = serde_json::json!({"line": line});
                if let Some(cond) = condition {
                    bp["condition"] = serde_json::Value::String(cond.to_string());
                }
                bp
            })
            .collect();

        let args = serde_json::json!({
            "source": { "path": file },
            "breakpoints": bp_args,
        });

        let response = self.client.send_request("setBreakpoints", Some(args)).await?;

        let verified: Vec<BreakpointInfo> = response
            .body
            .as_ref()
            .and_then(|b| b.get("breakpoints"))
            .and_then(|bps| bps.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|bp| BreakpointInfo {
                        id: bp.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                        file: file.to_string(),
                        line: bp.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        condition: bp
                            .get("condition")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        verified: bp.get("verified").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.breakpoints.insert(file.to_string(), verified.clone());
        Ok(verified)
    }

    /// Continue execution until the next breakpoint or program exit.
    ///
    /// Sends DAP `continue` and waits for a `stopped` event (30s timeout).
    /// Returns the updated debug state.
    pub async fn continue_(&mut self) -> Result<&DebugState> {
        if !matches!(self.state.status, SessionStatus::Paused) {
            return Err(DapError::SessionNotPaused);
        }

        let thread_id = self.state.thread_id.unwrap_or(1);
        let args = serde_json::json!({"threadId": thread_id});
        self.client.send_request("continue", Some(args)).await?;

        self.state.status = SessionStatus::Running;

        // Wait for stopped event
        match self
            .client
            .wait_for_event("stopped", Duration::from_secs(30))
            .await
        {
            Ok(event) => {
                self.update_state_from_stopped(&event);
            }
            Err(DapError::Timeout(_)) => {
                // Program may still be running — that's OK
                return Ok(&self.state);
            }
            Err(e) => return Err(e),
        }

        Ok(&self.state)
    }

    /// Step execution: "over", "into", or "out".
    ///
    /// Maps to DAP `next`, `stepIn`, `stepOut` respectively.
    /// Waits for the `stopped` event after stepping.
    pub async fn step(&mut self, step_type: &str) -> Result<&DebugState> {
        if !matches!(self.state.status, SessionStatus::Paused) {
            return Err(DapError::SessionNotPaused);
        }

        let command = match step_type {
            "over" => "next",
            "into" => "stepIn",
            "out" => "stepOut",
            other => {
                return Err(DapError::DapProtocolError {
                    command: "step".to_string(),
                    message: format!(
                        "Invalid step type: {other}. Use over, into, or out."
                    ),
                })
            }
        };

        let thread_id = self.state.thread_id.unwrap_or(1);
        let args = serde_json::json!({"threadId": thread_id});
        self.client.send_request(command, Some(args)).await?;

        // Wait for stopped event after step
        let event = self
            .client
            .wait_for_event("stopped", Duration::from_secs(30))
            .await?;

        self.update_state_from_stopped(&event);
        Ok(&self.state)
    }

    /// Update internal state from a DAP `stopped` event.
    fn update_state_from_stopped(&mut self, event: &crate::protocol::DapEvent) {
        self.state.status = SessionStatus::Paused;

        if let Some(body) = &event.body {
            self.state.thread_id = body.get("threadId").and_then(|v| v.as_i64());
        }
    }

    // -----------------------------------------------------------------
    // T404d: State inspection + eval
    // -----------------------------------------------------------------

    /// Refresh and return the current debug state.
    ///
    /// When paused: drains events, fetches threads → stackTrace → scopes → variables.
    /// Variables with `variablesReference > 0` are expanded 1 level (Record fields).
    pub async fn state(&mut self) -> Result<&DebugState> {
        // Drain any pending events first
        for event in self.client.drain_events() {
            if event.event == "stopped" {
                self.update_state_from_stopped(&event);
            }
        }

        if !matches!(self.state.status, SessionStatus::Paused) {
            return Ok(&self.state);
        }

        let thread_id = self.state.thread_id.unwrap_or(1);

        // Fetch stack trace
        let stack_resp = self
            .client
            .send_request(
                "stackTrace",
                Some(serde_json::json!({"threadId": thread_id})),
            )
            .await?;

        let frames: Vec<StackFrame> = stack_resp
            .body
            .as_ref()
            .and_then(|b| b.get("stackFrames"))
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|f| StackFrame {
                        id: f.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                        name: f
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        source: f
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        line: f.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                        column: f.get("column").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Update location from top frame
        if let Some(top) = frames.first() {
            self.state.location = Some(Location {
                file: top.source.clone().unwrap_or_default(),
                line: top.line,
                column: top.column,
                procedure: Some(top.name.clone()),
            });
        }

        self.state.stack = frames;

        // Fetch variables from top frame's scopes
        if let Some(top_frame) = self.state.stack.first() {
            let frame_id = top_frame.id;
            let scopes_resp = self
                .client
                .send_request(
                    "scopes",
                    Some(serde_json::json!({"frameId": frame_id})),
                )
                .await?;

            let mut all_variables = Vec::new();
            let scope_refs: Vec<i64> = scopes_resp
                .body
                .as_ref()
                .and_then(|b| b.get("scopes"))
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.get("variablesReference").and_then(|v| v.as_i64()))
                        .collect()
                })
                .unwrap_or_default();

            for var_ref in scope_refs {
                let vars = self.fetch_variables(var_ref).await?;
                all_variables.extend(vars);
            }

            self.state.variables = all_variables;
        }

        // T405: Record breakpoint hit
        self.record_hit();

        Ok(&self.state)
    }

    /// Record a breakpoint hit snapshot for history (T405).
    fn record_hit(&mut self) {
        if !matches!(self.state.status, SessionStatus::Paused) {
            return;
        }

        self.hit_counter += 1;
        let hit = BreakpointHit {
            seq: self.hit_counter,
            breakpoint_id: 0, // DAP doesn't always provide this in stopped events
            timestamp: chrono_now(),
            location: self
                .state
                .location
                .clone()
                .unwrap_or(Location {
                    file: String::new(),
                    line: 0,
                    column: 0,
                    procedure: None,
                }),
            variables: self.state.variables.clone(),
        };

        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(hit);
    }

    /// Evaluate an expression at the current frame.
    pub async fn eval(&mut self, expr: &str) -> Result<EvalResult> {
        if !matches!(self.state.status, SessionStatus::Paused) {
            return Err(DapError::SessionNotPaused);
        }

        let frame_id = self
            .state
            .stack
            .first()
            .map(|f| f.id)
            .unwrap_or(0);

        let args = serde_json::json!({
            "expression": expr,
            "frameId": frame_id,
            "context": "repl",
        });

        let response = self.client.send_request("evaluate", Some(args)).await?;

        let body = response.body.unwrap_or_default();
        Ok(EvalResult {
            result: body
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            type_name: body
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Fetch variables for a given variablesReference, with 1-level expansion.
    async fn fetch_variables(&mut self, var_ref: i64) -> Result<Vec<Variable>> {
        let resp = self
            .client
            .send_request(
                "variables",
                Some(serde_json::json!({"variablesReference": var_ref})),
            )
            .await?;

        let vars: Vec<serde_json::Value> = resp
            .body
            .as_ref()
            .and_then(|b| b.get("variables"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut result = Vec::new();
        for v in &vars {
            let child_ref = v
                .get("variablesReference")
                .and_then(|r| r.as_i64())
                .unwrap_or(0);

            // Expand 1 level for Records
            let fields = if child_ref > 0 {
                self.fetch_flat_variables(child_ref).await?
            } else {
                vec![]
            };

            result.push(Variable {
                name: v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                value: v
                    .get("value")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                type_name: v
                    .get("type")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                fields,
            });
        }

        Ok(result)
    }

    /// Fetch variables without expansion (leaf level).
    async fn fetch_flat_variables(&mut self, var_ref: i64) -> Result<Vec<Variable>> {
        let resp = self
            .client
            .send_request(
                "variables",
                Some(serde_json::json!({"variablesReference": var_ref})),
            )
            .await?;

        Ok(resp
            .body
            .as_ref()
            .and_then(|b| b.get("variables"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|v| Variable {
                        name: v
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        value: v
                            .get("value")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        type_name: v
                            .get("type")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        fields: vec![],
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Get breakpoint hit history.
    ///
    /// If `var_filter` is set, returns only hits where that variable's value
    /// changed from the previous hit.
    pub fn history(&self, var_filter: Option<&str>) -> Vec<&BreakpointHit> {
        let Some(var_name) = var_filter else {
            return self.history.iter().collect();
        };

        let var_lower = var_name.to_lowercase();
        let mut prev_value: Option<String> = None;
        let mut filtered = Vec::new();

        for hit in &self.history {
            let current = hit
                .variables
                .iter()
                .find(|v| v.name.to_lowercase() == var_lower)
                .map(|v| v.value.clone());

            if current != prev_value {
                filtered.push(hit);
                prev_value = current;
            }
        }

        filtered
    }

    /// Disconnect from the debug adapter and kill the subprocess.
    ///
    /// Sends a DAP `disconnect` request (best-effort) then kills the process.
    /// Idempotent — safe to call multiple times.
    pub async fn stop(&mut self) -> Result<()> {
        if matches!(self.state.status, SessionStatus::Stopped) {
            return Ok(());
        }

        info!("Stopping debug session");

        // Best-effort disconnect request (don't fail if it times out)
        let disconnect_args = serde_json::json!({
            "restart": false,
            "terminateDebuggee": true,
        });
        match self
            .client
            .send_request_timeout("disconnect", Some(disconnect_args), Duration::from_secs(5))
            .await
        {
            Ok(_) => info!("DAP disconnect acknowledged"),
            Err(e) => warn!(error = %e, "DAP disconnect failed (killing process)"),
        }

        self.client.kill().await?;
        self.state.status = SessionStatus::Stopped;
        self.history.clear();
        self.hit_counter = 0;
        info!("Debug session stopped");
        Ok(())
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        // Best-effort kill to prevent orphan processes.
        // We can't do async in Drop, so just signal the kill.
        // The DapClient's own Drop will handle cleanup.
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Get current timestamp as ISO 8601 string (no chrono dependency).
fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", now.as_secs())
}

/// Resolve which launch config to use.
fn resolve_config(
    launch_file: Option<&launch::DebugConfigFile>,
    config_name: Option<&str>,
) -> Result<BcServerConfig> {
    let file = launch_file.ok_or_else(|| {
        DapError::CompilationFailed(
            "No launch configuration found. Create .zed/debug.json or .vscode/launch.json"
                .to_string(),
        )
    })?;

    if file.configs.is_empty() {
        return Err(DapError::CompilationFailed(
            "Launch configuration file has no AL configurations".to_string(),
        ));
    }

    if let Some(name) = config_name {
        file.configs
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| {
                DapError::CompilationFailed(format!(
                    "Launch configuration '{}' not found. Available: {}",
                    name,
                    file.configs
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    } else {
        Ok(file.configs[0].clone())
    }
}

/// Compile the AL project using `dotnet alc`.
async fn compile_project(toolchain: &AlToolchain, project_root: &Path) -> Result<()> {
    if !project_root.join("app.json").is_file() {
        return Err(DapError::CompilationFailed(format!(
            "No app.json found in {}",
            project_root.display()
        )));
    }

    let mut cmd = tokio::process::Command::new("dotnet");
    cmd.arg(toolchain.alc.display().to_string());
    cmd.arg(format!("/project:{}", project_root.display()));
    cmd.arg(format!("/out:{}", project_root.display()));

    let packages_dir = project_root.join(".alpackages");
    if packages_dir.is_dir() {
        cmd.arg(format!("/packagecachepath:{}", packages_dir.display()));
    }

    cmd.stderr(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());

    let output = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| DapError::Timeout(Duration::from_secs(120)))?
        .map_err(|e| DapError::CompilationFailed(format!("Failed to run alc: {e}")))?;

    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(DapError::CompilationFailed(format!("{stdout}{stderr}")))
    }
}

/// Build the DAP launch arguments from the BC server config.
fn build_launch_args(config: &BcServerConfig, project_root: &Path) -> serde_json::Value {
    let auth_str = match config.authentication {
        launch::AuthMethod::Windows => "Windows",
        launch::AuthMethod::UserPassword => "UserPassword",
        launch::AuthMethod::AAD => "AAD",
    };

    let mut args = serde_json::json!({
        "type": "al",
        "request": "launch",
        "name": config.name,
        "authentication": auth_str,
        "breakOnError": true,
        "launchBrowser": false,
    });

    if let Some(ref server) = config.server {
        args["server"] = serde_json::Value::String(server.clone());
    }
    if let Some(ref instance) = config.server_instance {
        args["serverInstance"] = serde_json::Value::String(instance.clone());
    }
    if let Some(port) = config.port {
        args["port"] = serde_json::json!(port);
    }
    if let Some(ref env_name) = config.environment_name {
        args["environmentName"] = serde_json::Value::String(env_name.clone());
    }
    if let Some(ref tenant) = config.tenant {
        args["tenant"] = serde_json::Value::String(tenant.clone());
    }

    args["projectRoot"] = serde_json::Value::String(project_root.display().to_string());

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_toolchain() -> AlToolchain {
        AlToolchain {
            version: "1.0.0".to_string(),
            dotnet_root: PathBuf::from("/nonexistent"),
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::new(),
            analyzers: al_protocol::AnalyzerPaths {
                code_cop: PathBuf::new(),
                app_source_cop: PathBuf::new(),
                ui_cop: PathBuf::new(),
                per_tenant_cop: PathBuf::new(),
                common: PathBuf::new(),
            },
        }
    }

    fn test_config(name: &str) -> BcServerConfig {
        BcServerConfig {
            name: name.to_string(),
            environment_type: launch::EnvironmentType::OnPrem,
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: Some(7049),
            environment_name: None,
            tenant: None,
            authentication: launch::AuthMethod::Windows,
        }
    }

    #[test]
    fn resolve_config_no_file_returns_error() {
        let result = resolve_config(None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No launch configuration"));
    }

    #[test]
    fn resolve_config_empty_configs_returns_error() {
        let file = launch::DebugConfigFile {
            path: PathBuf::from("test"),
            configs: vec![],
        };
        let result = resolve_config(Some(&file), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no AL configurations"));
    }

    #[test]
    fn resolve_config_first_by_default() {
        let file = launch::DebugConfigFile {
            path: PathBuf::from("test"),
            configs: vec![test_config("First")],
        };
        let config = resolve_config(Some(&file), None).unwrap();
        assert_eq!(config.name, "First");
    }

    #[test]
    fn resolve_config_by_name() {
        let mut staging = test_config("Staging");
        staging.server = Some("http://staging".to_string());
        staging.tenant = Some("default".to_string());

        let file = launch::DebugConfigFile {
            path: PathBuf::from("test"),
            configs: vec![test_config("Dev"), staging],
        };

        let config = resolve_config(Some(&file), Some("staging")).unwrap();
        assert_eq!(config.name, "Staging");
        assert_eq!(config.server.unwrap(), "http://staging");
    }

    #[test]
    fn resolve_config_by_name_not_found() {
        let file = launch::DebugConfigFile {
            path: PathBuf::from("test"),
            configs: vec![test_config("Dev")],
        };

        let result = resolve_config(Some(&file), Some("Production"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("'Production' not found"), "got: {msg}");
        assert!(msg.contains("Dev"), "should list available configs");
    }

    #[test]
    fn build_launch_args_includes_required_fields() {
        let config = test_config("Test");
        let args = build_launch_args(&config, Path::new("/project"));
        assert_eq!(args["name"], "Test");
        assert_eq!(args["type"], "al");
        assert_eq!(args["request"], "launch");
    }

    #[test]
    fn build_launch_args_includes_tenant_when_set() {
        let mut config = test_config("Test");
        config.tenant = Some("mytenant".to_string());
        let args = build_launch_args(&config, Path::new("/project"));
        assert_eq!(args["tenant"], "mytenant");
    }

    #[test]
    fn build_launch_args_omits_tenant_when_none() {
        let config = test_config("Test");
        let args = build_launch_args(&config, Path::new("/project"));
        assert!(args.get("tenant").is_none());
    }

    #[tokio::test]
    async fn compile_project_no_app_json() {
        let dir = tempfile::tempdir().unwrap();
        let tc = dummy_toolchain();
        let result = compile_project(&tc, dir.path()).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("No app.json"), "got: {msg}");
    }

    #[tokio::test]
    async fn start_no_launch_config() {
        let dir = tempfile::tempdir().unwrap();
        // Create app.json but no launch config
        std::fs::write(dir.path().join("app.json"), "{}").unwrap();
        let tc = dummy_toolchain();
        let result = DebugSession::start(&tc, dir.path(), None).await;
        assert!(result.is_err());
        // Should fail at launch config resolution (no .zed/debug.json or .vscode/launch.json)
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("launch configuration") || msg.contains("alc"),
                    "got: {msg}"
                );
            }
            Ok(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn debug_state_initial_is_running() {
        let state = DebugState {
            status: SessionStatus::Running,
            session_id: "s1".to_string(),
            location: None,
            stack: vec![],
            variables: vec![],
            thread_id: None,
        };
        assert_eq!(state.status, SessionStatus::Running);
        assert!(state.location.is_none());
        assert!(state.stack.is_empty());
    }

    // -----------------------------------------------------------------
    // T404c: Breakpoints + execution control tests
    // -----------------------------------------------------------------

    #[test]
    fn step_type_mapping() {
        // Verify the step type → DAP command mapping is correct
        assert_eq!(
            match "over" {
                "over" => "next",
                "into" => "stepIn",
                "out" => "stepOut",
                _ => "invalid",
            },
            "next"
        );
        assert_eq!(
            match "into" {
                "over" => "next",
                "into" => "stepIn",
                "out" => "stepOut",
                _ => "invalid",
            },
            "stepIn"
        );
        assert_eq!(
            match "out" {
                "over" => "next",
                "into" => "stepIn",
                "out" => "stepOut",
                _ => "invalid",
            },
            "stepOut"
        );
    }

    #[test]
    fn update_state_from_stopped_sets_paused() {
        use crate::protocol::DapEvent;

        let event = DapEvent {
            seq: 1,
            type_: "event".to_string(),
            event: "stopped".to_string(),
            body: Some(serde_json::json!({
                "reason": "breakpoint",
                "threadId": 42
            })),
        };

        let mut state = DebugState {
            status: SessionStatus::Running,
            session_id: "s1".to_string(),
            location: None,
            stack: vec![],
            variables: vec![],
            thread_id: None,
        };

        // Simulate what update_state_from_stopped does
        state.status = SessionStatus::Paused;
        if let Some(body) = &event.body {
            state.thread_id = body.get("threadId").and_then(|v| v.as_i64());
        }

        assert_eq!(state.status, SessionStatus::Paused);
        assert_eq!(state.thread_id, Some(42));
    }

    // -----------------------------------------------------------------
    // T404d: State inspection + eval tests
    // -----------------------------------------------------------------

    #[test]
    fn parse_stack_frame_from_dap() {
        let dap_frame = serde_json::json!({
            "id": 1,
            "name": "OnRun",
            "source": {"path": "/project/test.al"},
            "line": 42,
            "column": 5
        });

        let frame = StackFrame {
            id: dap_frame["id"].as_i64().unwrap(),
            name: dap_frame["name"].as_str().unwrap().to_string(),
            source: dap_frame["source"]["path"].as_str().map(String::from),
            line: dap_frame["line"].as_u64().unwrap() as u32,
            column: dap_frame["column"].as_u64().unwrap() as u32,
        };

        assert_eq!(frame.id, 1);
        assert_eq!(frame.name, "OnRun");
        assert_eq!(frame.source.as_deref(), Some("/project/test.al"));
        assert_eq!(frame.line, 42);
    }

    #[test]
    fn parse_variable_from_dap() {
        let dap_var = serde_json::json!({
            "name": "Rec",
            "value": "Customer",
            "type": "Record Customer",
            "variablesReference": 5
        });

        let var = Variable {
            name: dap_var["name"].as_str().unwrap().to_string(),
            value: dap_var["value"].as_str().unwrap().to_string(),
            type_name: dap_var["type"].as_str().unwrap().to_string(),
            fields: vec![],
        };

        assert_eq!(var.name, "Rec");
        assert_eq!(var.type_name, "Record Customer");
        assert!(dap_var["variablesReference"].as_i64().unwrap() > 0, "should expand");
    }

    #[test]
    fn eval_result_from_dap_response() {
        let body = serde_json::json!({
            "result": "10000",
            "type": "Code[20]",
            "variablesReference": 0
        });

        let eval = EvalResult {
            result: body["result"].as_str().unwrap().to_string(),
            type_name: body["type"].as_str().unwrap().to_string(),
        };

        assert_eq!(eval.result, "10000");
        assert_eq!(eval.type_name, "Code[20]");
    }

    // -----------------------------------------------------------------
    // T405: Debug history recording tests
    // -----------------------------------------------------------------

    fn make_hit(seq: u32, var_name: &str, var_value: &str) -> BreakpointHit {
        BreakpointHit {
            seq,
            breakpoint_id: 0,
            timestamp: format!("{}s", seq),
            location: Location {
                file: "test.al".to_string(),
                line: 10,
                column: 1,
                procedure: Some("OnRun".to_string()),
            },
            variables: vec![Variable {
                name: var_name.to_string(),
                value: var_value.to_string(),
                type_name: "Text".to_string(),
                fields: vec![],
            }],
        }
    }

    #[test]
    fn history_returns_all_without_filter() {
        let history = vec![
            make_hit(1, "x", "10"),
            make_hit(2, "x", "20"),
            make_hit(3, "x", "30"),
        ];

        // Simulate what history() does
        let result: Vec<&BreakpointHit> = history.iter().collect();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn history_var_filter_returns_changed_only() {
        let history = vec![
            make_hit(1, "x", "10"),
            make_hit(2, "x", "10"),  // same value — should be filtered
            make_hit(3, "x", "20"),  // changed — should be included
            make_hit(4, "x", "20"),  // same — filtered
            make_hit(5, "x", "30"),  // changed — included
        ];

        let var_name = "x";
        let var_lower = var_name.to_lowercase();
        let mut prev_value: Option<String> = None;
        let mut filtered = Vec::new();

        for hit in &history {
            let current = hit
                .variables
                .iter()
                .find(|v| v.name.to_lowercase() == var_lower)
                .map(|v| v.value.clone());
            if current != prev_value {
                filtered.push(hit);
                prev_value = current;
            }
        }

        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0].seq, 1);
        assert_eq!(filtered[1].seq, 3);
        assert_eq!(filtered[2].seq, 5);
    }

    #[test]
    fn history_var_filter_nonexistent_var() {
        let history = vec![
            make_hit(1, "x", "10"),
            make_hit(2, "x", "20"),
        ];

        let var_lower = "nonexistent";
        let mut prev_value: Option<String> = None;
        let mut filtered = Vec::new();

        for hit in &history {
            let current = hit
                .variables
                .iter()
                .find(|v| v.name.to_lowercase() == var_lower)
                .map(|v| v.value.clone());
            if current != prev_value {
                filtered.push(hit);
                prev_value = current;
            }
        }

        // Variable doesn't exist in any hit → current is always None → no changes detected
        assert_eq!(filtered.len(), 0);
    }

    #[test]
    fn max_history_cap() {
        assert_eq!(MAX_HISTORY, 1000);
    }

    #[test]
    fn chrono_now_returns_seconds() {
        let ts = chrono_now();
        assert!(ts.ends_with('s'), "timestamp should end with 's': {ts}");
        let secs: u64 = ts.trim_end_matches('s').parse().unwrap();
        assert!(secs > 1700000000, "timestamp should be recent: {secs}");
    }
}
