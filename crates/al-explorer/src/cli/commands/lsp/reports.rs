//! Workspace analysis & generation: object generate, obsolete/audit/permission reports, dependency graph, breaking-change/arch lint, and duplicate detection.

use std::fmt::Write as _;
use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_generate(
    kind: &str,
    id: Option<i64>,
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
    let mut params = serde_json::json!({ "kind": kind, "name": name });
    if let Some(id) = id {
        params["id"] = serde_json::json!(id);
    }
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
                print!("{}", terminal_lines(code));
                for warning in result["warnings"].as_array().into_iter().flatten() {
                    if let Some(warning) = warning.as_str() {
                        eprintln!("warning: {}", terminal_text(warning));
                    }
                }
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
            let entries = list_rows(result).as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("No obsolete symbols found.");
            } else {
                print!("{}", obsolete_text(&entries));
                eprintln!("\n{} obsolete symbol(s)", entries.len());
            }
        },
    )
}

/// The lines `obsolete` prints, one per obsolete symbol.
fn obsolete_text(entries: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for e in entries {
        let kind = text_field(e, "kind", "?");
        let object = text_field(e, "object", "?");
        let symbol = text_field(e, "symbol", "?");
        let reason = text_field(e, "reason", "");
        let state = text_field(e, "state", "");
        let _ = writeln!(out, "{kind} {object}::{symbol} [{state}]: {reason}");
    }
    out
}

pub fn cmd_obsolete_usages(json: bool) -> ExitCode {
    run_command(
        "obsoleteUsages",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let findings = list_rows(result).as_array().cloned().unwrap_or_default();
            if findings.is_empty() {
                println!("No calls to obsolete procedures found.");
                return;
            }
            for finding in &findings {
                let file = text_field(finding, "file", "?");
                let line = finding["range"]["start"]["line"]
                    .as_u64()
                    .map_or(0, |l| l + 1);
                let column = finding["range"]["start"]["character"]
                    .as_u64()
                    .map_or(0, |c| c + 1);
                let message = text_field(finding, "message", "");
                println!("{file}:{line}:{column}: {message}");
            }
            eprintln!("\n{} call(s) to obsolete procedures", findings.len());
        },
    )
}

pub fn cmd_package_diff(from: &str, to: &str, all: bool, json: bool) -> ExitCode {
    run_command(
        "packageDiff",
        Some(serde_json::json!({ "from": from, "to": to, "all": all })),
        json,
        None,
        |result| {
            println!("{}", package_diff_summary_line(result));
            if let Some(warning) = result["warning"].as_str() {
                eprintln!("warning: {}", terminal_text(warning));
            }
            print!("{}", package_diff_changes_text(result));
        },
    )
}

/// The first line `package-diff` prints: the two packages and the counts.
fn package_diff_summary_line(result: &serde_json::Value) -> String {
    let side_text = |side: &str| {
        let package = &result[side];
        format!(
            "{} {}",
            text_field(package, "name", "?"),
            text_field(package, "version", "?")
        )
    };
    format!(
        "{} -> {}: {} change(s), {} breaking, {} used by this workspace, {} possibly",
        side_text("from"),
        side_text("to"),
        result["totalChanges"].as_u64().unwrap_or(0),
        result["breakingChanges"].as_u64().unwrap_or(0),
        result["affectingWorkspace"].as_u64().unwrap_or(0),
        result["possiblyAffecting"].as_u64().unwrap_or(0),
    )
}

/// Each change `package-diff` prints, with the workspace code that uses it.
fn package_diff_changes_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    for change in list_rows(&result["changes"])
        .as_array()
        .into_iter()
        .flatten()
    {
        let object = text_field(change, "object", "?");
        let target = match change["member"].as_str() {
            Some(member) => format!("{object}.{}", terminal_text(member)),
            None => object,
        };
        // Table and page "Payment Terms" share a name.
        let target = match change["objectKind"].as_str() {
            Some(kind) => format!("{} {target}", terminal_text(kind)),
            None => target,
        };
        let severity = if change["isBreaking"].as_bool().unwrap_or(false) {
            "breaking"
        } else {
            "warning"
        };
        let _ = writeln!(
            out,
            "\n[{severity}] {target}: {}",
            text_field(change, "description", "")
        );
        for (key, prefix) in [("uses", ""), ("possibleUses", "possibly: ")] {
            for used in change[key].as_array().into_iter().flatten() {
                let kind = text_field(used, "k", "?");
                let name = text_field(used, "n", "?");
                let how = text_field(used, "type", "?");
                let procedure = text_field(used, "proc", "");
                if used["proc"].is_string() {
                    let _ = writeln!(out, "    {prefix}{kind} {name}, {procedure} ({how})");
                } else {
                    let _ = writeln!(out, "    {prefix}{kind} {name} ({how})");
                }
            }
        }
    }
    out
}

