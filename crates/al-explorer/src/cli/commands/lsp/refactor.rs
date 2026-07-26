//! Refactoring & diagnostics: profiler hints, member sorting, file organization, workspace diag, and test snapshot/mutation.

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_profiler_hints(hotspots: &[String], json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    // The daemon's `profiler_hints` resolver keys on a `procedure` field (and
    // an optional `object` field to disambiguate same-named procedures), so
    // emit those keys — a previous `{"name": …}` payload was silently ignored
    // and every lookup returned zero hints. Accept the
    // `Object.Procedure` shorthand used elsewhere (e.g. `impact`) by splitting
    // on the last dot.
    let hotspot_values: Vec<serde_json::Value> = hotspots
        .iter()
        .map(|h| match h.rsplit_once('.') {
            Some((object, procedure)) if !object.is_empty() && !procedure.is_empty() => {
                serde_json::json!({ "object": object, "procedure": procedure })
            }
            _ => serde_json::json!({ "procedure": h }),
        })
        .collect();
    match request_checked(
        &mut client,
        "profiler.hints",
        Some(serde_json::json!({ "hotspots": hotspot_values })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let hints = result.as_array().cloned().unwrap_or_default();
                if hints.is_empty() {
                    println!("No profiler hints found.");
                } else {
                    for h in &hints {
                        let procedure = h.get("procedure").and_then(|v| v.as_str()).unwrap_or("?");
                        let object = h.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                        let self_ms = h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let hits = h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
                        println!("{object}::{procedure}: {self_ms:.1}ms ({hits} samples)");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_sort_members(file: Option<&str>, all: bool, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = if let Some(f) = file {
        let uri = match file_to_uri(f) {
            Ok(uri) => uri,
            Err(error) => return report_error(&error, json),
        };
        serde_json::json!({ "uri": uri, "dryRun": dry_run })
    } else {
        serde_json::json!({ "all": all, "dryRun": dry_run })
    };
    match request_checked(&mut client, "sortMembers", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let changed = result
                    .get("changed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if changed {
                    println!("Members sorted.");
                } else {
                    println!("Already sorted — no changes.");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_organize_files(dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(
        &mut client,
        "organizeFiles",
        Some(serde_json::json!({ "dryRun": dry_run })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("files")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if files.is_empty() {
                    println!("All files already correctly named.");
                } else {
                    for f in &files {
                        let from = f.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                        let to = f.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                        let renamed = f.get("renamed").and_then(|v| v.as_bool()).unwrap_or(false);
                        let status = if dry_run {
                            "[dry-run]"
                        } else if renamed {
                            "[renamed]"
                        } else {
                            "[failed]"
                        };
                        println!("{status} {from} -> {to}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_diag(json: bool) -> ExitCode {
    run_command(
        "diag",
        Some(serde_json::json!({"cmd": "summary"})),
        json,
        None,
        |result| {
            println!("Workspace Diagnostics:");
            for (key, value) in result.as_object().into_iter().flat_map(|o| o.iter()) {
                println!("  {}: {}", key, value);
            }
        },
    )
}

fn report_snapshot_comparison(
    result: &serde_json::Value,
    json: bool,
    match_message: &str,
) -> ExitCode {
    let divergences = result
        .get("divergences")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if json {
        print_json(result);
    } else if divergences.is_empty() {
        println!("{match_message}");
    } else {
        println!("{} divergence(s):", divergences.len());
        for divergence in divergences {
            let breakpoint = divergence
                .get("breakpoint_id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let iteration = divergence
                .get("iteration")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let field = divergence
                .get("field_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let old = divergence.get("old_value").cloned().unwrap_or_default();
            let new = divergence.get("new_value").cloned().unwrap_or_default();
            println!("  [bp {breakpoint} iter {iteration}] {field}: {old} -> {new}");
        }
    }
    if divergences.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn cmd_test_snapshot(subcmd: &crate::cli::TestSnapshotCommands, json: bool) -> ExitCode {
    use crate::cli::TestSnapshotCommands;

    match subcmd {
        TestSnapshotCommands::Capture {
            codeunit,
            codeunit_name,
            method,
            bc_version,
            breakpoints,
            output,
            config,
            timeout_ms,
        } => {
            let mut points = Vec::with_capacity(breakpoints.len());
            for breakpoint in breakpoints {
                let Some((file, line)) = breakpoint.rsplit_once(':') else {
                    return report_error(
                        &format!(
                            "Invalid breakpoint '{breakpoint}'; expected FILE:LINE with a 1-based line"
                        ),
                        json,
                    );
                };
                let line = match line.parse::<u32>().ok().filter(|line| *line > 0) {
                    Some(line) => line,
                    None => {
                        return report_error(
                            &format!(
                                "Invalid breakpoint '{breakpoint}'; line must be a positive integer"
                            ),
                            json,
                        );
                    }
                };
                points.push(serde_json::json!({ "file": file, "line": line }));
            }
            let output = if std::path::Path::new(output).is_absolute() {
                std::path::PathBuf::from(output)
            } else {
                match std::env::current_dir() {
                    Ok(current) => current.join(output),
                    Err(error) => {
                        return report_error(
                            &format!("Cannot resolve snapshot output path: {error}"),
                            json,
                        );
                    }
                }
            };
            let mut params = serde_json::json!({
                "codeunitId": codeunit,
                "codeunitName": codeunit_name,
                "methodName": method,
                "bcVersion": bc_version,
                "breakpoints": points,
                "outputPath": output,
            });
            if let Some(config) = config {
                params["config"] = serde_json::json!(config);
            }
            if let Some(timeout_ms) = timeout_ms {
                params["timeoutMs"] = serde_json::json!(timeout_ms);
            }
            let mut client = match connect(None) {
                Ok(client) => client,
                Err(error) => return report_error(&error, json),
            };
            client.set_request_timeout(std::time::Duration::from_secs(
                timeout_ms
                    .map(|timeout| timeout.div_ceil(1000).saturating_add(30))
                    .unwrap_or(330),
            ));
            match request_checked(&mut client, "tests.snapshot_capture", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        println!(
                            "[PASS] Captured {} sample(s) to {}",
                            result
                                .get("sampleCount")
                                .and_then(|value| value.as_u64())
                                .unwrap_or(0),
                            result
                                .get("snapshotPath")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?"),
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => report_error(&error, json),
            }
        }
        TestSnapshotCommands::Validate { path } => {
            let abs_path = match std::path::Path::new(path).canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return report_error(&format!("Cannot resolve path '{path}': {error}"), json);
                }
            };
            let mut client = match connect(None) {
                Ok(client) => client,
                Err(error) => return report_error(&error, json),
            };
            client.set_request_timeout(std::time::Duration::from_secs(120));
            let params = serde_json::json!({
                "snapshotPath": abs_path.display().to_string(),
            });
            match request_checked(&mut client, "tests.snapshot_validate", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        println!(
                            "[PASS] Valid snapshot: {} sample(s), codeunit {}, method {}, BC {}",
                            result
                                .get("sampleCount")
                                .and_then(|value| value.as_u64())
                                .unwrap_or(0),
                            result
                                .get("codeunitId")
                                .map(serde_json::Value::to_string)
                                .unwrap_or_else(|| "?".to_string()),
                            result
                                .get("methodName")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?"),
                            result
                                .get("bcVersion")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?")
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(error) => report_error(&error, json),
            }
        }
        TestSnapshotCommands::Replay {
            path,
            bc_version,
            config,
            timeout_ms,
        } => {
            let snapshot_path = match std::path::Path::new(path).canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return report_error(&format!("Cannot resolve path '{path}': {error}"), json);
                }
            };
            let mut params = serde_json::json!({
                "snapshotPath": snapshot_path,
                "bcVersion": bc_version,
            });
            if let Some(config) = config {
                params["config"] = serde_json::json!(config);
            }
            if let Some(timeout_ms) = timeout_ms {
                params["timeoutMs"] = serde_json::json!(timeout_ms);
            }
            let mut client = match connect(None) {
                Ok(client) => client,
                Err(error) => return report_error(&error, json),
            };
            client.set_request_timeout(std::time::Duration::from_secs(
                timeout_ms
                    .map(|timeout| timeout.div_ceil(1000).saturating_add(30))
                    .unwrap_or(330),
            ));
            match request_checked(&mut client, "tests.snapshot_replay", Some(params)) {
                Ok(result) => report_snapshot_comparison(
                    &result,
                    json,
                    "[PASS] Live replay matches the baseline snapshot.",
                ),
                Err(error) => report_error(&error, json),
            }
        }
        TestSnapshotCommands::Diff { a, b } => {
            let abs_a = match std::path::Path::new(a).canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return report_error(&format!("Cannot resolve path '{a}': {e}"), json);
                }
            };
            let abs_b = match std::path::Path::new(b).canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return report_error(&format!("Cannot resolve path '{b}': {e}"), json);
                }
            };
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({
                "pathA": abs_a.display().to_string(),
                "pathB": abs_b.display().to_string(),
            });
            match request_checked(&mut client, "tests.snapshot_diff", Some(params)) {
                Ok(result) => report_snapshot_comparison(&result, json, "Snapshots are identical."),
                Err(e) => report_error(&e, json),
            }
        }
    }
}
pub fn cmd_test_mutate(
    files: &[String],
    parallel: bool,
    timeout_ms: Option<u64>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let mut params = serde_json::json!({ "parallel": parallel });
    if !files.is_empty() {
        params["files"] = serde_json::Value::Array(
            files
                .iter()
                .map(|f| serde_json::Value::String(f.clone()))
                .collect(),
        );
    }
    if let Some(ms) = timeout_ms {
        params["timeoutMs"] = serde_json::Value::Number(serde_json::Number::from(ms));
    }

    // Mutation testing re-runs the suite once per mutant — the longest
    // operation the daemon offers.
    client.set_request_timeout(std::time::Duration::from_secs(3600));

    match request_checked(&mut client, "tests.mutate", Some(params)) {
        Ok(result) => {
            let exit_code = match mutation_exit_code(&result) {
                Ok(code) => code,
                Err(error) => return report_error(&error, json),
            };
            if json {
                print_json(&result);
                return exit_code;
            }

            let killed = result.get("killed").and_then(|v| v.as_u64()).unwrap_or(0);
            let survived = result.get("survived").and_then(|v| v.as_u64()).unwrap_or(0);
            let errored = result.get("errored").and_then(|v| v.as_u64()).unwrap_or(0);
            let unscored = result.get("unscored").and_then(|v| v.as_u64()).unwrap_or(0);
            let score = result.get("mutationScore").and_then(|v| v.as_f64());

            match score {
                Some(score) => println!(
                    "Killed: {killed} · Survived: {survived} · Errored: {errored} · Unscored: {unscored} (mutation score: {score:.1}%)"
                ),
                None => println!(
                    "Killed: {killed} · Survived: {survived} · Errored: {errored} · Unscored: {unscored} (mutation score: n/a)"
                ),
            }

            if let Some(variants) = result.get("variants").and_then(|v| v.as_array()) {
                let survivors: Vec<_> = variants
                    .iter()
                    .filter(|v| !v.get("killed").and_then(|k| k.as_bool()).unwrap_or(false))
                    .filter(|v| v.get("error").and_then(|e| e.as_str()).is_none())
                    .collect();
                if !survivors.is_empty() {
                    println!("\nSurvived mutations (test gaps):");
                    println!("{:<8} {:<30} {:>5}  Change", "ID", "File", "Line");
                    println!("{}", "-".repeat(70));
                    for v in &survivors {
                        // The mutation details (id/file/line/description) live in
                        // the nested `variant` object; the top-level keys are
                        // `killed`/`killingTest`/`error`. Reading them at the top
                        // level printed "?  ?  0  ?" for every survivor row.
                        let variant = v.get("variant").unwrap_or(v);
                        let id = variant.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                        let file = variant.get("file").and_then(|x| x.as_str()).unwrap_or("?");
                        let file_short = std::path::Path::new(file)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(file);
                        let line = variant.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
                        let desc = variant
                            .get("description")
                            .and_then(|x| x.as_str())
                            .unwrap_or("?");
                        let reason = v
                            .get("survivalReason")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown");
                        println!(
                            "{:<8} {:<30} {:>5}  {} [{}]",
                            &id[..id.len().min(8)],
                            file_short,
                            line,
                            desc,
                            reason
                        );
                    }
                }
            }

            exit_code
        }
        Err(e) => report_error(&e, json),
    }
}

fn mutation_exit_code(result: &serde_json::Value) -> Result<ExitCode, String> {
    let count = |field: &str| {
        result
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                format!("validated mutation response has no non-negative integer '{field}' count")
            })
    };
    let survived = count("survived")?;
    let errored = count("errored")?;
    let unscored = count("unscored")?;
    if survived == 0 && errored == 0 && unscored == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod mutation_exit_tests {
    use super::*;

    #[test]
    fn mutation_exit_fails_for_every_non_killed_outcome() {
        let result = |survived, errored, unscored| {
            serde_json::json!({
                "killed": 1,
                "survived": survived,
                "errored": errored,
                "unscored": unscored,
                "variants": [],
            })
        };
        assert_eq!(
            mutation_exit_code(&result(0, 0, 0)).unwrap(),
            ExitCode::SUCCESS
        );
        for value in [result(1, 0, 0), result(0, 1, 0), result(0, 0, 1)] {
            assert_eq!(mutation_exit_code(&value).unwrap(), ExitCode::FAILURE);
        }
    }
}
