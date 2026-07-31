//! Workspace analysis & generation: object generate, obsolete/audit/permission reports, dependency graph, breaking-change/arch lint, and duplicate detection.

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_generate(
    kind: &str,
    id: i64,
    name: &str,
    table: Option<&str>,
    page_type: Option<&str>,
    subject: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "kind": kind, "id": id, "name": name });
    if let Some(t) = table {
        params["table"] = serde_json::Value::String(t.to_string());
    }
    if let Some(pt) = page_type {
        params["pageType"] = serde_json::Value::String(pt.to_string());
    }
    if let Some(s) = subject {
        params["subject"] = serde_json::Value::String(s.to_string());
    }
    match request_checked(&mut client, "generate", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let code = result.get("code").and_then(|v| v.as_str()).unwrap_or("");
                print!("{code}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_obsolete(json: bool) -> ExitCode {
    run_command(
        "obsolete",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let entries = result.as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("No obsolete symbols found.");
            } else {
                for e in &entries {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let object = e.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let symbol = e.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
                    let reason = e.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                    let state = e.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{kind} {object}::{symbol} [{state}]: {reason}");
                }
                eprintln!("\n{} obsolete symbol(s)", entries.len());
            }
        },
    )
}

pub fn cmd_audit_data_classification(json: bool) -> ExitCode {
    run_command_with_exit(
        "audit.dataClassification",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let entries = result.as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("No table fields found to audit.");
            } else {
                let mut unclassified = 0usize;
                for e in &entries {
                    let table = e.get("table").and_then(|v| v.as_str()).unwrap_or("?");
                    let field = e.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                    let classification = e
                        .get("classification")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let risk = e.get("risk").and_then(|v| v.as_str()).unwrap_or("?");
                    if risk == "unclassified" {
                        unclassified += 1;
                    }
                    println!("{table}.{field}: {classification} [{risk}]");
                }
                eprintln!(
                    "\n{} field(s) audited; {} unclassified",
                    entries.len(),
                    unclassified
                );
            }
        },
        |result| {
            if result.as_array().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.get("risk").and_then(|value| value.as_str()) == Some("unclassified")
                })
            }) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        },
    )
}

pub fn cmd_permission_audit(json: bool) -> ExitCode {
    run_command_with_exit(
        "permissions.audit",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let coverage = result
                .get("coverage")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let over_broad = result
                .get("overBroad")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let over_granted = result
                .get("overGrantedRights")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            if coverage.is_empty() {
                println!("All objects covered by permission sets.");
            } else {
                for e in &coverage {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let covered = e.get("covered").and_then(|v| v.as_bool()).unwrap_or(false);
                    let status = if covered { "covered" } else { "MISSING" };
                    println!("{kind} \"{name}\": {status}");
                }
            }

            // Report over-broad and unused object-level grants.
            if over_broad.is_empty() {
                println!("\nNo over-broad grants detected (object-level check).");
            } else {
                println!("\nOver-broad / unused grants (object-level; RIMDX rights not verified):");
                for e in &over_broad {
                    let set = e
                        .get("permissionSet")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let ot = e.get("objectType").and_then(|v| v.as_str()).unwrap_or("?");
                    let obj = e.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let rights = e.get("rights").and_then(|v| v.as_str()).unwrap_or("");
                    println!(
                        "  {set}: {ot} \"{obj}\" = {rights} — unused (object not referenced in workspace)"
                    );
                }
                eprintln!("\n{} over-broad grant(s)", over_broad.len());
            }

            // Report right-level (RIMDX) over-grants on referenced tables.
            if over_granted.is_empty() {
                println!("\nNo over-granted RIMDX rights detected (right-level check).");
            } else {
                println!(
                    "\nOver-granted rights (right-level; table is read but I/M/D write site not \
                     found — over-approximation, R never flagged):"
                );
                for e in &over_granted {
                    let set = e
                        .get("permissionSet")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let obj = e.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let granted = e
                        .get("grantedRights")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let over = e.get("overGranted").and_then(|v| v.as_str()).unwrap_or("");
                    let observed = e
                        .get("observedRights")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!(
                        "  {set}: TableData \"{obj}\" = {granted} — only {observed} observed; \
                         drop {over}"
                    );
                }
                eprintln!("\n{} over-granted right(s)", over_granted.len());
            }
        },
        |result| {
            let missing = result
                .get("coverage")
                .and_then(|value| value.as_array())
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry.get("covered").and_then(|value| value.as_bool()) == Some(false)
                    })
                });
            let over_broad = result
                .get("overBroad")
                .and_then(|value| value.as_array())
                .is_some_and(|entries| !entries.is_empty());
            let over_granted = result
                .get("overGrantedRights")
                .and_then(|value| value.as_array())
                .is_some_and(|entries| !entries.is_empty());
            if missing || over_broad || over_granted {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        },
    )
}

