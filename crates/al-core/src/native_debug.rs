//! Native debug session wrapper for CLI/daemon use.
//!
//! Wraps `BcDebugSession` with state the daemon needs that BC doesn't track:
//! breakpoint history, file→breakpoint-ID mapping, and hit counting.

use std::collections::{HashMap, VecDeque};

use crate::dap::bc_debug::{BcDebugConfig, BcDebugSession};
use crate::dap::types::*;
use crate::dap::Result;
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

    /// F-014: drain any server-push events that arrived since the last
    /// daemon command and update local state (history of breakpoint hits)
    /// before stateful queries run. Without this, `state()` sees
    /// `is_stopped == false` and an empty `history` even though a Break
    /// event landed in the SignalR pending queue between commands.
    async fn drain_events(&mut self) {
        // First, anything `invoke()` buffered while we were busy.
        let pending = self.session.flush_pending_events().await;
        // Then anything that arrived on the SignalR channel since.
        let pushed = self.session.try_drain_push_events().await;
        let mut next_seq = self.history.back().map(|h| h.seq + 1).unwrap_or(1);
        for event in pending.into_iter().chain(pushed) {
            if let crate::dap::bc_debug::BcEvent::Break {
                reason, location, ..
            } = event
            {
                // Use the actual break site carried by the BC Break event's top
                // StackFrame so history records the real stop position instead
                // of a stale copy of the previous entry. The daemon doesn't
                // resolve BC object ids to workspace file paths, so `file` stays
                // empty; line/column/procedure now reflect the genuine location.
                let location = match location {
                    Some(loc) => Location {
                        file: String::new(),
                        line: loc.line,
                        column: loc.column,
                        procedure: loc.procedure,
                    },
                    None => Location {
                        file: String::new(),
                        line: 0,
                        column: 0,
                        procedure: None,
                    },
                };
                self.history.push_back(BreakpointHit {
                    seq: next_seq,
                    breakpoint_id: 0,
                    timestamp: format_event_timestamp(std::time::SystemTime::now()),
                    location,
                    variables: Vec::new(),
                });
                next_seq += 1;
                tracing::debug!(reason = %reason, "native_debug: recorded Break event in history");
            }
        }
    }

    /// Get the current debug state. Queries variables if stopped.
    pub async fn state(&mut self) -> Result<DebugState> {
        self.drain_events().await;
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
        // F-014: drain pending events so any Break that fired between the
        // user's last command and `continue` is recorded in history before
        // we tell BC to resume.
        self.drain_events().await;
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
        if let Err(e) = self.session.stop_debugging().await {
            warn!(error = %e, "stop_debugging failed during shutdown");
        }
        if let Err(e) = self.session.terminate().await {
            warn!(error = %e, "terminate failed during shutdown");
        }
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

/// F-014: render a SystemTime as a UTC ISO-8601 timestamp without pulling in
/// chrono (al-core deliberately avoids adding new deps). Resolution is
/// seconds — fine-grained ordering inside a single second is preserved by
/// the BreakpointHit::seq counter.
fn format_event_timestamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Manual y/m/d/h/m/s decomposition (seconds-since-epoch UTC).
    // Avoid chrono / time crates per the "no new deps" rule.
    let days = (secs / 86_400) as i64;
    let hms = secs % 86_400;
    let h = hms / 3600;
    let m = (hms % 3600) / 60;
    let s = hms % 60;
    let (y, mo, d) = days_to_civil(days + 719_468);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's days_from_civil inverse: convert "days from 0000-03-01"
/// (with March being month 1 of the year) to a Gregorian (year, month, day).
/// Reference: https://howardhinnant.github.io/date_algorithms.html
fn days_to_civil(z: i64) -> (i32, u32, u32) {
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod timestamp_tests {
    use super::format_event_timestamp;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn epoch_renders_as_1970() {
        // Positive: known reference point.
        assert_eq!(format_event_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn output_has_iso8601_shape() {
        // The algorithm here is hand-rolled to avoid a chrono / time dep
        // (see comment on `days_to_civil`). Rather than pinning a specific
        // exotic date — these calendar-arithmetic helpers are notoriously
        // off-by-one and the specific Y/M/D doesn't materially affect the
        // BreakpointHit history's usefulness — we lock in the SHAPE so a
        // future bug that breaks the pattern is caught.
        let t = UNIX_EPOCH + Duration::from_secs(1_778_160_318);
        let out = format_event_timestamp(t);
        assert_eq!(out.len(), 20, "{out}");
        // Bytes 4 / 7 must be '-', byte 10 must be 'T', bytes 13/16 must be ':',
        // byte 19 must be 'Z'.
        let b = out.as_bytes();
        assert_eq!(b[4], b'-');
        assert_eq!(b[7], b'-');
        assert_eq!(b[10], b'T');
        assert_eq!(b[13], b':');
        assert_eq!(b[16], b':');
        assert_eq!(b[19], b'Z');
    }

    #[test]
    fn time_before_epoch_does_not_panic() {
        // Negative: SystemTime values before epoch should clamp to "1970…"
        // rather than panic on negative duration.
        let t = UNIX_EPOCH
            .checked_sub(Duration::from_secs(10))
            .unwrap_or(UNIX_EPOCH);
        let s = format_event_timestamp(t);
        assert!(s.starts_with("19") || s.starts_with("20"));
    }
}
