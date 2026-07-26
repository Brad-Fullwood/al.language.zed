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
        let uri = match file_to_uri(f) {
            Ok(uri) => uri,
            Err(error) => return report_error(&error, json),
        };
        params["uri"] = serde_json::json!(uri);
    }

    match request_checked(&mut client, "metrics", Some(params)) {
        Ok(result) => {
            let has_hotspots = if all {
                result.as_array().is_some_and(|files| {
                    files.iter().any(|file| {
                        file.get("hotspots")
                            .and_then(|value| value.as_array())
                            .is_some_and(|hotspots| !hotspots.is_empty())
                    })
                })
            } else {
                result
                    .get("hotspots")
                    .and_then(|value| value.as_array())
                    .is_some_and(|hotspots| !hotspots.is_empty())
            };
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
            if has_hotspots {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
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
    run_command_with_exit(
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
        |result| {
            if result
                .as_array()
                .is_some_and(|violations| !violations.is_empty())
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        },
    )
}

pub fn cmd_add_application_area(value: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(
        &mut client,
        "fix.applicationArea",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            let (files, changes) = match bulk_fix_counts(&result, dry_run) {
                Ok(summary) => summary,
                Err(error) => return report_error(&error, json),
            };
            if json {
                print_json(&result);
            } else {
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

pub fn cmd_add_tooltips(from_table: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "dryRun": dry_run, "fromTable": from_table });
    match request_checked(&mut client, "fix.tooltips", Some(params)) {
        Ok(result) => {
            let (files, changes) = match bulk_fix_counts(&result, dry_run) {
                Ok(summary) => summary,
                Err(error) => return report_error(&error, json),
            };
            if json {
                print_json(&result);
            } else {
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
    match request_checked(
        &mut client,
        "fix.dataClassification",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            let (files, changes) = match bulk_fix_counts(&result, dry_run) {
                Ok(summary) => summary,
                Err(error) => return report_error(&error, json),
            };
            if json {
                print_json(&result);
            } else {
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

fn bulk_fix_counts(
    result: &serde_json::Value,
    expected_dry_run: bool,
) -> Result<(usize, u64), String> {
    let object = result.as_object().ok_or_else(|| {
        "daemon returned an invalid bulk-fix response: expected object".to_string()
    })?;
    let files = object
        .get("modifiedFiles")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "daemon returned an invalid bulk-fix response: 'modifiedFiles' must be an array"
                .to_string()
        })?;
    if files.iter().any(|file| !file.is_string()) {
        return Err(
            "daemon returned an invalid bulk-fix response: every modified file must be a string"
                .to_string(),
        );
    }
    let changes = object
        .get("changesCount")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            "daemon returned an invalid bulk-fix response: 'changesCount' must be an integer"
                .to_string()
        })?;
    let dry_run = object
        .get("dryRun")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            "daemon returned an invalid bulk-fix response: 'dryRun' must be a boolean".to_string()
        })?;
    if dry_run != expected_dry_run {
        return Err(format!(
            "daemon returned an inconsistent bulk-fix response: requested dryRun={expected_dry_run}, got {dry_run}"
        ));
    }
    Ok((files.len(), changes))
}

#[cfg(test)]
mod tests {
    use super::bulk_fix_counts;

    #[test]
    fn bulk_fix_response_contract_rejects_old_or_inconsistent_shapes() {
        assert!(
            bulk_fix_counts(
                &serde_json::json!({
                    "modifiedFiles": ["A.al"],
                    "changesCount": 2,
                    "dryRun": true
                }),
                true
            )
            .is_ok()
        );
        for value in [
            serde_json::json!({"filesModified": 1, "totalChanges": 2}),
            serde_json::json!({
                "modifiedFiles": ["A.al"],
                "changesCount": 2,
                "dryRun": false
            }),
            serde_json::json!({
                "modifiedFiles": [7],
                "changesCount": 2,
                "dryRun": true
            }),
        ] {
            assert!(bulk_fix_counts(&value, true).is_err());
        }
    }
}
