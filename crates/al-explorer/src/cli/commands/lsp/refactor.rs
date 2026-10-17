//! Refactoring & diagnostics: profiler hints, member sorting, file organization, workspace diag, and test snapshot/mutation.

use std::fmt::Write as _;
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
                let hints = list_rows(&result).as_array().cloned().unwrap_or_default();
                if hints.is_empty() {
                    println!("No profiler hints found.");
                } else {
                    print!("{}", profiler_hints_text(&hints));
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
                if !changed {
                    println!("Already sorted — no changes.");
                } else if dry_run {
                    // The file is untouched; say what would change instead of
                    // reporting a sort that did not happen.
                    match (
                        result.get("sorted").and_then(|v| v.as_str()),
                        result.get("files"),
                    ) {
                        (Some(sorted), _) => {
                            println!("Would reorder members (dry run, nothing written):\n");
                            print!("{}", terminal_lines(sorted));
                        }
                        (None, Some(files)) => {
                            println!("Would reorder members in (dry run, nothing written):");
                            for file in files.as_array().into_iter().flatten() {
                                if file["changed"].as_bool() == Some(true) {
                                    println!("  {}", text_field(file, "file", "?"));
                                }
                            }
                        }
                        _ => println!("Would reorder members (dry run, nothing written)."),
                    }
                } else {
                    println!("Members sorted.");
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
                    print!("{}", organized_files_text(&files, dry_run));
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
                // A JSON value prints as JSON, which writes a control
                // character as `\u001b`.
                let value = serde_json::to_string(value).unwrap_or_default();
                println!("  {}: {value}", terminal_text(key));
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
        println!("{}", terminal_text(match_message));
    } else {
        print!("{}", divergences_text(divergences));
    }
    if divergences.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Failing methods in a `tests.snapshot_capture` response.
///
/// The daemon runs the test to record the snapshot and returns its
/// `TestCodeunitResult` under `testResult`. A baseline captured from a red test
/// is worthless, so it must not report success.
pub(crate) fn captured_test_failures(result: &serde_json::Value) -> u64 {
    result
        .get("testResult")
        .and_then(|value| value.get("failed"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
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
                    let failed = captured_test_failures(&result);
                    if json {
                        print_json(&result);
                    } else {
                        let samples = result
                            .get("sampleCount")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(0);
                        let path = text_field(&result, "snapshotPath", "?");
                        if failed > 0 {
                            println!(
                                "[FAIL] Captured {samples} sample(s) to {path} from a test with \
                                 {failed} failing method(s)"
                            );
                        } else {
                            println!("[PASS] Captured {samples} sample(s) to {path}");
                        }
                    }
                    if failed > 0 {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
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
                        println!("{}", valid_snapshot_line(&result));
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
                    print!("{}", survivors_text(&survivors));
                }
            }

            exit_code
        }
        Err(e) => report_error(&e, json),
    }
}

/// One line per profiler hint: the object, the procedure, the time and the
/// sample count.
fn profiler_hints_text(hints: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for h in hints {
        let procedure = text_field(h, "procedure", "?");
        let object = text_field(h, "object", "?");
        let self_ms = h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let hits = h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
        let _ = writeln!(
            out,
            "{object}::{procedure}: {self_ms:.1}ms ({hits} samples)"
        );
    }
    out
}

/// One line per file `organize-files` renamed or would rename.
fn organized_files_text(files: &[serde_json::Value], dry_run: bool) -> String {
    let mut out = String::new();
    for f in files {
        let from = text_field(f, "from", "?");
        let to = text_field(f, "to", "?");
        // The daemon reports `renamed: !dryRun` and turns a failed rename
        // into an RPC error, so a listed file in a non-dry run was renamed.
        let status = if dry_run { "[dry-run]" } else { "[renamed]" };
        let _ = writeln!(out, "{status} {from} -> {to}");
    }
    out
}

/// The count and one line per divergence between two snapshots.
fn divergences_text(divergences: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} divergence(s):", divergences.len());
    for divergence in divergences {
        let breakpoint = divergence
            .get("breakpoint_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let iteration = divergence
            .get("iteration")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let field = text_field(divergence, "field_path", "?");
        // The recorded values print as JSON, which writes a control
        // character as `\u001b`.
        let old = serde_json::to_string(&divergence["old_value"]).unwrap_or_default();
        let new = serde_json::to_string(&divergence["new_value"]).unwrap_or_default();
        let _ = writeln!(
            out,
            "  [bp {breakpoint} iter {iteration}] {field}: {old} -> {new}"
        );
    }
    out
}

