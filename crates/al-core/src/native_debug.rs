//! Native debug session wrapper for CLI/daemon use.
//!
//! Wraps `BcDebugSession` with state the daemon needs that BC doesn't track:
//! breakpoint history, file→breakpoint-ID mapping, and hit counting.

use std::collections::{HashMap, VecDeque};

use al_dap_client::bc_debug::{BcDebugConfig, BcDebugSession};
use al_dap_client::types::*;
use al_dap_client::Result;
use tracing::{info, warn};

/// Wraps `BcDebugSession` with daemon-side state: breakpoint tracking, history.
pub struct NativeDebugSession {
    pub session: BcDebugSession,
    pub config: BcDebugConfig,
    /// file path → list of BC breakpoint IDs
    breakpoints: HashMap<String, Vec<i64>>,
    history: VecDeque<BreakpointHit>,
}

impl NativeDebugSession {
    /// Connect to BC and attach the debug session.
    ///
    /// `config` — BC server connection config (built from launch.json / debug.json).
    /// `access_token` — OAuth Bearer token for the BC tenant.
    pub async fn start(config: BcDebugConfig, access_token: &str) -> Result<Self> {
        let session = BcDebugSession::connect(&config, access_token).await?;

        // Attach and signal configuration done
        session.attach(&config).await?;
        session.configuration_done(&config).await?;

        info!(connection_id = %session.connection_id, "Native debug session started");

        Ok(Self {
            session,
            config,
            breakpoints: HashMap::new(),
            history: VecDeque::new(),
        })
    }

    /// Session identifier (SignalR connection ID).
    pub fn session_id(&self) -> &str {
        &self.session.connection_id
    }

    /// Set breakpoints for a file. Removes old breakpoints for the same file first.
    ///
    /// `object_type` and `object_id` come from the workspace file index — the caller
    /// resolves the AL file path to its BC object type + ID.
    pub async fn set_breakpoints(
        &mut self,
        file: &str,
        lines: &[(u32, Option<&str>)],
        object_type: i32,
        object_id: i32,
    ) -> Result<Vec<BreakpointInfo>> {
        // Remove existing breakpoints for this file
        if let Some(old_ids) = self.breakpoints.remove(file) {
            for bp_id in &old_ids {
                if let Err(e) = self.session.remove_breakpoint(*bp_id).await {
                    warn!(bp_id, error = %e, "Failed to remove old breakpoint");
                }
            }
        }

        // Add new breakpoints
        let mut results = Vec::new();
        let mut new_ids = Vec::new();

        for &(line, condition) in lines {
            let cond = condition.unwrap_or("");
            match self
                .session
                .add_breakpoint(object_type, object_id, line as i64, 0, cond)
                .await
            {
                Ok(resp) => {
                    let bp_id = resp
                        .get("Id")
                        .or_else(|| resp.get("id"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let verified = resp
                        .get("Verified")
                        .or_else(|| resp.get("verified"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    new_ids.push(bp_id);
                    results.push(make_bp_info(file, line, cond, bp_id, verified));
                }
                Err(e) => {
                    warn!(line, error = %e, "Failed to add breakpoint");
                    results.push(make_bp_info(file, line, cond, 0, false));
                }
            }
        }

        if !new_ids.is_empty() {
            self.breakpoints.insert(file.to_string(), new_ids);
        }

        Ok(results)
    }

    /// Get the current debug state. Queries variables if stopped.
    pub async fn state(&mut self) -> Result<DebugState> {
        let is_stopped = self.session.is_stopped().await;
        let status = if is_stopped {
            SessionStatus::Paused
        } else {
            SessionStatus::Running
        };

        let mut variables = Vec::new();
        if is_stopped {
            // Get variables for frame 0
            if let Ok(vars_json) = self.session.get_variables(0).await {
                variables = parse_bc_variables(&vars_json);
            }
        }

        Ok(DebugState {
            status,
            session_id: self.session.connection_id.clone(),
            location: self.history.back().map(|h| h.location.clone()),
            stack: Vec::new(), // BC doesn't expose a full stack via SignalR the same way
            variables,
            thread_id: Some(1),
        })
    }

    /// Evaluate an expression in the current frame.
    pub async fn eval(&self, expr: &str) -> Result<EvalResult> {
        let result = self.session.evaluate(0, expr).await?;

        let value = result
            .get("Value")
            .or_else(|| result.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let type_name = result
            .get("TypeName")
            .or_else(|| result.get("typeName"))
            .or_else(|| result.get("Type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(EvalResult {
            result: value,
            type_name,
        })
    }

    /// Continue execution after a breakpoint (BreakpointExitReason=0).
    pub async fn continue_exec(&mut self) -> Result<DebugState> {
        self.session
            .continue_execution(serde_json::json!(0))
            .await?;
        // Return running state immediately — next state() call will show updated position
        Ok(DebugState {
            status: SessionStatus::Running,
            session_id: self.session.connection_id.clone(),
            location: None,
            stack: Vec::new(),
            variables: Vec::new(),
            thread_id: Some(1),
        })
    }

    /// Step (over/in/out).
    ///
    /// BC's `SetBreakpointResponse` controls step type via BreakpointExitReason:
    /// 0=Continue, 1=StepOver, 2=StepIn, 3=StepOut.
    pub async fn step(&mut self, step_type: &str) -> Result<DebugState> {
        match step_type {
            "in" => self.session.step_in().await?,
            "out" => self.session.step_out().await?,
            _ => self.session.step_over().await?,
        }
        Ok(DebugState {
            status: SessionStatus::Running,
            session_id: self.session.connection_id.clone(),
            location: None,
            stack: Vec::new(),
            variables: Vec::new(),
            thread_id: Some(1),
        })
    }

    /// Get breakpoint hit history, optionally filtered by variable name.
    pub fn history(&self, var_filter: Option<&str>) -> Vec<&BreakpointHit> {
        match var_filter {
            Some(filter) => self
                .history
                .iter()
                .filter(|h| {
                    h.variables
                        .iter()
                        .any(|v| v.name.eq_ignore_ascii_case(filter))
                })
                .collect(),
            None => self.history.iter().collect(),
        }
    }

    /// Stop the debug session and disconnect.
    pub async fn stop(&mut self) -> Result<()> {
        let _ = self.session.stop_debugging().await;
        let _ = self.session.terminate().await;
        info!("Native debug session stopped");
        Ok(())
    }
}

/// Build a `BreakpointInfo` from the common fields, normalising empty conditions to `None`.
fn make_bp_info(file: &str, line: u32, condition: &str, id: i64, verified: bool) -> BreakpointInfo {
    BreakpointInfo {
        id,
        file: file.to_string(),
        line,
        condition: if condition.is_empty() {
            None
        } else {
            Some(condition.to_string())
        },
        verified,
    }
}

/// Parse BC's `LocalNode[]` JSON into our `Variable` type.
fn parse_bc_variables(json: &serde_json::Value) -> Vec<Variable> {
    let arr = match json.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .filter_map(|node| {
            let name = node
                .get("Name")
                .or_else(|| node.get("name"))
                .and_then(|v| v.as_str())?
                .to_string();
            let value = node
                .get("Value")
                .or_else(|| node.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let type_name = node
                .get("TypeName")
                .or_else(|| node.get("typeName"))
                .or_else(|| node.get("Type"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(Variable {
                name,
                value,
                type_name,
                fields: Vec::new(),
            })
        })
        .collect()
}
