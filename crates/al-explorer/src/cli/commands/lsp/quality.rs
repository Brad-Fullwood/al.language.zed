//! Code-quality commands: complexity metrics, SQL anti-pattern scan, and the annotation auto-fixers (application area, tooltips, data classification).

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_metrics(
    file: Option<&str>,
    all: bool,
    threshold_cyclomatic: u32,
    threshold_cognitive: u32,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let mut params = serde_json::json!({
        "all": all,
        "thresholdCyclomatic": threshold_cyclomatic,
        "thresholdCognitive": threshold_cognitive,
    });

    if let Some(f) = file {
        let Some(uri) = file_to_uri(f) else {
            return report_error(&format!("Cannot resolve path: {f}"), json);
        };
        params["uri"] = serde_json::json!(uri);
    }

    match client.request("metrics", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if all {
                if let Some(files) = result.as_array() {
                    let mut total_hotspots = 0usize;
                    for file_result in files {
                        let fname = file_result
                            .get("file")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        if let Some(hotspots) =
                            file_result.get("hotspots").and_then(|v| v.as_array())
                        {
                            if !hotspots.is_empty() {
                                for h in hotspots {
                                    print_complexity_entry(Some(fname), h);
                                }
                                total_hotspots += hotspots.len();
                            }
                        }
                    }
                    if total_hotspots == 0 {
                        eprintln!("No complexity hotspots found");
                    } else {
                        eprintln!("\n{total_hotspots} hotspot(s) found");
                    }
                }
            } else {
                if let Some(procs) = result.get("procedures").and_then(|v| v.as_array()) {
                    if procs.is_empty() {
                        eprintln!("No procedures found");
                    } else {
                        for p in procs {
                            print_complexity_entry(file, p);
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn print_complexity_entry(file: Option<&str>, entry: &serde_json::Value) {
    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let line = entry.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
    let cyclomatic = entry
        .get("cyclomatic")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cognitive = entry.get("cognitive").and_then(|v| v.as_u64()).unwrap_or(0);
    let loc_prefix = if let Some(f) = file {
        format!("{f}:{line}: ")
    } else {
        format!("{line}: ")
    };
    println!("{loc_prefix}{name}  cyclomatic={cyclomatic}  cognitive={cognitive}");
}

pub fn cmd_sql_scan(json: bool) -> ExitCode {
    run_command(
        "sqlPatterns",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let violations = result.as_array().cloned().unwrap_or_default();
            if violations.is_empty() {
                eprintln!("No SQL anti-patterns found");
            } else {
                for v in &violations {
                    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
                    let object = v.get("object").and_then(|o| o.as_str()).unwrap_or("?");
                    let procedure = v.get("procedure").and_then(|p| p.as_str()).unwrap_or("?");
                    let line = v.get("line").and_then(|l| l.as_u64()).unwrap_or(0);
                    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("?");
                    let file_path = v.get("file").and_then(|f| f.as_str()).unwrap_or("");
                    if file_path.is_empty() {
                        println!("{object}::{procedure}:{line}: [{kind}] {message}");
                    } else {
                        println!("{file_path}:{line}: [{kind}] {object}::{procedure}: {message}");
                    }
                }
                eprintln!("\n{} SQL anti-pattern(s) found", violations.len());
            }
        },
    )
}

pub fn cmd_add_application_area(value: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request(
        "fix.applicationArea",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if dry_run {
                    println!("Dry run: would modify {files} file(s) with {changes} change(s)");
                } else {
                    println!(
                        "Applied ApplicationArea = {value} to {changes} control(s) in {files} file(s)"
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_add_tooltips(from_table: Option<&str>, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "dryRun": dry_run });
    if let Some(t) = from_table {
        params["fromTable"] = serde_json::Value::String(t.to_string());
    }
    match client.request("fix.tooltips", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if dry_run {
                    println!("Dry run: would modify {files} file(s) with {changes} tooltip(s)");
                } else {
                    println!("Added {changes} tooltip(s) across {files} file(s)");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_add_data_classification(value: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request(
        "fix.dataClassification",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if dry_run {
                    println!("Dry run: would modify {files} file(s) with {changes} field(s)");
                } else {
                    println!(
                        "Applied DataClassification = {value} to {changes} field(s) in {files} file(s)"
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

