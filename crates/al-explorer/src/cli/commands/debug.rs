//! `al-explorer debug`, `snapshot` and `profile` subcommands: the CLI front
//! for the daemon's debug-session, snapshot and profiling methods.

use std::fmt::Write as _;
use std::process::ExitCode;

use serde_json::Value;

use super::super::{DebugCommands, ProfileCommands, SnapshotCommands};

use super::{
    absolutize_path, bc_server_params, connect, file_to_uri, print_json, report_error,
    request_checked, terminal_lines, terminal_text, text_field,
};

pub fn cmd_debug(subcmd: &DebugCommands, json: bool) -> ExitCode {
    match subcmd {
        DebugCommands::Start { config } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            client.set_request_timeout(std::time::Duration::from_secs(120));
            let mut params = serde_json::json!({ "cmd": "start" });
            if let Some(config) = config {
                params["config"] = serde_json::Value::String(config.clone());
            }
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session = text_field(&result, "session", "?");
                        let status = text_field(&result, "status", "?");
                        println!("Debug session started: {session} (status: {status})");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Breakpoint {
            file,
            line,
            condition,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let abs_file = match file_to_uri(file) {
                Ok(uri) => uri,
                Err(error) => return report_error(&error, json),
            };
            let mut params = serde_json::json!({
                "cmd": "breakpoint",
                "file": abs_file,
                "line": line,
            });
            if let Some(condition) = condition {
                params["condition"] = serde_json::Value::String(condition.clone());
            }
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let bps = result.get("breakpoints").and_then(|v| v.as_array());
                        if let Some(bps) = bps {
                            for bp in bps {
                                let bp_line = bp.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                                let verified = bp
                                    .get("verified")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let status = if verified { "verified" } else { "unverified" };
                                println!("Breakpoint at line {bp_line}: {status}");
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::State => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "state"});
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        print!("{}", debug_state_text(&result));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Eval { expr } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "eval", "expr": expr});
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        println!("{}", debug_eval_line(&result));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Continue => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "continue"});
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status = text_field(&result, "status", "?");
                        println!("Continued. Status: {status}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Step { step_type } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "step", "stepType": step_type});
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        print!("{}", debug_step_text(&result));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::History { var } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = serde_json::json!({"cmd": "history"});
            if let Some(var) = var {
                params["var"] = serde_json::Value::String(var.clone());
            }
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let hits = result.get("hits").and_then(|v| v.as_array());
                        if let Some(hits) = hits {
                            if hits.is_empty() {
                                println!("No breakpoint hits recorded.");
                            } else {
                                for hit in hits {
                                    let seq = hit.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let ts = text_field(hit, "timestamp", "?");
                                    println!("Hit #{seq} at {ts}");
                                }
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Stop => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "stop"});
            match request_checked(&mut client, "debug", Some(params)) {
                Ok(result) => {
                    let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    let stopped = debug_stop_actually_stopped(status);
                    if json {
                        print_json(&result);
                    } else if stopped {
                        println!("Debug session stopped.");
                    } else {
                        println!("Debug stop: {}.", terminal_text(status));
                    }
                    if stopped {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
                    }
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}

/// The text `debug state` prints: the session, where it stopped and each
/// variable. Names and values come from the BC debugger.
fn debug_state_text(result: &Value) -> String {
    let mut out = String::new();
    let status = text_field(result, "status", "?");
    let session_id = text_field(result, "sessionId", "?");
    let _ = writeln!(out, "Session {session_id}: {status}");
    if let Some(loc) = result.get("location") {
        let file = text_field(loc, "file", "?");
        let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let proc = text_field(loc, "procedure", "");
        let _ = writeln!(out, "  at {file}:{line} ({proc})");
    }
    if let Some(vars) = result.get("variables").and_then(|v| v.as_array()) {
        let _ = writeln!(out, "  Variables:");
        for var in vars {
            let name = text_field(var, "name", "?");
            let val = text_field(var, "value", "?");
            let ty = text_field(var, "typeName", "?");
            let _ = writeln!(out, "    {name}: {ty} = {val}");
        }
    }
    out
}

/// The line `debug eval` prints: the value and its type.
fn debug_eval_line(result: &Value) -> String {
    let val = text_field(result, "result", "?");
    let ty = text_field(result, "typeName", "?");
    format!("{val} ({ty})")
}

/// The text `debug step` prints: the status and the new location.
fn debug_step_text(result: &Value) -> String {
    let mut out = String::new();
    let status = text_field(result, "status", "?");
    let _ = writeln!(out, "Stepped. Status: {status}");
    if let Some(loc) = result.get("location") {
        let file = text_field(loc, "file", "?");
        let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let _ = writeln!(out, "  at {file}:{line}");
    }
    out
}

/// The table `profile analyze` prints for the profile's hotspots.
fn hotspots_text(spots: &[Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:>8}  {:>8}  {:>8}  PROCEDURE",
        "SELF(ms)", "TOTAL(ms)", "HITS"
    );
    let _ = writeln!(out, "{}", "-".repeat(80));
    for h in spots {
        let proc = text_field(h, "procedure", "?");
        let self_ms = h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let total_ms = h.get("totalTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let hits = h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
        let label = match h.get("object").and_then(|v| v.as_str()) {
            Some(object) => format!("{proc} ({})", terminal_text(object)),
            None => proc,
        };
        let _ = writeln!(out, "{self_ms:>8.1}  {total_ms:>8.1}  {hits:>8}  {label}");
    }
    out
}

/// Whether a `debug stop` response's `status` field means a session was
/// actually stopped, versus e.g. "no active debug session" — a request the
/// daemon reports as `Ok` (not a JSON-RPC error) even though there was
/// nothing to stop. Per `Docs/reference/cli-commands.md`'s exit contract
/// ("0 means the requested gate passed"), only the former should exit 0.
fn debug_stop_actually_stopped(status: &str) -> bool {
    status == "stopped"
}

fn warning_lines(result: &Value) -> String {
    let mut out = String::new();
    for warning in result
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let _ = writeln!(out, "Warning: {}", terminal_text(warning));
    }
    out
}

/// What `snapshot start` prints.
fn snapshot_start_text(result: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Snapshot session started: {} ({})",
        text_field(result, "debuggingContext", "?"),
        text_field(result, "attachKind", "?")
    );
    for point in result
        .get("snappoints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let line = point.get("line").and_then(Value::as_u64).unwrap_or(0);
        let id = point.get("id").and_then(Value::as_i64).unwrap_or(0);
        let condition = match point.get("condition").and_then(Value::as_str) {
            Some(condition) if !condition.is_empty() => {
                format!(" if {}", terminal_text(condition))
            }
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "  {} {id} line {line}{condition} ({})",
            text_field(point, "objectType", "?"),
            text_field(point, "origin", "?")
        );
    }
    out.push_str(&warning_lines(result));
    out
}

