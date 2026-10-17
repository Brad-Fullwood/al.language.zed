//! `al-explorer debug`, `snapshot` and `profile` subcommands: the CLI front
//! for the daemon's debug-session, snapshot and profiling methods.

use std::fmt::Write as _;
use std::process::ExitCode;

use serde_json::Value;

use super::super::{DebugCommands, ProfileCommands, SnapshotCommands};

use super::{
    absolutize_path, bc_server_params, connect, file_to_uri, print_json, report_error,
    request_checked, terminal_text, text_field,
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

/// The table `snapshot list` prints for the server's snapshots.
fn snapshots_text(snaps: &[Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<30} {:<25} {:>10}", "ID", "CREATED", "SIZE");
    let _ = writeln!(out, "{}", "-".repeat(70));
    for s in snaps {
        let id = text_field(s, "id", "?");
        let created = text_field(s, "createdAt", "-");
        let size = s.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let _ = writeln!(out, "{id:<30} {created:<25} {size:>10}");
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

pub fn cmd_snapshot(subcmd: &SnapshotCommands, json: bool) -> ExitCode {
    match subcmd {
        SnapshotCommands::Start {
            server,
            company,
            description,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = match bc_server_params(
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
            if let Some(d) = description {
                params["description"] = serde_json::json!(d);
            }
            match request_checked(&mut client, "snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshot_id = text_field(&result, "snapshotId", "?");
                        let status = text_field(&result, "status", "?");
                        println!("Snapshot started: {snapshot_id} (status: {status})");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        SnapshotCommands::List {
            server,
            company,
            username,
            password,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = match bc_server_params(
                "list",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                None,
            ) {
                Ok(params) => params,
                Err(error) => return report_error(&error, json),
            };
            match request_checked(&mut client, "snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshots = result.get("snapshots").and_then(|v| v.as_array());
                        if let Some(snaps) = snapshots {
                            if snaps.is_empty() {
                                eprintln!("No snapshots available on server.");
                            } else {
                                print!("{}", snapshots_text(snaps));
                                eprintln!("\n{} snapshot(s)", snaps.len());
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        SnapshotCommands::Download {
            snapshot_id,
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
                "download",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            ) {
                Ok(params) => params,
                Err(error) => return report_error(&error, json),
            };
            params["snapshotId"] = serde_json::json!(snapshot_id);
            match request_checked(&mut client, "snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path = text_field(&result, "path", "?");
                        let status = text_field(&result, "status", "?");
                        println!("Snapshot {} {status}: {path}", terminal_text(snapshot_id));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
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
        debug_eval_line, debug_state_text, debug_step_text, hotspots_text, snapshots_text,
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
        let snaps = [serde_json::json!({"id": CLEAR, "createdAt": TITLE, "sizeBytes": 10})];
        assert_escaped(&snapshots_text(&snaps));

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
