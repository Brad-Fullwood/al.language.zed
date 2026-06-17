//! Native debug session wrapper for CLI/daemon use.
//!
//! Wraps `BcDebugSession` with state the daemon needs that BC doesn't track:
//! breakpoint history, file→breakpoint-ID mapping, and hit counting.

use std::collections::{HashMap, VecDeque};

use crate::dap::bc_debug::{BcDebugConfig, BcDebugSession};
use crate::dap::types::*;
use crate::dap::Result;
use tracing::{info, warn};

/// Upper bound on the in-memory Break-event history. Long-running debug
/// sessions over hot loops can accumulate thousands of hits per minute;
/// without a cap, `history` grows unboundedly and the daemon's memory
/// climbs with it. Oldest entries are evicted first (FIFO), preserving
/// recent hits which are by far the most useful for the user.
const HISTORY_CAP: usize = 10_000;

pub struct NativeDebugSession {
    pub session: BcDebugSession,
    pub config: BcDebugConfig,
    breakpoints: HashMap<String, Vec<i64>>,
    history: VecDeque<BreakpointHit>,
}

/// Append a Break event to history with the configured cap. Pure (no `self`
/// dependency) so it unit-tests without standing up a `BcDebugSession`.
fn push_with_cap(history: &mut VecDeque<BreakpointHit>, hit: BreakpointHit, cap: usize) {
    history.push_back(hit);
    while history.len() > cap {
        history.pop_front();
    }
}

impl NativeDebugSession {
    pub async fn start(config: BcDebugConfig, access_token: &str) -> Result<Self> {
        let session = BcDebugSession::connect(&config, access_token).await?;

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

    pub fn session_id(&self) -> &str {
        &self.session.connection_id
    }

    pub async fn set_breakpoints(
        &mut self,
        file: &str,
        lines: &[(u32, Option<&str>)],
        object_type: i32,
        object_id: i32,
    ) -> Result<Vec<BreakpointInfo>> {
        if let Some(old_ids) = self.breakpoints.remove(file) {
            for bp_id in &old_ids {
                if let Err(e) = self.session.remove_breakpoint(*bp_id).await {
                    warn!(bp_id, error = %e, "Failed to remove old breakpoint");
                }
            }
        }

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
        let pending = self.session.flush_pending_events().await;
        let pushed = self.session.try_drain_push_events().await;
        let mut next_seq = self.history.back().map(|h| h.seq + 1).unwrap_or(1);
        for event in pending.into_iter().chain(pushed) {
            if let crate::dap::bc_debug::BcEvent::Break {
                reason, location, ..
            } = event
            {
                // `file` stays empty; the daemon doesn't resolve BC object ids to workspace file paths.
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
                push_with_cap(
                    &mut self.history,
                    BreakpointHit {
                        seq: next_seq,
                        breakpoint_id: 0,
                        timestamp: format_event_timestamp(std::time::SystemTime::now()),
                        location,
                        variables: Vec::new(),
                    },
                    HISTORY_CAP,
                );
                next_seq += 1;
                tracing::debug!(reason = %reason, "native_debug: recorded Break event in history");
            }
        }
    }

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
            if let Ok(vars_json) = self.session.get_variables(0).await {
                variables = parse_bc_variables(&vars_json);
            }
        }

