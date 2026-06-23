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
    match client.request("generate", Some(params)) {
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
    run_command(
        "audit.dataClassification",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let entries = result.as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("All table fields have DataClassification set.");
            } else {
                for e in &entries {
                    let table = e.get("table").and_then(|v| v.as_str()).unwrap_or("?");
                    let field = e.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                    let dc = e
                        .get("dataClassification")
                        .and_then(|v| v.as_str())
                        .unwrap_or("missing");
                    println!("{table}.{field}: {dc}");
                }
                eprintln!("\n{} field(s) missing DataClassification", entries.len());
            }
        },
    )
}

pub fn cmd_permission_audit(json: bool) -> ExitCode {
    run_command(
        "permissions.audit",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let entries = result.as_array().cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("All objects covered by permission sets.");
            } else {
                for e in &entries {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let covered = e.get("covered").and_then(|v| v.as_bool()).unwrap_or(false);
                    let status = if covered { "covered" } else { "MISSING" };
                    println!("{kind} \"{name}\": {status}");
                }
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
    match client.request(
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

pub fn cmd_breaking_changes(json: bool) -> ExitCode {
    run_command(
        "breaking",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let changes = result.as_array().cloned().unwrap_or_default();
            if changes.is_empty() {
                println!("No breaking changes detected.");
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
    )
}

pub fn cmd_arch_lint(json: bool) -> ExitCode {
    run_command(
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
    match client.request(
        "duplicates",
        Some(serde_json::json!({
            "minTokens": min_tokens,
            "minSimilarity": min_similarity,
        })),
    ) {
        Ok(result) => {
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
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_upgrade_report(json: bool) -> ExitCode {
    run_command(
        "upgrade",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let issues = result.as_array().cloned().unwrap_or_default();
            if issues.is_empty() {
                println!("No upgrade issues found.");
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
    )
}
