use std::process::ExitCode;

use crate::{DebugCommands, DiagCommands, ProfileCommands, SnapshotCommands};

use super::{bc_server_params, connect, file_to_uri, print_json, report_error};

pub fn cmd_debug(subcmd: &DebugCommands, json: bool) -> ExitCode {
    match subcmd {
        DebugCommands::Start { config } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            client.set_read_timeout(std::time::Duration::from_secs(120));
            let params = serde_json::json!({
                "cmd": "start",
                "config": config,
            });
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session =
                            result.get("session").and_then(|v| v.as_str()).unwrap_or("?");
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
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
            let abs_file = file_to_uri(file).unwrap_or_else(|| file.clone());
            let params = serde_json::json!({
                "cmd": "breakpoint",
                "file": abs_file,
                "line": line,
                "condition": condition,
            });
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let bps = result.get("breakpoints").and_then(|v| v.as_array());
                        if let Some(bps) = bps {
                            for bp in bps {
                                let bp_line =
                                    bp.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                                let verified =
                                    bp.get("verified").and_then(|v| v.as_bool()).unwrap_or(false);
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
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let session_id = result
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        println!("Session {session_id}: {status}");
                        if let Some(loc) = result.get("location") {
                            let file =
                                loc.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                            let proc =
                                loc.get("procedure").and_then(|v| v.as_str()).unwrap_or("");
                            println!("  at {file}:{line} ({proc})");
                        }
                        if let Some(vars) = result.get("variables").and_then(|v| v.as_array()) {
                            println!("  Variables:");
                            for var in vars {
                                let name =
                                    var.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let val =
                                    var.get("value").and_then(|v| v.as_str()).unwrap_or("?");
                                let ty =
                                    var.get("typeName").and_then(|v| v.as_str()).unwrap_or("?");
                                println!("    {name}: {ty} = {val}");
                            }
                        }
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
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let val =
                            result.get("result").and_then(|v| v.as_str()).unwrap_or("?");
                        let ty =
                            result.get("typeName").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{val} ({ty})");
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
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
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
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Stepped. Status: {status}");
                        if let Some(loc) = result.get("location") {
                            let file =
                                loc.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!("  at {file}:{line}");
                        }
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
            let params = serde_json::json!({"cmd": "history", "var": var});
            match client.request("debug", Some(params)) {
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
                                    let seq =
                                        hit.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let ts = hit
                                        .get("timestamp")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
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
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Debug session {status}.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}

pub fn cmd_diag(subcmd: &DiagCommands, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let (cmd, params) = match subcmd {
        DiagCommands::Sessions => ("sessions", serde_json::json!({})),
        DiagCommands::Events {
            limit,
            level,
            target,
        } => (
            "events",
            serde_json::json!({ "limit": limit, "level": level, "target": target }),
        ),
        DiagCommands::Slow { limit } => ("slow", serde_json::json!({ "limit": limit })),
        DiagCommands::Failures => ("failures", serde_json::json!({})),
        DiagCommands::Search { query, limit } => (
            "search",
            serde_json::json!({ "query": query, "limit": limit }),
        ),
        DiagCommands::Summary => ("summary", serde_json::json!({})),
    };

    let mut req_params = params;
    req_params["cmd"] = serde_json::Value::String(cmd.to_string());

    match client.request("diag", Some(req_params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                match cmd {
                    "sessions" => {
                        let sessions = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        println!("{:<6} {:<22} {:<8} EVENTS", "ID", "STARTED", "PID");
                        println!("{}", "-".repeat(50));
                        for s in sessions {
                            println!(
                                "{:<6} {:<22} {:<8} {}",
                                s.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                                s.get("started_at").and_then(|v| v.as_str()).unwrap_or("?"),
                                s.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                                s.get("event_count").and_then(|v| v.as_i64()).unwrap_or(0),
                            );
                        }
                    }
                    "slow" => {
                        let spans = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        println!("{:<30} {:>12}", "OPERATION", "DURATION");
                        println!("{}", "-".repeat(44));
                        for s in spans {
                            let name =
                                s.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let dur =
                                s.get("duration_us").and_then(|v| v.as_i64()).unwrap_or(0);
                            let dur_ms = dur as f64 / 1000.0;
                            println!("{:<30} {:>10.1}ms", name, dur_ms);
                        }
                    }
                    "summary" => {
                        let total = result
                            .get("total_events")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let failures = result
                            .get("failure_count")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let avg_span = result
                            .get("avg_span_duration_us")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        println!(
                            "Session: {}",
                            result
                                .get("session_id")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0)
                        );
                        println!("Events:  {total}");
                        println!("Failures: {failures}");
                        println!("Avg span: {:.1}ms", avg_span as f64 / 1000.0);
                        if let Some(by_level) =
                            result.get("by_level").and_then(|v| v.as_array())
                        {
                            println!("\nBy level:");
                            for item in by_level {
                                if let Some(arr) = item.as_array() {
                                    let level =
                                        arr.first().and_then(|v| v.as_str()).unwrap_or("?");
                                    let count =
                                        arr.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                                    println!("  {:<8} {}", level, count);
                                }
                            }
                        }
                    }
                    _ => {
                        let events = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        for e in events {
                            let level =
                                e.get("level").and_then(|v| v.as_str()).unwrap_or("?");
                            let target =
                                e.get("target").and_then(|v| v.as_str()).unwrap_or("?");
                            let msg = e.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                            println!("[{level}] {target}: {msg}");
                        }
                        eprintln!("\n{} events", events.len());
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
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
            let mut params = bc_server_params(
                "start",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            );
            if let Some(d) = description {
                params["description"] = serde_json::json!(d);
            }
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshot_id = result
                            .get("snapshotId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
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
            let params = bc_server_params(
                "list",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                None,
            );
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshots = result.get("snapshots").and_then(|v| v.as_array());
                        if let Some(snaps) = snapshots {
                            if snaps.is_empty() {
                                eprintln!("No snapshots available on server.");
                            } else {
                                println!("{:<30} {:<25} {:>10}", "ID", "CREATED", "SIZE");
                                println!("{}", "-".repeat(70));
                                for s in snaps {
                                    let id =
                                        s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                    let created = s
                                        .get("createdAt")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("-");
                                    let size = s
                                        .get("sizeBytes")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    println!("{:<30} {:<25} {:>10}", id, created, size);
                                }
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
            let mut params = bc_server_params(
                "download",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            );
            params["snapshotId"] = serde_json::json!(snapshot_id);
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path =
                            result.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Snapshot {snapshot_id} {status}: {path}");
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
            let params = bc_server_params(
                "start",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            );
            match client.request("profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session_id = result
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
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
            let mut params = bc_server_params(
                "stop",
                server,
                company,
                username.as_deref(),
                password.as_deref(),
                output_dir.as_deref(),
            );
            if let Some(sid) = session_id {
                params["sessionId"] = serde_json::json!(sid);
            }
            match client.request("profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path =
                            result.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let status =
                            result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
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
            let params = serde_json::json!({
                "cmd": "analyze",
                "path": path,
                "topN": top,
            });
            match client.request("profiling", Some(params)) {
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
                                println!(
                                    "{:>8}  {:>8}  {:>8}  PROCEDURE",
                                    "SELF(ms)", "TOTAL(ms)", "HITS"
                                );
                                println!("{}", "-".repeat(80));
                                for h in spots {
                                    let proc = h
                                        .get("procedure")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    let self_ms = h
                                        .get("selfTimeMs")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    let total_ms = h
                                        .get("totalTimeMs")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    let hits =
                                        h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let obj = h.get("object").and_then(|v| v.as_str());
                                    let label = if let Some(o) = obj {
                                        format!("{proc} ({o})")
                                    } else {
                                        proc.to_string()
                                    };
                                    println!(
                                        "{self_ms:>8.1}  {total_ms:>8.1}  {hits:>8}  {label}"
                                    );
                                }
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