        Ok(DebugState {
            status,
            session_id: self.session.connection_id.clone(),
            location: self.history.back().map(|h| h.location.clone()),
            stack: Vec::new(), // BC doesn't expose a full stack via SignalR
            variables,
            thread_id: Some(1),
        })
    }

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
        self.drain_events().await;
        self.session
            .continue_execution(serde_json::json!(0))
            .await?;
        Ok(DebugState {
            status: SessionStatus::Running,
            session_id: self.session.connection_id.clone(),
            location: None,
            stack: Vec::new(),
            variables: Vec::new(),
            thread_id: Some(1),
        })
    }

    /// BC's `SetBreakpointResponse` controls step type via BreakpointExitReason:
    /// 0=Continue, 1=StepOver, 2=StepIn, 3=StepOut.
    pub async fn step(&mut self, step_type: &str) -> Result<DebugState> {
        self.drain_events().await;
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

#[cfg(test)]
impl NativeDebugSession {
    /// Test-only: wrap an already-constructed (fake) `BcDebugSession` without
    /// running the real connect / attach / configuration_done network handshake
    /// that [`NativeDebugSession::start`] performs. Uses the identical field
    /// initialisers `start` does, so it introduces no new runtime behaviour and
    /// is compiled only under `#[cfg(test)]`.
    fn from_parts(session: BcDebugSession, config: BcDebugConfig) -> Self {
        Self {
            session,
            config,
            breakpoints: HashMap::new(),
            history: VecDeque::new(),
        }
    }

    /// Test-only: pre-populate the Break-event history so the pure `history()`
    /// filter / ordering logic can be exercised directly. `drain_events`
    /// records hits with empty `variables`, so the variable-name filter branch
    /// is otherwise unreachable from a real Break event. Goes through the same
    /// `push_with_cap` the production path uses.
    fn seed_history(&mut self, hits: impl IntoIterator<Item = BreakpointHit>) {
        for h in hits {
            push_with_cap(&mut self.history, h, HISTORY_CAP);
        }
    }
}

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

    /// Exact-value tests. The original suite only pinned the ISO-8601 *shape*
    /// (out of caution that the hand-rolled calendar math might be off-by-one).
    /// It isn't — these values were cross-checked against Python's `datetime`.
    /// Pinning the real rendered string is a far stronger guard: it catches a
    /// silently wrong date (e.g. an off-by-one in `days_to_civil` or a broken
    /// h/m/s split) that the shape test would wave through.
    #[test]
    fn known_seconds_render_exact_date_and_time() {
        // Non-zero hour/min/sec exercises the full hms decomposition, not just
        // the all-zero epoch case.
        let t = UNIX_EPOCH + Duration::from_secs(1_778_160_318);
        assert_eq!(format_event_timestamp(t), "2026-05-07T13:25:18Z");
    }

    #[test]
    fn second_day_after_epoch() {
        // Boundary: exactly one day past epoch must roll the day, not the month.
        let t = UNIX_EPOCH + Duration::from_secs(86_400);
        assert_eq!(format_event_timestamp(t), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn leap_day_2000_renders_feb_29() {
        // Year 2000 is divisible by 400 → a leap year → Feb 29 exists. This is
        // the classic case the Gregorian "century rule" gets wrong; it forces
        // the `m <= 2 ? y+1` year-correction branch in days_to_civil.
        let t = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(format_event_timestamp(t), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn non_leap_century_2100_has_no_feb_29() {
        // Year 2100 is divisible by 100 but NOT 400 → NOT a leap year. The day
        // that would be "Feb 29" must render as "Feb 28". A naive leap rule
        // (every 4 years) would render 02-29 here — this test catches that.
        let t = UNIX_EPOCH + Duration::from_secs(4_107_456_000);
        assert_eq!(format_event_timestamp(t), "2100-02-28T00:00:00Z");
    }

    #[test]
    fn march_first_crosses_month_boundary() {
        // March 1 is where Hinnant's "year starts in March" internal calendar
        // wraps back to the real January. Pins the month-rollover edge.
        let t = UNIX_EPOCH + Duration::from_secs(1_583_020_800);
        assert_eq!(format_event_timestamp(t), "2020-03-01T00:00:00Z");
    }
}

#[cfg(test)]
mod days_to_civil_tests {
    use super::days_to_civil;

    // `days_to_civil` takes days since 0000-03-01 (Hinnant's epoch). The
    // Unix-epoch offset is 719_468, so `719_468 + n` is "n days after
    // 1970-01-01". Testing the helper directly nails down the month-index
    // remap (`mp < 10 ? mp+3 : mp-9`) and the year correction independently of
    // the timestamp formatter.
    const UNIX_OFFSET: i64 = 719_468;

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(days_to_civil(UNIX_OFFSET), (1970, 1, 1));
    }

    #[test]
    fn january_uses_high_month_index_branch() {
        // Jan/Feb come from mp >= 10 (the `mp - 9` branch) because Hinnant's
        // internal year starts in March. Jan 31 1970 is 30 days after epoch.
        assert_eq!(days_to_civil(UNIX_OFFSET + 30), (1970, 1, 31));
    }

    #[test]
    fn december_uses_low_month_index_branch() {
        // December comes from mp < 10 (the `mp + 3` branch). Dec 31 1970 is
        // day 364 (1970 is not a leap year).
        assert_eq!(days_to_civil(UNIX_OFFSET + 364), (1970, 12, 31));
    }

    #[test]
    fn handles_pre_epoch_negative_internal_days() {
        // z can be < 0 inside the function's own era math even though the
        // formatter clamps before calling it. Year 0001-01-01 is a deep
        // negative offset and must not panic or misclassify the era.
        // (Cross-checked: 0001-01-01 is 719_162 days before the Hinnant epoch.)
        let (y, m, d) = days_to_civil(UNIX_OFFSET - 719_162);
        assert_eq!((y, m, d), (1, 1, 1));
    }
}

#[cfg(test)]
mod history_cap_tests {
    use super::{push_with_cap, HISTORY_CAP};
    use crate::dap::types::{BreakpointHit, Location};
    use std::collections::VecDeque;

    fn hit(seq: u32) -> BreakpointHit {
        BreakpointHit {
            seq,
            breakpoint_id: 0,
            timestamp: String::new(),
            location: Location {
                file: String::new(),
                line: 0,
                column: 0,
                procedure: None,
            },
            variables: Vec::new(),
        }
    }

    #[test]
    fn under_cap_keeps_everything() {
        let mut h = VecDeque::new();
        for i in 0..5 {
            push_with_cap(&mut h, hit(i), 10);
        }
        assert_eq!(h.len(), 5);
        assert_eq!(h.front().unwrap().seq, 0);
        assert_eq!(h.back().unwrap().seq, 4);
    }

    #[test]
    fn at_cap_evicts_oldest_first() {
        // Positive: at exactly the cap, push evicts the FIFO head and
        // keeps the most recent entries — what a debugging user wants.
        let mut h = VecDeque::new();
        let cap = 3;
        for i in 0..5 {
            push_with_cap(&mut h, hit(i), cap);
        }
        assert_eq!(h.len(), cap);
        let seqs: Vec<_> = h.iter().map(|h| h.seq).collect();
        assert_eq!(seqs, vec![2, 3, 4]);
    }

    #[test]
    fn cap_zero_drops_input() {
        // Edge: a zero cap means "never retain". The push lands and is
        // immediately evicted. Documents the saturating semantics so a
        // future ill-considered config knob can't accidentally smuggle
        // unbounded growth back in via `cap=0`.
        let mut h = VecDeque::new();
        push_with_cap(&mut h, hit(7), 0);
        assert!(h.is_empty());
    }

    #[test]
    fn default_cap_drops_at_documented_bound() {
        // Tripwire: at the live HISTORY_CAP, the oldest hit is evicted on
        // the (cap+1)-th push. Detects "raised the cap to u64::MAX-style"
        // edits as test failures rather than as production memory bugs.
        let mut h = VecDeque::new();
        for i in 0..HISTORY_CAP as u32 {
            push_with_cap(&mut h, hit(i), HISTORY_CAP);
        }
        assert_eq!(h.len(), HISTORY_CAP);
        push_with_cap(&mut h, hit(HISTORY_CAP as u32), HISTORY_CAP);
        assert_eq!(h.len(), HISTORY_CAP);
        assert_eq!(h.front().unwrap().seq, 1);
        assert_eq!(h.back().unwrap().seq, HISTORY_CAP as u32);
    }
}

#[cfg(test)]
mod make_bp_info_tests {
    use super::make_bp_info;

    #[test]
    fn empty_condition_normalises_to_none() {
        // The set_breakpoints path passes `condition.unwrap_or("")`, so an
        // unconditional breakpoint arrives here as "". That must serialise as
        // `condition: None`, not `Some("")` — clients distinguish "no condition"
        // from "empty condition expression".
        let info = make_bp_info("src/Foo.al", 42, "", 7, true);
        assert_eq!(info.condition, None);
        assert_eq!(info.id, 7);
        assert_eq!(info.file, "src/Foo.al");
        assert_eq!(info.line, 42);
        assert!(info.verified);
    }

    #[test]
    fn non_empty_condition_is_preserved() {
        let info = make_bp_info("src/Bar.al", 10, "x > 5", 3, true);
        assert_eq!(info.condition.as_deref(), Some("x > 5"));
    }

    #[test]
    fn failed_breakpoint_records_zero_id_and_unverified() {
        // Error path mirror: when add_breakpoint fails, set_breakpoints builds
        // an info with id=0 / verified=false so the client sees the breakpoint
        // was rejected rather than silently dropping it.
        let info = make_bp_info("src/Baz.al", 1, "", 0, false);
        assert_eq!(info.id, 0);
        assert!(!info.verified);
        assert_eq!(info.condition, None);
    }
}

#[cfg(test)]
mod parse_bc_variables_tests {
    use super::parse_bc_variables;
    use serde_json::json;

    #[test]
    fn parses_pascal_case_local_nodes() {
        // BC's newer servers return PascalCase keys. The happy path: a fully
        // populated LocalNode becomes a Variable with name/value/type.
        let json = json!([
            { "Name": "Customer", "Value": "10000", "TypeName": "Record" }
        ]);
        let vars = parse_bc_variables(&json);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "Customer");
        assert_eq!(vars[0].value, "10000");
        assert_eq!(vars[0].type_name, "Record");
        assert!(vars[0].fields.is_empty());
    }

    #[test]
    fn parses_camel_case_keys() {
        // Older BC servers use camelCase. Both spellings must be accepted so
        // variable inspection works regardless of server version.
        let json = json!([
            { "name": "i", "value": "5", "typeName": "Integer" }
        ]);
        let vars = parse_bc_variables(&json);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "i");
        assert_eq!(vars[0].value, "5");
        assert_eq!(vars[0].type_name, "Integer");
    }

    #[test]
    fn type_falls_back_to_bare_type_key() {
        // The parser tries TypeName, then typeName, then "Type". This locks in
        // that third fallback so a server emitting only "Type" still yields the
        // type instead of an empty string.
        let json = json!([
            { "Name": "amt", "Value": "1.0", "Type": "Decimal" }
        ]);
        let vars = parse_bc_variables(&json);
        assert_eq!(vars[0].type_name, "Decimal");
    }

    #[test]
    fn node_without_name_is_skipped() {
        // Edge: a node missing its Name (the only required key — note the `?`
        // in the filter_map) is dropped entirely rather than producing a
        // nameless variable. The well-formed sibling survives.
        let json = json!([
            { "Value": "orphan" },
            { "Name": "keep", "Value": "v" }
        ]);
        let vars = parse_bc_variables(&json);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "keep");
    }

    #[test]
    fn missing_value_and_type_default_to_empty() {
        // A node with only a Name is still a valid variable — value and type
        // default to "" rather than being dropped.
        let json = json!([{ "Name": "flag" }]);
        let vars = parse_bc_variables(&json);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "flag");
        assert_eq!(vars[0].value, "");
        assert_eq!(vars[0].type_name, "");
    }

    #[test]
    fn non_array_json_yields_empty() {
        // Malformed/unexpected shape: BC sometimes returns an error object or
        // null instead of the LocalNode array. The parser must degrade to an
        // empty Vec, never panic.
        assert!(parse_bc_variables(&json!(null)).is_empty());
        assert!(parse_bc_variables(&json!({ "Name": "notanarray" })).is_empty());
        assert!(parse_bc_variables(&json!("string")).is_empty());
    }

    #[test]
    fn empty_array_yields_no_variables() {
        assert!(parse_bc_variables(&json!([])).is_empty());
    }
}