pub fn cmd_deps_graph(format: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let include_dot = format == "dot";
    match request_checked(
        &mut client,
        "deps.graph",
        Some(serde_json::json!({ "format": format, "dot": include_dot })),
    ) {
        Ok(result) => {
            if json || !include_dot {
                print_json(&result);
            } else {
                let dot = result
                    .get("content")
                    .and_then(|v| v.as_str())
                    .or_else(|| result.get("dot").and_then(|v| v.as_str()))
                    .unwrap_or("");
                print!("{dot}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// Read a previous `.app`'s symbols into the `{"baselineSymbols": [...]}`
/// shape the daemon's `breaking`/`upgrade` methods expect
/// (`baseline_symbols_from_params` in `build_dispatch/mod.rs`).
fn baseline_params_from_app(baseline_app: &str) -> Result<serde_json::Value, String> {
    let pkg = al_symbols::app_reader::read_app_file(std::path::Path::new(baseline_app))
        .map_err(|e| format!("reading baseline .app {baseline_app}: {e}"))?;
    let baseline_symbols = serde_json::to_value(&pkg.objects)
        .map_err(|e| format!("serializing baseline symbols: {e}"))?;
    Ok(serde_json::json!({ "baselineSymbols": baseline_symbols }))
}

pub fn cmd_breaking_changes(baseline_app: Option<&str>, json: bool) -> ExitCode {
    let Some(baseline_app) = baseline_app else {
        let reason = "No --baseline-app supplied; breaking-change analysis was not evaluated";
        if json {
            print_json(&serde_json::json!({
                "analysis": "breaking",
                "evaluated": false,
                "reason": reason,
                "changes": [],
            }));
        } else {
            println!("Not evaluated: {reason}.");
        }
        return ExitCode::FAILURE;
    };
    let params = match baseline_params_from_app(baseline_app) {
        Ok(params) => params,
        Err(error) => return report_error(&error, json),
    };
    run_command_with_exit(
        "breaking",
        Some(params),
        json,
        None,
        |result| {
            let changes = result.as_array().cloned().unwrap_or_default();
            if changes.is_empty() {
                println!("No breaking changes detected (against {baseline_app}).");
            } else {
                for c in &changes {
                    let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let object = c.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let description = c.get("description").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("[{kind}] \"{object}\": {description}");
                }
                eprintln!("\n{} breaking change(s)", changes.len());
            }
        },
        array_findings_exit_code,
    )
}

pub fn cmd_arch_lint(json: bool) -> ExitCode {
    run_command_with_exit(
        "arch.lint",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let violations = result.as_array().cloned().unwrap_or_default();
            if violations.is_empty() {
                println!("No architecture violations found.");
            } else {
                for v in &violations {
                    let rule = v.get("ruleId").and_then(|v| v.as_str()).unwrap_or("?");
                    let object = v.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let message = v.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("[{rule}] {object}: {message}");
                }
                eprintln!("\n{} architecture violation(s)", violations.len());
            }
        },
        array_findings_exit_code,
    )
}

pub fn cmd_native_check(json: bool) -> ExitCode {
    run_command_with_exit(
        "nativeCheck",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let findings = result.as_array().cloned().unwrap_or_default();
            if findings.is_empty() {
                println!("No native semantic issues found.");
            } else {
                for f in &findings {
                    let code = f.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                    let sev = f.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
                    let otype = f.get("objectType").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = f.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let msg = f.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                    let file = f.get("file").and_then(|v| v.as_str()).unwrap_or("");
                    println!("[{code}] {sev} {otype} \"{name}\": {msg}");
                    if !file.is_empty() {
                        println!("    {file}");
                    }
                }
                eprintln!("\n{} native semantic finding(s)", findings.len());
            }
        },
        |result| {
            if result.as_array().is_some_and(|findings| {
                findings.iter().any(|finding| {
                    finding
                        .get("severity")
                        .and_then(|value| value.as_str())
                        .is_some_and(|severity| severity.eq_ignore_ascii_case("error"))
                })
            }) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        },
    )
}

fn format_block_location(loc: Option<&serde_json::Value>) -> String {
    let Some(loc) = loc else {
        return "?".to_string();
    };
    let file = loc.get("file").and_then(|v| v.as_str()).unwrap_or("?");
    let proc = loc.get("procedure").and_then(|v| v.as_str()).unwrap_or("?");
    let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
    format!("{file}:{line} ({proc})")
}

pub fn cmd_duplicates(min_tokens: usize, min_similarity: f32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(
        &mut client,
        "duplicates",
        Some(serde_json::json!({
            "minTokens": min_tokens,
            "minSimilarity": min_similarity,
        })),
    ) {
        Ok(result) => {
            let has_duplicates = result
                .as_array()
                .is_some_and(|duplicates| !duplicates.is_empty());
            if json {
                print_json(&result);
            } else {
                let dups = result.as_array().cloned().unwrap_or_default();
                if dups.is_empty() {
                    println!("No duplicate code blocks found.");
                } else {
                    for d in &dups {
                        let sim = d.get("similarity").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let loc1 = format_block_location(d.get("first"));
                        let loc2 = format_block_location(d.get("second"));
                        println!("{:.0}% similarity: {} ~ {}", sim * 100.0, loc1, loc2);
                    }
                    eprintln!("\n{} duplicate block(s)", dups.len());
                }
            }
            if has_duplicates {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_upgrade_report(baseline_app: Option<&str>, json: bool) -> ExitCode {
    let Some(baseline_app) = baseline_app else {
        let reason = "No --baseline-app supplied; upgrade analysis was not evaluated";
        if json {
            print_json(&serde_json::json!({
                "analysis": "upgrade",
                "evaluated": false,
                "reason": reason,
                "issues": [],
            }));
        } else {
            println!("Not evaluated: {reason}.");
        }
        return ExitCode::FAILURE;
    };
    let params = match baseline_params_from_app(baseline_app) {
        Ok(params) => params,
        Err(error) => return report_error(&error, json),
    };
    run_command_with_exit(
        "upgrade",
        Some(params),
        json,
        None,
        |result| {
            let issues = result.as_array().cloned().unwrap_or_default();
            if issues.is_empty() {
                println!("No upgrade issues found (against {baseline_app}).");
            } else {
                for i in &issues {
                    let kind = i.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let object = i.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                    let description = i.get("description").and_then(|v| v.as_str()).unwrap_or("?");
                    let hint = i
                        .get("migrationHint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("[{kind}] \"{object}\": {description}");
                    if !hint.is_empty() {
                        println!("  Migration: {hint}");
                    }
                }
                eprintln!("\n{} upgrade issue(s)", issues.len());
            }
        },
        array_findings_exit_code,
    )
}

fn array_findings_exit_code(result: &serde_json::Value) -> ExitCode {
    if result
        .as_array()
        .is_some_and(|findings| !findings.is_empty())
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod exit_status_tests {
    use super::*;

    #[test]
    fn array_quality_gates_fail_on_findings() {
        assert_eq!(
            array_findings_exit_code(&serde_json::json!([])),
            ExitCode::SUCCESS
        );
        assert_eq!(
            array_findings_exit_code(&serde_json::json!([{"kind": "finding"}])),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn cross_version_checks_without_a_baseline_are_not_green() {
        assert_eq!(cmd_breaking_changes(None, true), ExitCode::FAILURE);
        assert_eq!(cmd_upgrade_report(None, true), ExitCode::FAILURE);
    }
}