/// The line `test-snapshot validate` prints for a valid snapshot.
fn valid_snapshot_line(result: &serde_json::Value) -> String {
    let samples = result
        .get("sampleCount")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let codeunit = match result.get("codeunitId") {
        Some(id) => serde_json::to_string(id).unwrap_or_default(),
        None => "?".to_string(),
    };
    let method = text_field(result, "methodName", "?");
    let bc_version = text_field(result, "bcVersion", "?");
    format!(
        "[PASS] Valid snapshot: {samples} sample(s), codeunit {codeunit}, method {method}, \
         BC {bc_version}"
    )
}

/// The table of mutations no test killed.
fn survivors_text(survivors: &[&serde_json::Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\nSurvived mutations (test gaps):");
    let _ = writeln!(out, "{:<8} {:<30} {:>5}  Change", "ID", "File", "Line");
    let _ = writeln!(out, "{}", "-".repeat(70));
    for v in survivors {
        // The mutation details (id/file/line/description) live in the nested
        // `variant` object. The top-level keys are `killed`, `killingTest` and
        // `error`. Reading them at the top level printed "?  ?  0  ?" for
        // every survivor row.
        let variant = v.get("variant").unwrap_or(v);
        let id = variant.get("id").and_then(|x| x.as_str()).unwrap_or("?");
        let file = variant.get("file").and_then(|x| x.as_str()).unwrap_or("?");
        let file_short = terminal_text(
            std::path::Path::new(file)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(file),
        );
        let line = variant.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
        let desc = text_field(variant, "description", "?");
        let reason = text_field(v, "survivalReason", "unknown");
        // The variant id embeds the source file name verbatim, so a byte
        // slice splits a multi-byte character in `Kundæ.al` and panics.
        let id_short = terminal_text(&id.chars().take(8).collect::<String>());
        let _ = writeln!(
            out,
            "{id_short:<8} {file_short:<30} {line:>5}  {desc} [{reason}]"
        );
    }
    out
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
    fn a_capture_from_a_failing_test_is_not_a_success() {
        // `test-snapshot capture` used to print `[PASS]` and exit 0 whatever
        // the captured test did, so every later validate/replay compared
        // against a baseline recorded from a red test.
        let response = |failed: u64| {
            serde_json::json!({
                "captured": true,
                "snapshotPath": "snap/base.snap.json",
                "sampleCount": 3,
                "testResult": { "total": 2, "passed": 2 - failed, "failed": failed, "skipped": 0 },
            })
        };
        assert_eq!(captured_test_failures(&response(0)), 0);
        assert_eq!(captured_test_failures(&response(1)), 1);
        // A response without the field must not read as a failure.
        assert_eq!(
            captured_test_failures(&serde_json::json!({ "captured": true })),
            0
        );
    }

    #[test]
    fn a_non_ascii_variant_id_is_shortened_by_characters() {
        // `&id[..8]` split the `æ` in a `Kundæ.al` variant id and panicked.
        let id = "cb:Kundæ.al:12:340:x";
        let short: String = id.chars().take(8).collect();
        assert_eq!(short, "cb:Kundæ");
    }

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

#[cfg(test)]
mod terminal_text_tests {
    use super::{
        divergences_text, organized_files_text, profiler_hints_text, survivors_text,
        valid_snapshot_line,
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
    fn profiler_hints_and_renamed_files_print_crafted_names_escaped() {
        let hints = [serde_json::json!({
            "object": CLEAR, "procedure": COLOURED, "selfTimeMs": 1.5, "hitCount": 2
        })];
        assert_eq!(
            profiler_hints_text(&hints),
            "Sec7 Tests\\u{1b}[2J::Bad\\u{1b}[31m Name\\u{1b}[0m: 1.5ms (2 samples)\n"
        );

        let files = [serde_json::json!({"from": CLEAR, "to": TITLE})];
        assert_escaped(&organized_files_text(&files, true));
    }

    #[test]
    fn snapshot_and_mutation_output_prints_crafted_names_escaped() {
        let divergences = [serde_json::json!({
            "breakpoint_id": 1, "iteration": 0, "field_path": CLEAR,
            "old_value": COLOURED, "new_value": TITLE
        })];
        assert_escaped(&divergences_text(&divergences));

        let snapshot = serde_json::json!({
            "sampleCount": 3, "codeunitId": 50172, "methodName": CLEAR, "bcVersion": TITLE
        });
        assert_escaped(&valid_snapshot_line(&snapshot));

        let survivor = serde_json::json!({
            "killed": false, "survivalReason": TITLE,
            "variant": {
                "id": "\u{1b}[2Jabcdef", "file": format!("src/{CLEAR}.al"),
                "line": 4, "description": COLOURED
            }
        });
        let text = survivors_text(&[&survivor]);
        assert_escaped(&text);
        assert!(text.contains(r"\u{1b}[2Jabcd Sec7"), "got: {text}");
    }
}