/// What `snapshot status` prints.
fn snapshot_status_text(result: &Value) -> String {
    let seconds = result
        .get("secondsSinceStarted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = format!(
        "{}: {} ({seconds}s since started)\n",
        text_field(result, "debuggingContext", "?"),
        text_field(result, "status", "?")
    );
    out.push_str(&warning_lines(result));
    out
}

/// What `snapshot finish` prints.
fn snapshot_finish_text(result: &Value) -> String {
    let mut out = String::new();
    if result.get("empty").and_then(Value::as_bool) == Some(true) {
        let _ = writeln!(out, "{}", text_field(result, "message", "Empty snapshot."));
    } else {
        let bytes = result.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        let _ = writeln!(
            out,
            "Snapshot saved: {} ({bytes} bytes)",
            text_field(result, "zipPath", "?")
        );
        if let Some(profile) = result.get("cpuProfilePath").and_then(Value::as_str) {
            let _ = writeln!(out, "CPU profile: {}", terminal_text(profile));
        }
    }
    out.push_str(&warning_lines(result));
    out
}

/// The table `snapshot list` prints for the stored sessions.
fn snapshot_sessions_text(sessions: &[Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<38} {:<12} {:<28} {:<10} ZIP",
        "CONTEXT", "STATUS", "ENVIRONMENT", "STARTED"
    );
    for session in sessions {
        let started = session
            .get("createdAt")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let environment = match session.get("environmentName").and_then(Value::as_str) {
            Some(name) => terminal_text(name),
            None => text_field(session, "environmentType", "?"),
        };
        let _ = writeln!(
            out,
            "{:<38} {:<12} {:<28} {:<10} {}",
            text_field(session, "debuggingContext", "?"),
            text_field(session, "lastStatus", "-"),
            environment,
            started,
            text_field(session, "zipPath", "-")
        );
    }
    out
}