#[cfg(test)]
mod native_session_tests {
    use super::*;
    use crate::dap::bc_debug::fake::FakeBc;
    use serde_json::json;

    fn session(conn: &str) -> (NativeDebugSession, FakeBc) {
        let (bc, fake) = FakeBc::start(conn);
        (
            NativeDebugSession::from_parts(bc, BcDebugConfig::default()),
            fake,
        )
    }

    fn hit_with_var(seq: u32, var: &str) -> BreakpointHit {
        BreakpointHit {
            seq,
            breakpoint_id: 0,
            timestamp: String::new(),
            location: Location {
                file: String::new(),
                line: seq,
                column: 0,
                procedure: None,
            },
            variables: vec![Variable {
                name: var.to_string(),
                value: String::new(),
                type_name: String::new(),
                fields: Vec::new(),
            }],
        }
    }

    #[tokio::test]
    async fn set_breakpoints_adds_and_parses_pascalcase_response() {
        let (mut nds, fake) = session("c1");
        fake.reply_ok("AddBreakpoint", json!({ "Id": 77, "Verified": true }));

        let infos = nds
            .set_breakpoints("src/Foo.al", &[(10, None)], 5, 50100)
            .await
            .unwrap();

        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, 77);
        assert!(infos[0].verified);
        assert_eq!(infos[0].line, 10);
        assert_eq!(infos[0].file, "src/Foo.al");
        assert_eq!(infos[0].condition, None);

