//! Project-level commands: rules, permissions, error codes, builtins, parse, inlay hints, fixes, authentication, debug-config init, and scaffolding.

use std::fmt::Write as _;
use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_rules(json: bool) -> ExitCode {
    run_command("rules", None, json, None, |result| {
        let rules = list_rows(result).as_array().map(|v| &v[..]).unwrap_or(&[]);
        if rules.is_empty() {
            eprintln!(
                "No native lint rules are registered. Semantic AL diagnostics \
                 (CodeCop, AppSourceCop, UICop, PerTenantCop) are produced by the \
                 Microsoft analyzers via the compiler bridge, not this native registry."
            );
            return;
        }
        print!("{}", rules_table_text(rules));
        eprintln!("\n{} rules", rules.len());
    })
}

/// The table `rules` prints, a header and one row per rule.
fn rules_table_text(rules: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<10} {:<8} {:<25} DESCRIPTION",
        "CODE", "SEV", "NAME"
    );
    let _ = writeln!(out, "{}", "-".repeat(80));
    for r in rules {
        let code = text_field(r, "code", "?");
        let sev = text_field(r, "severity", "?");
        let name = text_field(r, "name", "?");
        let desc = text_field(r, "description", "?");
        let _ = writeln!(out, "{:<10} {:<8} {:<25} {}", code, sev, name, desc);
    }
    out
}

