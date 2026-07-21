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
    match client.request(
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
        let uri = file_to_uri(f).unwrap_or_else(|| f.to_string());
        serde_json::json!({ "uri": uri, "dryRun": dry_run })
    } else {
        serde_json::json!({ "all": all, "dryRun": dry_run })
    };
    match client.request("sortMembers", Some(params)) {
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
    match client.request(
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
pub fn cmd_test_snapshot(subcmd: &crate::cli::TestSnapshotCommands, json: bool) -> ExitCode {
    use crate::cli::TestSnapshotCommands;

    match subcmd {
        TestSnapshotCommands::Validate { path } => {
            let abs_path = match std::path::Path::new(path).canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    return report_error(&format!("Cannot resolve path '{path}': {e}"), json);
                }
            };
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            client.set_request_timeout(std::time::Duration::from_secs(120));
            let params = serde_json::json!({
                "snapshotPath": abs_path.display().to_string(),
            });
            match client.request("tests.snapshot_validate", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        println!("[PASS] Snapshot file is valid.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
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
            match client.request("tests.snapshot_diff", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        // The daemon returns `{ "divergences": [Divergence] }`,
                        // not a bare array, and each Divergence carries
                        // `breakpoint_id` / `iteration` / `field_path` /
                        // `old_value` / `new_value`. The previous formatter read
                        // a top-level array with `sampleIndex`/`field`/`expected`/
                        // `actual`, so it always printed "Snapshots are
                        // identical." even when they differed.
                        let divs = result
                            .get("divergences")
                            .and_then(|v| v.as_array())
                            .map(|v| &v[..])
                            .unwrap_or(&[]);
                        if divs.is_empty() {
                            println!("Snapshots are identical.");
                        } else {
                            println!("{} divergence(s):", divs.len());
                            for d in divs {
                                let bp =
                                    d.get("breakpoint_id").and_then(|v| v.as_u64()).unwrap_or(0);
                                let iter = d.get("iteration").and_then(|v| v.as_u64()).unwrap_or(0);
                                let field =
                                    d.get("field_path").and_then(|v| v.as_str()).unwrap_or("?");
                                let old = d.get("old_value").cloned().unwrap_or_default();
                                let new = d.get("new_value").cloned().unwrap_or_default();
                                println!("  [bp {bp} iter {iter}] {field}: {old} -> {new}");
                            }
                            return ExitCode::FAILURE;
                        }
                    }
                    ExitCode::SUCCESS
                }
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

    match client.request("tests.mutate", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }

            let killed = result.get("killed").and_then(|v| v.as_u64()).unwrap_or(0);
            let survived = result.get("survived").and_then(|v| v.as_u64()).unwrap_or(0);
            let errored = result.get("errored").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = killed + survived;
            let score = if total > 0 {
                killed as f64 * 100.0 / total as f64
            } else {
                0.0
            };

            println!(
                "Killed: {killed} · Survived: {survived} · Errored: {errored} (mutation score: {score:.1}%)"
            );

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
                        println!(
                            "{:<8} {:<30} {:>5}  {}",
                            &id[..id.len().min(8)],
                            file_short,
                            line,
                            desc
                        );
                    }
                }
            }

            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