        let frames = fake.sent_frames();
        assert_eq!(frames.len(), 1, "exactly one AddBreakpoint invoke");
        assert_eq!(frames[0]["target"], "AddBreakpoint");
        assert_eq!(frames[0]["arguments"][0]["ObjectType"], 5);
        assert_eq!(frames[0]["arguments"][0]["ObjectNumber"], 50100);
        assert_eq!(frames[0]["arguments"][1]["Line"], 10);
    }

    #[tokio::test]
    async fn set_breakpoints_parses_camelcase_response() {
        // Older BC returns camelCase id/verified. Without the camelCase
        // fallback the id would default to 0 and verified to true.
        let (mut nds, fake) = session("c1");
        fake.reply_ok("AddBreakpoint", json!({ "id": 5, "verified": false }));

        let infos = nds
            .set_breakpoints("src/A.al", &[(3, None)], 1, 18)
            .await
            .unwrap();

        assert_eq!(infos[0].id, 5, "camelCase id parsed");
        assert!(!infos[0].verified, "camelCase verified parsed");
    }

    #[tokio::test]
    async fn set_breakpoints_normalizes_conditions_and_forwards_them() {
        // A real condition is preserved; an empty condition normalises to None
        // but is still forwarded verbatim ("") to BC as the third arg.
        let (mut nds, fake) = session("c1");
        fake.reply_ok("AddBreakpoint", json!({ "Id": 1, "Verified": true }));
        fake.reply_ok("AddBreakpoint", json!({ "Id": 2, "Verified": true }));

        let infos = nds
            .set_breakpoints("src/B.al", &[(1, Some("x > 5")), (2, Some(""))], 5, 99)
            .await
            .unwrap();

        assert_eq!(infos[0].condition.as_deref(), Some("x > 5"));
        assert_eq!(infos[1].condition, None, "empty condition → None");

        let frames = fake.sent_frames();
        assert_eq!(frames[0]["arguments"][2], "x > 5");
        assert_eq!(frames[1]["arguments"][2], "");
    }

    #[tokio::test]
    async fn set_breakpoints_removes_prior_ids_for_same_file() {
        let (mut nds, fake) = session("c1");
        fake.reply_ok("AddBreakpoint", json!({ "Id": 100, "Verified": true }));
        fake.reply_ok("AddBreakpoint", json!({ "Id": 101, "Verified": true }));
        nds.set_breakpoints("src/C.al", &[(1, None), (2, None)], 5, 50)
            .await
            .unwrap();

        fake.reply_ok("RemoveBreakpoint", json!(null));
        fake.reply_ok("RemoveBreakpoint", json!(null));
        fake.reply_ok("AddBreakpoint", json!({ "Id": 200, "Verified": true }));
        nds.set_breakpoints("src/C.al", &[(9, None)], 5, 50)
            .await
            .unwrap();

        let removed: Vec<i64> = fake
            .sent_frames()
            .iter()
            .filter(|f| f["target"] == "RemoveBreakpoint")
            .map(|f| f["arguments"][0].as_i64().unwrap())
            .collect();
        assert_eq!(
            removed,
            vec![100, 101],
            "prior ids removed before re-adding"
        );
    }

    #[tokio::test]
    async fn set_breakpoints_partial_failure_marks_failed_unverified() {
        let (mut nds, fake) = session("c1");
        fake.reply_ok("AddBreakpoint", json!({ "Id": 11, "Verified": true }));
        fake.reply_err("AddBreakpoint", "compilation error");

        let infos = nds
            .set_breakpoints("src/D.al", &[(1, None), (2, None)], 5, 50)
            .await
            .unwrap();

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].id, 11);
        assert!(infos[0].verified);
        assert_eq!(infos[1].id, 0, "failed add → id 0");
        assert!(!infos[1].verified, "failed add → unverified");
    }

    #[tokio::test]
    async fn state_stopped_reports_paused_with_variables_and_location() {
        let (mut nds, fake) = session("sess-7");
        fake.push_callback(
            "Break",
            json!([
                null,
                [{ "DisplayName": "OnRun", "SourcePosition": { "Line": 42, "Column": 8 } }],
                "stopped"
            ]),
        );
        fake.reply_ok(
            "GetVariables",
            json!([{ "Name": "Customer", "Value": "10000", "TypeName": "Record" }]),
        );

        let st = nds.state().await.unwrap();

        assert_eq!(st.status, SessionStatus::Paused);
        assert_eq!(st.session_id, "sess-7");
        assert_eq!(st.variables.len(), 1);
        assert_eq!(st.variables[0].name, "Customer");
        assert_eq!(st.variables[0].type_name, "Record");
        let loc = st.location.expect("location recorded from Break");
        assert_eq!(loc.line, 42);
        assert_eq!(loc.column, 8);
        assert_eq!(loc.procedure.as_deref(), Some("OnRun"));
    }

    #[tokio::test]
    async fn state_running_reports_no_variables_and_no_query() {
        let (mut nds, fake) = session("sess-run");
        let st = nds.state().await.unwrap();

        assert_eq!(st.status, SessionStatus::Running);
        assert!(st.variables.is_empty());
        assert!(st.location.is_none());
        assert!(
            fake.sent_frames()
                .iter()
                .all(|f| f["target"] != "GetVariables"),
            "running state must not query variables"
        );
    }

    #[tokio::test]
    async fn drain_events_records_breaks_with_incrementing_seq() {
        let (mut nds, fake) = session("c1");
        fake.push_callback(
            "Break",
            json!([null, [{ "SourcePosition": { "Line": 1, "Column": 0 } }], ""]),
        );
        fake.push_callback(
            "Break",
            json!([null, [{ "SourcePosition": { "Line": 2, "Column": 0 } }], ""]),
        );
        fake.reply_ok("GetVariables", json!([]));

        nds.state().await.unwrap();

        let hist = nds.history(None);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].seq, 1);
        assert_eq!(hist[1].seq, 2);
        assert_eq!(hist[0].location.line, 1);
        assert_eq!(hist[1].location.line, 2);
    }

    #[tokio::test]
    async fn drain_events_extracts_camelcase_break_location() {
        // BreakLocation extraction must accept camelCase field names from BC.
        let (mut nds, fake) = session("c1");
        fake.push_callback(
            "Break",
            json!([
                null,
                [{ "displayName": "MyProc", "sourcePosition": { "line": 7, "column": 3 } }],
                ""
            ]),
        );
        fake.reply_ok("GetVariables", json!([]));

        let st = nds.state().await.unwrap();
        let loc = st.location.expect("camelCase location extracted");
        assert_eq!(loc.line, 7);
        assert_eq!(loc.column, 3);
        assert_eq!(loc.procedure.as_deref(), Some("MyProc"));
    }

    #[tokio::test]
    async fn drain_events_flushes_events_buffered_during_an_invoke() {
        // A Break that lands while an invoke holds event_rx is buffered into
        // pending_events (not dropped); the next drain flushes it to history.
        let (mut nds, fake) = session("c1");
        fake.push_callback(
            "Break",
            json!([null, [{ "SourcePosition": { "Line": 5, "Column": 1 } }], ""]),
        );
        fake.reply_ok(
            "GetWatchNode",
            json!({ "Value": "1", "TypeName": "Integer" }),
        );

        // eval does not drain — the Break is buffered, history stays empty.
        let _ = nds.eval("x").await.unwrap();
        assert!(
            nds.history(None).is_empty(),
            "eval does not drain pending events"
        );

        fake.reply_ok("GetVariables", json!([]));
        nds.state().await.unwrap();

        let hist = nds.history(None);
        assert_eq!(hist.len(), 1, "buffered Break flushed by next drain");
        assert_eq!(hist[0].location.line, 5);
    }

    #[tokio::test]
    async fn eval_extracts_value_and_type_name() {
        let (nds, fake) = session("c1");
        fake.reply_ok(
            "GetWatchNode",
            json!({ "Value": "10000", "TypeName": "Code[20]" }),
        );

        let r = nds.eval("Customer.\"No.\"").await.unwrap();
        assert_eq!(r.result, "10000");
        assert_eq!(r.type_name, "Code[20]");

        let frames = fake.sent_frames();
        assert_eq!(frames[0]["target"], "GetWatchNode");
        assert_eq!(frames[0]["arguments"][0], 0);
        assert_eq!(frames[0]["arguments"][1], "Customer.\"No.\"");
    }

    #[tokio::test]
    async fn eval_type_name_falls_back_camel_then_bare_type() {
        // type_name fallback chain: TypeName → typeName → Type.
        let (nds, fake) = session("c1");
        fake.reply_ok(
            "GetWatchNode",
            json!({ "value": "1", "typeName": "Integer" }),
        );
        let r = nds.eval("i").await.unwrap();
        assert_eq!(r.result, "1");
        assert_eq!(r.type_name, "Integer", "camelCase typeName fallback");

        let (nds2, fake2) = session("c2");
        fake2.reply_ok("GetWatchNode", json!({ "Value": "2.5", "Type": "Decimal" }));
        let r2 = nds2.eval("amt").await.unwrap();
        assert_eq!(r2.type_name, "Decimal", "bare Type fallback");
    }

    #[tokio::test]
    async fn eval_missing_fields_default_to_empty() {
        let (nds, fake) = session("c1");
        fake.reply_ok("GetWatchNode", json!({}));
        let r = nds.eval("nothing").await.unwrap();
        assert_eq!(r.result, "");
        assert_eq!(r.type_name, "");
    }

    #[tokio::test]
    async fn continue_exec_drains_pending_then_resumes_running() {
        let (mut nds, fake) = session("sess-c");
        fake.push_callback(
            "Break",
            json!([null, [{ "SourcePosition": { "Line": 12, "Column": 0 } }], ""]),
        );
        fake.reply_ok("SetBreakpointResponse", json!(null));

        let st = nds.continue_exec().await.unwrap();
        assert_eq!(st.status, SessionStatus::Running);
        assert_eq!(st.session_id, "sess-c");
        assert!(st.location.is_none());

        let hist = nds.history(None);
        assert_eq!(hist.len(), 1, "F-014: Break recorded before resume");
        assert_eq!(hist[0].location.line, 12);

        let resume = fake
            .sent_frames()
            .into_iter()
            .find(|f| f["target"] == "SetBreakpointResponse")
            .expect("resume invoke sent");
        assert_eq!(resume["arguments"][0], 0, "continue → exit reason 0");
    }

    #[tokio::test]
    async fn step_dispatches_exit_reason_by_type() {
        for (kind, expected) in [("over", 1), ("in", 2), ("out", 3)] {
            let (mut nds, fake) = session("c");
            fake.reply_ok("SetBreakpointResponse", json!(null));
            let st = nds.step(kind).await.unwrap();
            assert_eq!(st.status, SessionStatus::Running, "step {kind} → Running");
            let f = fake
                .sent_frames()
                .into_iter()
                .find(|f| f["target"] == "SetBreakpointResponse")
                .expect("step invoke sent");
            assert_eq!(f["arguments"][0], expected, "step {kind} exit reason");
        }
    }

    #[tokio::test]
    async fn step_unknown_type_defaults_to_step_over() {
        let (mut nds, fake) = session("c");
        fake.reply_ok("SetBreakpointResponse", json!(null));
        nds.step("bogus").await.unwrap();
        let f = fake
            .sent_frames()
            .into_iter()
            .find(|f| f["target"] == "SetBreakpointResponse")
            .expect("step invoke sent");
        assert_eq!(f["arguments"][0], 1, "unknown step type → step-over (1)");
    }

    #[tokio::test]
    async fn step_drains_pending_break_before_advancing() {
        // F-014 symmetry with continue: a pending Break is recorded before step.
        let (mut nds, fake) = session("c");
        fake.push_callback(
            "Break",
            json!([null, [{ "SourcePosition": { "Line": 8, "Column": 0 } }], ""]),
        );
        fake.reply_ok("SetBreakpointResponse", json!(null));

        nds.step("over").await.unwrap();

        let hist = nds.history(None);
        assert_eq!(hist.len(), 1, "F-014: Break recorded before step");
        assert_eq!(hist[0].location.line, 8);
    }

    #[tokio::test]
    async fn history_filters_by_variable_name_case_insensitive() {
        let (mut nds, _fake) = session("c");
        nds.seed_history([
            hit_with_var(1, "Customer"),
            hit_with_var(2, "Vendor"),
            hit_with_var(3, "customer"),
        ]);
        let filtered = nds.history(Some("CUSTOMER"));
        assert_eq!(filtered.len(), 2, "case-insensitive match");
        assert_eq!(filtered[0].seq, 1);
        assert_eq!(filtered[1].seq, 3);
    }

    #[tokio::test]
    async fn history_unfiltered_returns_fifo_order() {
        let (mut nds, _fake) = session("c");
        nds.seed_history([
            hit_with_var(10, "a"),
            hit_with_var(20, "b"),
            hit_with_var(30, "c"),
        ]);
        let seqs: Vec<u32> = nds.history(None).iter().map(|h| h.seq).collect();
        assert_eq!(seqs, vec![10, 20, 30]);
    }

    #[tokio::test]
    async fn history_empty_returns_empty() {
        let (nds, _fake) = session("c");
        assert!(nds.history(None).is_empty());
        assert!(nds.history(Some("anything")).is_empty());
    }

    #[tokio::test]
    async fn stop_invokes_stop_debugging_then_terminate() {
        let (mut nds, fake) = session("c");
        fake.reply_ok("StopDebugging", json!(null));
        fake.reply_ok("TerminateSession", json!(null));

        nds.stop().await.unwrap();

        let targets: Vec<String> = fake
            .sent_frames()
            .iter()
            .map(|f| f["target"].as_str().unwrap_or("").to_string())
            .collect();
        let stop_idx = targets
            .iter()
            .position(|t| t == "StopDebugging")
            .expect("StopDebugging invoked");
        let term_idx = targets
            .iter()
            .position(|t| t == "TerminateSession")
            .expect("TerminateSession invoked");
        assert!(stop_idx < term_idx, "stop_debugging before terminate");
    }

    #[tokio::test]
    async fn stop_tolerates_errors_and_still_attempts_both_teardowns() {
        let (mut nds, fake) = session("c");
        fake.reply_err("StopDebugging", "already gone");
        fake.reply_err("TerminateSession", "no session");

        nds.stop().await.expect("stop tolerates teardown errors");

        let targets: Vec<String> = fake
            .sent_frames()
            .iter()
            .map(|f| f["target"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(targets.iter().any(|t| t == "StopDebugging"));
        assert!(
            targets.iter().any(|t| t == "TerminateSession"),
            "terminate attempted even after stop_debugging errored"
        );
    }
}