pub fn cmd_permissions(format: &str, name: &str, id: i64, role_id: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({
        "format": format,
        "name": name,
        "id": id,
        "roleId": role_id,
    });
    match request_checked(&mut client, "permissions", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let count = result
                    .get("objectCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                print!("{}", terminal_lines(content));
                eprintln!("\n{count} objects included");
                // A file the collector could not read contributes no
                // permissions. Saying nothing hands back a set that silently
                // omits that object.
                let skipped = result
                    .get("skipped")
                    .and_then(|value| value.as_array())
                    .map(|entries| &entries[..])
                    .unwrap_or_default();
                eprint!("{}", skipped_files_text(skipped));
                if !skipped.is_empty() {
                    eprintln!(
                        "{} file(s) contributed no permissions; the set does not cover them",
                        skipped.len()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The lines `permissions` prints for the files it could not read.
fn skipped_files_text(skipped: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for entry in skipped {
        let path = text_field(entry, "path", "?");
        let reason = text_field(entry, "reason", "?");
        let _ = writeln!(out, "skipped {path}: {reason}");
    }
    out
}

pub fn cmd_error_codes(json: bool) -> ExitCode {
    run_command("errorCodes", None, json, None, |result| {
        let codes = list_rows(result).as_array().map(|v| &v[..]).unwrap_or(&[]);
        if codes.is_empty() {
            eprintln!("No error codes loaded (requires ALTool)");
        } else {
            print!("{}", error_codes_text(codes));
            eprintln!("\n{} error codes", codes.len());
        }
    })
}

/// The lines `error-codes` prints, one per code.
fn error_codes_text(codes: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for c in codes {
        let code = text_field(c, "code", "?");
        let desc = text_field(c, "description", "?");
        let _ = writeln!(out, "{code}: {desc}");
    }
    out
}

pub fn cmd_builtins(json: bool) -> ExitCode {
    run_command("builtinTypes", None, json, None, |result| {
        let types = list_rows(result).as_array().map(|v| &v[..]).unwrap_or(&[]);
        if types.is_empty() {
            eprintln!("No builtin types loaded (requires ALTool)");
        } else {
            print!("{}", builtins_text(types));
            eprintln!("\n{} builtin types", types.len());
        }
    })
}

/// The lines `builtins` prints, one per type with its method count.
fn builtins_text(types: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for t in types {
        let name = text_field(t, "name", "?");
        let methods = t
            .get("methods")
            .and_then(|v| v.as_array())
            .map(|m| m.len())
            .unwrap_or(0);
        let _ = writeln!(out, "{name} ({methods} methods)");
    }
    out
}

pub fn cmd_parse(file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let params = serde_json::json!({ "uri": uri });
    match request_checked(&mut client, "parse", Some(params)) {
        Ok(result) => {
            let errors = match parse_error_count(&result) {
                Some(errors) => errors,
                None => {
                    return report_error(
                        "validated parse response has no non-negative integer error count",
                        json,
                    );
                }
            };
            if json {
                print_json(&result);
            } else {
                print!("{}", parse_summary_text(file, &result, errors));
                eprint!("{}", parse_errors_text(file, &result));
            }
            if errors > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn parse_error_count(result: &serde_json::Value) -> Option<u64> {
    result.get("errors").and_then(|value| value.as_u64())
}

/// The line `parse` prints: the file, its node and error counts and the
/// parse time.
fn parse_summary_text(file: &str, result: &serde_json::Value, errors: u64) -> String {
    let nodes = result
        .get("nodeCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let time = result
        .get("parseTimeMs")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let file = terminal_text(file);
    format!("{file}: {nodes} nodes, {errors} errors, {time:.1}ms\n")
}

/// The lines `parse` prints for each syntax error.
fn parse_errors_text(file: &str, result: &serde_json::Value) -> String {
    let mut out = String::new();
    let file = terminal_text(file);
    for e in result
        .get("parseErrors")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let line = e.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let col = e.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
        let msg = text_field(e, "message", "?");
        let _ = writeln!(out, "  {file}:{line}:{col}: {msg}");
    }
    out
}

pub fn cmd_hints(
    file: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let uri = match file_to_uri(file) {
        Ok(uri) => uri,
        Err(error) => return report_error(&error, json),
    };
    let mut params = serde_json::json!({ "uri": uri });
    if let Some(s) = start_line {
        params["startLine"] = serde_json::json!(s.saturating_sub(1));
    }
    if let Some(e) = end_line {
        params["endLine"] = serde_json::json!(e.saturating_sub(1));
    }
    match request_checked(&mut client, "inlayHints", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let hints = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
                if hints.is_empty() {
                    eprintln!("No inlay hints");
                } else {
                    print!("{}", inlay_hints_text(hints));
                    eprintln!("\n{} hints", hints.len());
                    print_page_footer(&result);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The lines `hints` prints, `line:col label`, 1-based like the command's
/// input.
fn inlay_hints_text(hints: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for hint in hints {
        let line = hint["position"]["line"].as_u64().map_or(0, |l| l + 1);
        let col = hint["position"]["character"].as_u64().map_or(0, |c| c + 1);
        let label = text_field(hint, "label", "?");
        let _ = writeln!(out, "{line}:{col}  {label}");
    }
    out
}

pub fn cmd_fix(file: Option<&str>, dry_run: bool, rule: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "dryRun": dry_run });
    if let Some(f) = file {
        let uri = match file_to_uri(f) {
            Ok(uri) => uri,
            Err(error) => return report_error(&error, json),
        };
        params["uri"] = serde_json::json!(uri);
    }
    if let Some(r) = rule {
        params["rule"] = serde_json::json!(r);
    }
    match request_checked(&mut client, "fix", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let diag_count = result
                    .get("diagnostics")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let fix_count = result.get("fixes").and_then(|v| v.as_u64()).unwrap_or(0);
                let is_dry = result
                    .get("dryRun")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_dry {
                    eprintln!("{diag_count} diagnostics, {fix_count} fixable (dry run)");
                } else {
                    eprintln!("{diag_count} diagnostics, {fix_count} fixed");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_authenticate(cmd: &str, tenant: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    if cmd == "login" {
        client.set_request_timeout(std::time::Duration::from_secs(120));
    }

    let mut params = serde_json::json!({ "cmd": cmd });
    if let Some(t) = tenant {
        params["tenant"] = serde_json::json!(t);
    }

    match request_checked(&mut client, "authenticate", Some(params)) {
        Ok(result) => {
            let usable = cmd != "status" || any_tenant_authenticated(&result);
            if json {
                print_json(&result);
            } else {
                match cmd {
                    "status" => {
                        if let Some(tenants) = result.get("tenants").and_then(|v| v.as_array()) {
                            if tenants.is_empty() {
                                eprintln!("No tenants configured in this project.");
                            }
                            eprint!("{}", tenant_status_text(tenants));
                        }
                    }
                    "clear" => {
                        let cleared = result.get("cleared").and_then(|v| v.as_u64()).unwrap_or(0);
                        eprintln!("Cleared {cleared} cached token(s).");
                    }
                    _ => eprint!("{}", authentication_text(&result)),
                }
            }
            if usable {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => report_error(&e, json),
    }
}

/// The lines `authenticate status` prints, one per tenant.
fn tenant_status_text(tenants: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for t in tenants {
        let id = text_field(t, "tenant", "?");
        let authed = t
            .get("authenticated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let expired = t.get("expired").and_then(|v| v.as_bool()).unwrap_or(true);
        let status = if authed && !expired {
            "authenticated"
        } else if authed && expired {
            "expired"
        } else {
            "not authenticated"
        };
        let _ = writeln!(out, "  {id}: {status}");
    }
    out
}

/// The lines `authenticate login` and `logout` print: the status, the
/// tenant and the daemon's messages.
fn authentication_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    let status = text_field(result, "status", "?");
    let tenant_id = text_field(result, "tenant", "?");
    let _ = writeln!(out, "Status: {status}");
    let _ = writeln!(out, "Tenant: {tenant_id}");
    for msg in result
        .get("messages")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if let Some(s) = msg.as_str().filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "  {}", terminal_text(s));
        }
    }
    out
}

/// Whether at least one tenant has a token that is present and unexpired.
///
/// `authenticate status` printed `not authenticated` for every tenant and
/// still exited 0, so `al authenticate status && al download-symbols --source
/// server` went on to run unauthenticated.
pub(crate) fn any_tenant_authenticated(result: &serde_json::Value) -> bool {
    result
        .get("tenants")
        .and_then(|value| value.as_array())
        .is_some_and(|tenants| {
            tenants.iter().any(|tenant| {
                tenant
                    .get("authenticated")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                    && !tenant
                        .get("expired")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(true)
            })
        })
}

pub fn cmd_init_debug(project_root: &std::path::Path, json: bool) -> ExitCode {
    let zed_dir = project_root.join(".zed");
    let debug_path = zed_dir.join("debug.json");
    let debug_path_display = debug_path.display().to_string();
    if debug_path.exists() {
        if json {
            print_json(&serde_json::json!({"status": "exists", "path": debug_path_display}));
        } else {
            eprintln!(
                "{}: already exists — not overwriting",
                terminal_text(&debug_path_display)
            );
        }
        return ExitCode::SUCCESS;
    }

    if let Err(e) = std::fs::create_dir_all(&zed_dir) {
        let msg = format!("Failed to create .zed directory: {e}");
        return report_error(&msg, json);
    }

    let configs = serde_json::json!([
        {
            "adapter": "al",
            "label": "Publish: Your own server",
            "request": "launch",
            "environmentType": "OnPrem",
            "server": "http://bcserver",
            "serverInstance": "BC",
            "authentication": "MicrosoftEntraID",
            "startupObjectId": 22,
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "launchBrowser": true,
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "tenant": "default"
        },
        {
            "adapter": "al",
            "label": "Publish: Cloud Sandbox",
            "request": "launch",
            "environmentType": "Sandbox",
            "environmentName": "sandbox",
            "startupObjectId": 22,
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "launchBrowser": true,
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10
        },
        {
            "adapter": "al",
            "label": "Attach: Your own server",
            "request": "attach",
            "environmentType": "OnPrem",
            "server": "http://bcserver",
            "serverInstance": "BC",
            "authentication": "MicrosoftEntraID",
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "breakOnNext": "WebServiceClient",
            "tenant": "default"
        },
        {
            "adapter": "al",
            "label": "Attach: Cloud Sandbox",
            "request": "attach",
            "environmentType": "Sandbox",
            "environmentName": "sandbox",
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "breakOnNext": "WebServiceClient"
        }
    ]);

    let content = match serde_json::to_string_pretty(&configs) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "Failed to serialize debug configurations: {}",
                terminal_text(&e.to_string())
            );
            return ExitCode::FAILURE;
        }
    };
    match std::fs::write(&debug_path, &content) {
        Ok(_) => {
            if json {
                print_json(&serde_json::json!({
                    "status": "created",
                    "path": debug_path_display,
                    "configurations": 4
                }));
            } else {
                let shown = terminal_text(&debug_path_display);
                eprintln!("Created {shown} with 4 configurations:");
                eprintln!("  - Publish: Your own server (launch)");
                eprintln!("  - Publish: Cloud Sandbox (launch)");
                eprintln!("  - Attach: Your own server (attach)");
                eprintln!("  - Attach: Cloud Sandbox (attach)");
                eprintln!("\nEdit {shown} to configure server URLs and authentication.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(
            &format!("Failed to write {}: {e}", debug_path.display()),
            json,
        ),
    }
}

/// Scaffold a project in `dir`, in this process.
///
/// Creating a project is the one command that runs where no project exists
/// yet. It used to go through the daemon, whose path containment refuses every
/// path while no project is loaded, so `al-explorer new` failed everywhere
/// except inside another AL project.
pub fn cmd_new(
    dir: &str,
    name: &str,
    publisher: &str,
    template: &str,
    runtime: &str,
    json: bool,
) -> ExitCode {
    use al_project::scaffold;

    let directory = match absolutize_path(dir) {
        Ok(directory) => directory,
        Err(error) => return report_error(&error, json),
    };
    let template = match template.parse::<scaffold::ProjectTemplate>() {
        Ok(template) => template,
        Err(error) => return report_error(&error, json),
    };
    if let Err(error) = scaffold::application_version_for_runtime(runtime) {
        return report_error(&error, json);
    }
    let config = scaffold::ScaffoldConfig {
        name: name.to_string(),
        publisher: publisher.to_string(),
        runtime: runtime.to_string(),
        template,
        ..scaffold::ScaffoldConfig::default()
    };

    match scaffold::create_project(std::path::Path::new(&directory), &config) {
        Ok(result) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            } else {
                print!(
                    "{}",
                    new_project_text(&result.project_dir, &result.files_created)
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error, json),
    }
}

/// The text `new` prints: the project directory and each file it wrote.
fn new_project_text(project_dir: &str, files: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Created AL project: {}", terminal_text(project_dir));
    for file in files {
        let _ = writeln!(out, "  {}", terminal_text(file));
    }
    out
}

pub fn cmd_free_ids(
    kind: Option<&str>,
    object: Option<&str>,
    count: u32,
    include_used: bool,
    json: bool,
) -> ExitCode {
    let mut params = serde_json::Map::new();
    if let Some(kind) = kind {
        params.insert("kind".to_string(), serde_json::json!(kind));
    }
    if let Some(object) = object {
        params.insert("object".to_string(), serde_json::json!(object));
    }
    params.insert("count".to_string(), serde_json::json!(count));
    if include_used {
        params.insert("includeUsed".to_string(), serde_json::json!(true));
    }
    run_command(
        "freeIds",
        Some(serde_json::Value::Object(params)),
        json,
        None,
        print_free_ids,
    )
}

/// Human-readable form of a `freeIds` report. The JSON path prints the result
/// unchanged; this is the only place that reshapes it.
fn print_free_ids(result: &serde_json::Value) {
    print!("{}", free_ids_text(result));
    let mode = result.get("mode").and_then(|value| value.as_str());
    let partial = mode.is_some_and(|mode| mode != "summary")
        && result["truncated"].as_bool().unwrap_or(false);
    if partial {
        eprintln!("Fewer numbers are left than were requested.");
    }
    for warning in result["warnings"].as_array().unwrap_or(&Vec::new()) {
        if let Some(warning) = warning.as_str() {
            eprintln!("warning: {}", terminal_text(warning));
        }
    }
}

/// The text `free-ids` prints for a report: the ranges and, per mode, the
/// next free number and the numbers counted.
fn free_ids_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    let number_list = |value: &serde_json::Value| {
        value
            .as_array()
            .map(|numbers| {
                numbers
                    .iter()
                    .filter_map(serde_json::Value::as_i64)
                    .map(|number| number.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };

    match result.get("mode").and_then(|value| value.as_str()) {
        Some("summary") => {
            let _ = writeln!(out, "Declared idRanges:");
            for range in result["ranges"].as_array().unwrap_or(&Vec::new()) {
                let _ = writeln!(
                    out,
                    "  {}-{} ({} IDs per kind)",
                    range["from"].as_i64().unwrap_or(0),
                    range["to"].as_i64().unwrap_or(0),
                    range["free"].as_i64().unwrap_or(0)
                );
            }
            let kinds = result["kinds"].as_array().cloned().unwrap_or_default();
            if kinds.is_empty() {
                let _ = writeln!(out, "No object in the workspace uses a declared range yet.");
            } else {
                let _ = writeln!(
                    out,
                    "\n{:<24} {:>6} {:>6} {:>10}",
                    "kind", "used", "free", "next free"
                );
                for row in &kinds {
                    let _ = writeln!(
                        out,
                        "{:<24} {:>6} {:>6} {:>10}",
                        text_field(row, "kind", "?"),
                        row["used"].as_i64().unwrap_or(0),
                        row["free"].as_i64().unwrap_or(0),
                        row["nextFree"]
                            .as_i64()
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    );
                }
            }
        }
        Some(mode) => {
            let what = match mode {
                "field" => "field number",
                "value" => "enum value ordinal",
                _ => "object ID",
            };
            let subject = terminal_text(
                result["object"]
                    .as_str()
                    .or_else(|| result["kind"].as_str())
                    .unwrap_or("?"),
            );
            let _ = writeln!(
                out,
                "Next free {what} for {subject}: {}",
                result["nextFree"]
                    .as_i64()
                    .map(|number| number.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            let free = number_list(&result["free"]);
            if result["free"].as_array().is_some_and(|list| list.len() > 1) {
                let _ = writeln!(out, "Free: {free}");
            }
            if let Some(base) = result["baseObject"].as_str() {
                let _ = writeln!(
                    out,
                    "Shares numbering with base object: {}",
                    terminal_text(base)
                );
            }
            for range in result["ranges"].as_array().unwrap_or(&Vec::new()) {
                let _ = writeln!(
                    out,
                    "Range {}-{}: {} used, {} free",
                    range["from"].as_i64().unwrap_or(0),
                    range["to"].as_i64().unwrap_or(0),
                    range["used"].as_i64().unwrap_or(0),
                    range["free"].as_i64().unwrap_or(0),
                );
            }
            if result["ranges"]
                .as_array()
                .is_none_or(|ranges| ranges.is_empty())
            {
                let _ = writeln!(out, "Used: {}", result["usedCount"].as_i64().unwrap_or(0));
            }
            let sources = result["sources"].as_array().cloned().unwrap_or_default();
            if sources.len() > 1 {
                let names: Vec<String> = sources
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(terminal_text)
                    .collect();
                let _ = writeln!(out, "Counted from: {}", names.join(", "));
            }
            if !result["used"].as_array().unwrap_or(&Vec::new()).is_empty() {
                let _ = writeln!(out, "Used numbers: {}", number_list(&result["used"]));
            }
        }
        None => {
            let _ = writeln!(out, "The daemon returned no free-ID mode.");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The human formatter must not panic on any shape the daemon can send,
    /// including the modes that carry no `ranges` and the summary that carries
    /// no `nextFree`.
    #[test]
    fn free_ids_formatter_handles_every_report_shape() {
        for report in [
            serde_json::json!({"mode": "summary", "usedCount": 0, "ranges": []}),
            serde_json::json!({
                "mode": "summary",
                "usedCount": 2,
                "ranges": [{"from": 50100, "to": 50199, "used": 0, "free": 100}],
                "kinds": [{"kind": "table", "used": 2, "free": 98, "nextFree": 50101}],
            }),
            serde_json::json!({
                "mode": "object",
                "kind": "table",
                "ranges": [{"from": 50100, "to": 50199, "used": 2, "free": 98}],
                "nextFree": 50101,
                "free": [50101, 50102],
                "usedCount": 2,
                "freeCount": 98,
                "truncated": true,
            }),
            serde_json::json!({
                "mode": "field",
                "kind": "tableextension",
                "object": "Customer Ext",
                "baseObject": "Customer",
                "ranges": [{"from": 50100, "to": 50199, "used": 1, "free": 99}],
                "nextFree": 50101,
                "free": [50101],
                "usedCount": 1,
                "sources": ["Customer (Base Application)", "Customer Ext"],
                "used": [50100],
            }),
            serde_json::json!({
                "mode": "value",
                "kind": "enum",
                "object": "Work Order Status",
                "nextFree": 2,
                "free": [2],
                "usedCount": 2,
                "warnings": ["app.json declares no idRanges"],
            }),
            serde_json::json!({}),
        ] {
            print_free_ids(&report);
        }
    }

    #[test]
    fn parse_error_count_preserves_failure_information_for_json_mode() {
        assert_eq!(
            parse_error_count(&serde_json::json!({"errors": 0})),
            Some(0)
        );
        assert_eq!(
            parse_error_count(&serde_json::json!({"errors": 3})),
            Some(3)
        );
        assert_eq!(
            parse_error_count(&serde_json::json!({"errors": null})),
            None
        );
    }

    #[test]
    fn init_debug_emits_only_native_supported_configuration() {
        let project = tempfile::tempdir().expect("temporary project");
        assert_eq!(cmd_init_debug(project.path(), false), ExitCode::SUCCESS);

        let content =
            std::fs::read_to_string(project.path().join(".zed/debug.json")).expect("debug.json");
        let configs: Vec<serde_json::Value> =
            serde_json::from_str(&content).expect("valid debug configuration");
        assert_eq!(configs.len(), 4);
        for config in &configs {
            assert!(config.get("build").is_none());
            assert!(config.get("usePublicURLFromServer").is_none());
            assert!(config.get("useMcpServerForDebugging").is_none());
        }
        for config in configs.iter().filter(|config| {
            config.get("environmentType").and_then(|v| v.as_str()) == Some("OnPrem")
        }) {
            assert_eq!(
                config.get("authentication").and_then(|v| v.as_str()),
                Some("MicrosoftEntraID")
            );
        }
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{
        authentication_text, builtins_text, error_codes_text, free_ids_text, inlay_hints_text,
        new_project_text, parse_errors_text, parse_summary_text, rules_table_text,
        skipped_files_text, tenant_status_text,
    };

    const CRAFTED_NAME: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const ESCAPED_NAME: &str = r"Bad\u{1b}[31m Name\u{1b}[0m";

    fn assert_escaped(text: &str) {
        assert!(
            !text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character reached the terminal text: {text:?}"
        );
        assert!(text.contains(ESCAPED_NAME), "got: {text}");
    }

    #[test]
    fn rules_error_codes_builtins_and_hints_print_a_crafted_name_escaped() {
        let rules = [serde_json::json!({
            "code": "AL0001", "severity": "warning", "name": CRAFTED_NAME, "description": "d"
        })];
        assert_escaped(&rules_table_text(&rules));

        let codes = [serde_json::json!({"code": "AL0118", "description": CRAFTED_NAME})];
        assert_eq!(
            error_codes_text(&codes),
            format!("AL0118: {ESCAPED_NAME}\n")
        );

        let types = [serde_json::json!({"name": CRAFTED_NAME, "methods": [{}, {}]})];
        assert_eq!(
            builtins_text(&types),
            format!("{ESCAPED_NAME} (2 methods)\n")
        );

        let hints = [serde_json::json!({
            "position": {"line": 4, "character": 10}, "label": CRAFTED_NAME
        })];
        assert_eq!(inlay_hints_text(&hints), format!("5:11  {ESCAPED_NAME}\n"));
    }

    #[test]
    fn parse_permissions_and_new_print_crafted_file_names_escaped() {
        let result = serde_json::json!({
            "nodeCount": 12,
            "parseTimeMs": 1.5,
            "parseErrors": [{"line": 3, "column": 7, "message": CRAFTED_NAME}]
        });
        let file = "src/Sec7\u{1b}]0;pwned\u{7}.al";
        assert_eq!(
            parse_summary_text(file, &result, 1),
            "src/Sec7\\u{1b}]0;pwned\\u{7}.al: 12 nodes, 1 errors, 1.5ms\n"
        );
        assert_escaped(&parse_errors_text(file, &result));

        let skipped = [serde_json::json!({"path": CRAFTED_NAME, "reason": "unreadable"})];
        assert_eq!(
            skipped_files_text(&skipped),
            format!("skipped {ESCAPED_NAME}: unreadable\n")
        );

        let text = new_project_text("/tmp/Sec7\u{1b}[2J", &[CRAFTED_NAME.to_string()]);
        assert_escaped(&text);
        assert!(
            text.contains(r"Created AL project: /tmp/Sec7\u{1b}[2J"),
            "got: {text}"
        );
    }

    #[test]
    fn authenticate_prints_tenant_ids_and_messages_escaped() {
        let tenants = [serde_json::json!({
            "tenant": CRAFTED_NAME, "authenticated": true, "expired": false
        })];
        assert_eq!(
            tenant_status_text(&tenants),
            format!("  {ESCAPED_NAME}: authenticated\n")
        );

        let result = serde_json::json!({
            "status": "ok", "tenant": "t\u{1b}[2J", "messages": [CRAFTED_NAME, ""]
        });
        let text = authentication_text(&result);
        assert_escaped(&text);
        assert!(text.contains(r"Tenant: t\u{1b}[2J"), "got: {text}");
    }

    #[test]
    fn free_ids_prints_object_kind_and_source_names_escaped() {
        let report = serde_json::json!({
            "mode": "field",
            "object": CRAFTED_NAME,
            "baseObject": "Base\u{1b}[2J",
            "nextFree": 50101,
            "free": [50101, 50102],
            "sources": [CRAFTED_NAME, "Other"],
            "used": [50100],
        });
        let text = free_ids_text(&report);
        assert_escaped(&text);
        assert!(
            text.contains(&format!("Next free field number for {ESCAPED_NAME}: 50101")),
            "got: {text}"
        );
        assert!(text.contains("Free: 50101, 50102\n"), "got: {text}");
        assert!(
            text.contains(r"Shares numbering with base object: Base\u{1b}[2J"),
            "got: {text}"
        );
        assert!(text.contains("Used numbers: 50100\n"), "got: {text}");

        let summary = serde_json::json!({
            "mode": "summary",
            "ranges": [],
            "kinds": [{"kind": CRAFTED_NAME, "used": 1, "free": 2, "nextFree": 3}],
        });
        assert_escaped(&free_ids_text(&summary));
    }
}