fn snapshot_params(cmd: &str, pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut params = serde_json::json!({ "cmd": cmd });
    for (key, value) in pairs {
        if let Some(value) = value {
            params[key] = value;
        }
    }
    params
}

/// The daemon `snapshot` request each `snapshot` subcommand sends.
fn snapshot_request(subcmd: &SnapshotCommands) -> Result<Value, String> {
    Ok(match subcmd {
        SnapshotCommands::Start {
            config,
            snap,
            break_on_next,
            user,
            session_id,
            verbosity,
            allow_production,
        } => snapshot_params(
            "start",
            vec![
                ("config", Some(serde_json::json!(config))),
                ("snaps", Some(serde_json::json!(snap))),
                ("breakOnNext", Some(serde_json::json!(break_on_next))),
                ("userId", user.as_ref().map(|u| serde_json::json!(u))),
                ("sessionId", session_id.map(|id| serde_json::json!(id))),
                (
                    "verbosity",
                    verbosity.as_ref().map(|v| serde_json::json!(v)),
                ),
                (
                    "allowProduction",
                    allow_production.then(|| serde_json::json!(true)),
                ),
            ],
        ),
        SnapshotCommands::Status { context } => snapshot_params(
            "status",
            vec![("context", context.as_ref().map(|c| serde_json::json!(c)))],
        ),
        SnapshotCommands::Finish { context, out } => {
            let out_dir = match out {
                Some(dir) => Some(serde_json::json!(absolutize_path(dir)?)),
                None => None,
            };
            snapshot_params(
                "finish",
                vec![
                    ("context", context.as_ref().map(|c| serde_json::json!(c))),
                    ("outputDir", out_dir),
                ],
            )
        }
        SnapshotCommands::List => snapshot_params("list", Vec::new()),
        SnapshotCommands::Show {
            zip,
            vars,
            fields,
            stop,
        } => snapshot_params(
            "show",
            vec![
                ("path", Some(serde_json::json!(absolutize_path(zip)?))),
                ("vars", vars.as_ref().map(|v| serde_json::json!(v))),
                ("fields", fields.as_ref().map(|f| serde_json::json!(f))),
                ("stop", stop.map(|n| serde_json::json!(n))),
            ],
        ),
    })
}