pub fn cmd_audit_data_classification(json: bool) -> ExitCode {
    run_command_with_exit(
        "audit.dataClassification",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let entries = list_rows(result).as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("No table fields found to audit.");
            } else {
                let mut unclassified = 0usize;
                for e in &entries {
                    let table = text_field(e, "table", "?");
                    let field = text_field(e, "field", "?");
                    let classification = text_field(e, "classification", "?");
                    let risk = text_field(e, "risk", "?");
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
            if list_rows(result).as_array().is_some_and(|entries| {
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
                    let kind = text_field(e, "kind", "?");
                    let name = text_field(e, "name", "?");
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
                    let set = text_field(e, "permissionSet", "?");
                    let ot = text_field(e, "objectType", "?");
                    let obj = text_field(e, "object", "?");
                    let rights = text_field(e, "rights", "");
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
                    let set = text_field(e, "permissionSet", "?");
                    let obj = text_field(e, "object", "?");
                    let granted = text_field(e, "grantedRights", "");
                    let over = text_field(e, "overGranted", "");
                    let observed = text_field(e, "observedRights", "");
                    println!(
                        "  {set}: TableData \"{obj}\" = {granted} — only {observed} observed; \
                         drop {over}"
                    );
                }
                eprintln!("\n{} over-granted right(s)", over_granted.len());
            }

            // A clause the audit could not read took part in no check, so the
            // sets above are answers about less code than the user has.
            let parse_issues = result
                .get("parseIssues")
                .and_then(|v| v.as_array())
                .map(|entries| &entries[..])
                .unwrap_or_default();
            if !parse_issues.is_empty() {
                println!("\nGrant clauses the audit could not read (excluded from every check):");
                for issue in parse_issues {
                    let set = text_field(issue, "permissionSet", "?");
                    let file = text_field(issue, "file", "?");
                    let clause = issue.get("clause").and_then(|v| v.as_u64()).unwrap_or(0);
                    let text = text_field(issue, "text", "");
                    let reason = text_field(issue, "reason", "?");
                    println!("  {set} ({file}), clause {clause}: {text} — {reason}");
                }
                eprintln!("\n{} unreadable grant clause(s)", parse_issues.len());
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
                print!("{}", terminal_lines(dot));
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Ok(manifest) = al_project::project::load_app_manifest(&cwd)
        && let Some(warning) = different_app_warning(&pkg.app_id, &pkg.name, &manifest)
    {
        eprintln!("warning: {}", terminal_text(&warning));
    }
    let baseline_symbols = serde_json::to_value(&pkg.objects)
        .map_err(|e| format!("serializing baseline symbols: {e}"))?;
    Ok(serde_json::json!({ "baselineSymbols": baseline_symbols }))
}

/// A warning when the baseline `.app` is not an earlier build of this
/// project. Comparing another app's surface reports its whole API as
/// removed, thousands of "breaking changes" with nothing wrong.
fn different_app_warning(
    baseline_id: &str,
    baseline_name: &str,
    project: &al_project::project::AppManifest,
) -> Option<String> {
    let normalize = |id: &str| {
        id.trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase()
    };
    if baseline_id.is_empty() || normalize(baseline_id) == normalize(&project.id) {
        return None;
    }
    Some(format!(
        "the baseline is \"{baseline_name}\" ({baseline_id}), not an earlier build of \"{}\" ({}); \
         every object it has that this app lacks is reported as removed. To compare \
         two versions of a dependency, use `package-diff`.",
        project.name, project.id
    ))
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
            let changes = list_rows(result).as_array().cloned().unwrap_or_default();
            if changes.is_empty() {
                println!(
                    "No breaking changes detected (against {}).",
                    terminal_text(baseline_app)
                );
            } else {
                for c in &changes {
                    let kind = text_field(c, "kind", "?");
                    let object = text_field(c, "object", "?");
                    let description = text_field(c, "description", "?");
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
            let violations = list_rows(result).as_array().cloned().unwrap_or_default();
            if violations.is_empty() {
                println!("No architecture violations found.");
            } else {
                for v in &violations {
                    let rule = text_field(v, "ruleId", "?");
                    let object = text_field(v, "object", "?");
                    let message = text_field(v, "message", "?");
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
            let findings = list_rows(result).as_array().cloned().unwrap_or_default();
            if findings.is_empty() {
                println!("No native semantic issues found.");
            } else {
                print!("{}", native_check_text(&findings));
                eprintln!("\n{} native semantic finding(s)", findings.len());
            }
        },
        |result| {
            if list_rows(result).as_array().is_some_and(|findings| {
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

/// The lines `native-check` prints: each finding, and its file on the next
/// line.
fn native_check_text(findings: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for f in findings {
        let code = text_field(f, "code", "?");
        let sev = text_field(f, "severity", "?");
        let otype = text_field(f, "objectType", "?");
        let name = text_field(f, "objectName", "?");
        let msg = text_field(f, "message", "?");
        let file = text_field(f, "file", "");
        let _ = writeln!(out, "[{code}] {sev} {otype} \"{name}\": {msg}");
        if !file.is_empty() {
            let _ = writeln!(out, "    {file}");
        }
    }
    out
}

fn block_location_text(loc: Option<&serde_json::Value>) -> String {
    let Some(loc) = loc else {
        return "?".to_string();
    };
    let file = text_field(loc, "file", "?");
    let proc = text_field(loc, "procedure", "?");
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
                let dups = list_rows(&result).as_array().cloned().unwrap_or_default();
                if dups.is_empty() {
                    println!("No duplicate code blocks found.");
                } else {
                    for d in &dups {
                        let sim = d.get("similarity").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let loc1 = block_location_text(d.get("first"));
                        let loc2 = block_location_text(d.get("second"));
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
            let issues = list_rows(result).as_array().cloned().unwrap_or_default();
            if issues.is_empty() {
                println!(
                    "No upgrade issues found (against {}).",
                    terminal_text(baseline_app)
                );
            } else {
                for i in &issues {
                    let kind = text_field(i, "kind", "?");
                    let object = text_field(i, "object", "?");
                    let description = text_field(i, "description", "?");
                    let hint = text_field(i, "migrationHint", "");
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

#[cfg(test)]
mod baseline_tests {
    use super::different_app_warning;

    fn manifest(id: &str) -> al_project::project::AppManifest {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "My App", "publisher": "Me", "version": "1.0.0.0"
        }))
        .unwrap()
    }

    #[test]
    fn an_earlier_build_of_the_same_app_is_not_warned_about() {
        let project = manifest("0a1b2c3d-0000-0000-0000-000000000001");
        assert_eq!(
            different_app_warning("{0A1B2C3D-0000-0000-0000-000000000001}", "My App", &project),
            None
        );
    }

    #[test]
    fn another_app_as_baseline_is_warned_about() {
        let project = manifest("0a1b2c3d-0000-0000-0000-000000000001");
        let warning = different_app_warning(
            "437dbf0e-84ff-417a-965d-ed2bb9650972",
            "Base Application",
            &project,
        )
        .expect("a different app id warns");
        assert!(warning.contains("Base Application"), "{warning}");
        assert!(warning.contains("package-diff"), "{warning}");
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{block_location_text, native_check_text, obsolete_text, package_diff_changes_text};

    const COLOURED: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const TITLE: &str = "Sec7 Caller\u{1b}]0;pwned\u{7}";
    const CLEAR: &str = "Sec7 Tests\u{1b}[2J";

    fn assert_escaped(text: &str) {
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
        assert!(text.contains(r"Bad\u{1b}[31m Name\u{1b}[0m"), "got: {text}");
        assert!(
            text.contains(r"Sec7 Caller\u{1b}]0;pwned\u{7}"),
            "got: {text}"
        );
        assert!(text.contains(r"Sec7 Tests\u{1b}[2J"), "got: {text}");
    }

    #[test]
    fn obsolete_native_check_package_diff_and_duplicates_print_crafted_names_escaped() {
        let obsolete = [serde_json::json!({
            "kind": "procedure", "object": COLOURED, "symbol": TITLE,
            "state": "Pending", "reason": CLEAR
        })];
        assert_escaped(&obsolete_text(&obsolete));

        let findings = [serde_json::json!({
            "code": "AL0118", "severity": "error", "objectType": "Codeunit",
            "objectName": COLOURED, "message": TITLE, "file": CLEAR
        })];
        let text = native_check_text(&findings);
        assert_escaped(&text);
        assert!(
            text.contains(r#"[AL0118] error Codeunit "Bad\u{1b}[31m Name\u{1b}[0m""#),
            "got: {text}"
        );

        let diff = serde_json::json!({"changes": [{
            "object": COLOURED, "member": "Run", "objectKind": "Codeunit",
            "isBreaking": true, "description": "removed",
            "uses": [{"k": "Codeunit", "n": TITLE, "type": "call", "proc": CLEAR}]
        }]});
        assert_escaped(&package_diff_changes_text(&diff));

        let location = serde_json::json!({"file": CLEAR, "procedure": TITLE, "line": 4});
        let text = format!(
            "{} {}",
            block_location_text(Some(&location)),
            block_location_text(Some(&serde_json::json!({"file": COLOURED})))
        );
        assert_escaped(&text);
    }
}