pub fn cmd_snapshot(subcmd: &SnapshotCommands, json: bool) -> ExitCode {
    let mut params = match snapshot_request(subcmd) {
        Ok(params) => params,
        Err(error) => return report_error(&error, json),
    };
    if json {
        if let SnapshotCommands::Show { .. } = subcmd {
            params["json"] = serde_json::json!(true);
        }
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(&mut client, "snapshot", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            match subcmd {
                SnapshotCommands::Start { .. } => print!("{}", snapshot_start_text(&result)),
                SnapshotCommands::Status { .. } => print!("{}", snapshot_status_text(&result)),
                SnapshotCommands::Finish { .. } => print!("{}", snapshot_finish_text(&result)),
                SnapshotCommands::List => {
                    let sessions = result
                        .get("sessions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if sessions.is_empty() {
                        eprintln!("No snapshot sessions on this machine.");
                    } else {
                        print!("{}", snapshot_sessions_text(&sessions));
                    }
                }
                SnapshotCommands::Show { .. } => {
                    let text = text_field_raw(&result, "text");
                    println!("{}", terminal_lines(&text));
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The string at `key` without escaping, for text escaped line by line at the print site.
fn text_field_raw(result: &Value, key: &str) -> String {
    result
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn cmd_profile(subcmd: &ProfileCommands, json: bool) -> ExitCode {
    match subcmd {
        ProfileCommands::Start {
            server,
            company,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = match bc_server_params(
                "start",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            ) {
                Ok(params) => params,
                Err(error) => return report_error(&error, json),
            };
            match request_checked(&mut client, "profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session_id = text_field(&result, "sessionId", "?");
                        let status = text_field(&result, "status", "?");
                        println!("Profiling started: session={session_id} ({status})");
                        eprintln!("Run `al profile stop --session-id {session_id}` when done.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        ProfileCommands::Stop {
            session_id,
            server,
            company,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = match bc_server_params(
                "stop",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            ) {
                Ok(params) => params,
                Err(error) => return report_error(&error, json),
            };
            if let Some(sid) = session_id {
                params["sessionId"] = serde_json::json!(sid);
            }
            match request_checked(&mut client, "profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path = text_field(&result, "path", "?");
                        let status = text_field(&result, "status", "?");
                        println!("Profiling {status}. Profile saved: {path}");
                        eprintln!("Analyze with: al profile analyze {path}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        ProfileCommands::Analyze { path, top } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let path = match absolutize_path(path) {
                Ok(path) => path,
                Err(error) => return report_error(&error, json),
            };
            let params = serde_json::json!({
                "cmd": "analyze",
                "path": path,
                "topN": top,
            });
            match request_checked(&mut client, "profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let duration = result
                            .get("durationMs")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        println!("Profile duration: {duration:.1}ms");
                        println!();
                        let hotspots = result.get("hotspots").and_then(|v| v.as_array());
                        if let Some(spots) = hotspots {
                            if spots.is_empty() {
                                eprintln!("No hotspots found in profile.");
                            } else {
                                print!("{}", hotspots_text(spots));
                                eprintln!("\n{} hotspot(s) shown", spots.len());
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}

#[cfg(test)]
mod debug_stop_exit_code_tests {
    use super::debug_stop_actually_stopped;

    #[test]
    fn stopped_status_is_success() {
        assert!(debug_stop_actually_stopped("stopped"));
    }

    #[test]
    fn no_active_session_status_is_not_success() {
        assert!(!debug_stop_actually_stopped("no active debug session"));
    }

    #[test]
    fn unknown_status_is_not_success() {
        assert!(!debug_stop_actually_stopped("?"));
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{
        debug_eval_line, debug_state_text, debug_step_text, hotspots_text, snapshot_sessions_text,
        snapshot_start_text,
    };

    const COLOURED: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const TITLE: &str = "Sec7 Caller\u{1b}]0;pwned\u{7}";
    const CLEAR: &str = "Sec7 Tests\u{1b}[2J";

    fn assert_escaped(text: &str) {
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
        assert!(text.contains(r"Sec7 Tests\u{1b}[2J"), "got: {text}");
    }

    #[test]
    fn debugger_frames_variables_and_values_print_escaped() {
        let state = serde_json::json!({
            "status": "paused", "sessionId": "s1",
            "location": {"file": CLEAR, "line": 3, "procedure": COLOURED},
            "variables": [{"name": COLOURED, "value": TITLE, "typeName": CLEAR}]
        });
        let text = debug_state_text(&state);
        assert_escaped(&text);
        assert!(
            text.contains(
                r"    Bad\u{1b}[31m Name\u{1b}[0m: Sec7 Tests\u{1b}[2J = Sec7 Caller\u{1b}]0;pwned\u{7}"
            ),
            "got: {text}"
        );

        let eval = serde_json::json!({"result": CLEAR, "typeName": TITLE});
        assert_escaped(&debug_eval_line(&eval));

        let step = serde_json::json!({"status": COLOURED, "location": {"file": CLEAR, "line": 9}});
        assert_eq!(
            debug_step_text(&step),
            "Stepped. Status: Bad\\u{1b}[31m Name\\u{1b}[0m\n  at Sec7 Tests\\u{1b}[2J:9\n"
        );
    }

    #[test]
    fn snapshot_ids_and_profile_procedures_print_escaped() {
        let sessions = [serde_json::json!({
            "debuggingContext": CLEAR, "lastStatus": TITLE, "createdAt": 10
        })];
        assert_escaped(&snapshot_sessions_text(&sessions));
        let started = serde_json::json!({
            "debuggingContext": "c", "attachKind": "UserSession",
            "snappoints": [{"objectType": "Codeunit", "id": 1, "line": 2,
                "condition": CLEAR, "origin": "object id"}],
            "warnings": [CLEAR]
        });
        let text = snapshot_start_text(&started);
        assert_escaped(&text);

        let spots = [serde_json::json!({
            "procedure": CLEAR, "object": COLOURED,
            "selfTimeMs": 1.0, "totalTimeMs": 2.0, "hitCount": 3
        })];
        let text = hotspots_text(&spots);
        assert_escaped(&text);
        assert!(
            text.contains(r"Sec7 Tests\u{1b}[2J (Bad\u{1b}[31m Name\u{1b}[0m)"),
            "got: {text}"
        );
    }
}
