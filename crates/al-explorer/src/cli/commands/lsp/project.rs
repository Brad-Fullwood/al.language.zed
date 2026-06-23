//! Project-level commands: rules, permissions, error codes, builtins, parse, inlay hints, fixes, authentication, debug-config init, and scaffolding.

use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_rules(json: bool) -> ExitCode {
    run_command("rules", None, json, None, |result| {
        let rules = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
        if rules.is_empty() {
            eprintln!(
                "No native lint rules are registered. Semantic AL diagnostics \
                 (CodeCop, AppSourceCop, UICop, PerTenantCop) are produced by the \
                 Microsoft analyzers via the compiler bridge, not this native registry."
            );
            return;
        }
        println!("{:<10} {:<8} {:<25} DESCRIPTION", "CODE", "SEV", "NAME");
        println!("{}", "-".repeat(80));
        for r in rules {
            let code = r.get("code").and_then(|v| v.as_str()).unwrap_or("?");
            let sev = r.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let desc = r.get("description").and_then(|v| v.as_str()).unwrap_or("?");
            println!("{:<10} {:<8} {:<25} {}", code, sev, name, desc);
        }
        eprintln!("\n{} rules", rules.len());
    })
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
    match client.request("permissions", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let count = result
                    .get("objectCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                print!("{content}");
                eprintln!("\n{count} objects included");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_error_codes(json: bool) -> ExitCode {
    run_command("errorCodes", None, json, None, |result| {
        let codes = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
        if codes.is_empty() {
            eprintln!("No error codes loaded (requires ALTool)");
        } else {
            for c in codes {
                let code = c.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("?");
                println!("{code}: {desc}");
            }
            eprintln!("\n{} error codes", codes.len());
        }
    })
}

pub fn cmd_builtins(json: bool) -> ExitCode {
    run_command("builtinTypes", None, json, None, |result| {
        let types = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
        if types.is_empty() {
            eprintln!("No builtin types loaded (requires ALTool)");
        } else {
            for t in types {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let methods = t
                    .get("methods")
                    .and_then(|v| v.as_array())
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!("{name} ({methods} methods)");
            }
            eprintln!("\n{} builtin types", types.len());
        }
    })
}

pub fn cmd_parse(file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({ "uri": uri });
    match client.request("parse", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let errors = result.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
                let nodes = result
                    .get("nodeCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let time = result
                    .get("parseTimeMs")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                println!("{file}: {nodes} nodes, {errors} errors, {time:.1}ms");
                if let Some(parse_errors) = result.get("parseErrors").and_then(|v| v.as_array()) {
                    for e in parse_errors {
                        let line = e.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                        let col = e.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                        let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                        eprintln!("  {file}:{line}:{col}: {msg}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
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
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let mut params = serde_json::json!({ "uri": uri });
    if let Some(s) = start_line {
        params["startLine"] = serde_json::json!(s.saturating_sub(1));
    }
    if let Some(e) = end_line {
        params["endLine"] = serde_json::json!(e.saturating_sub(1));
    }
    match client.request("inlayHints", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let hints = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if hints.is_empty() {
                    eprintln!("No inlay hints");
                } else {
                    for h in hints {
                        print_json(h);
                    }
                    eprintln!("\n{} hints", hints.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_fix(file: Option<&str>, dry_run: bool, rule: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "dryRun": dry_run });
    if let Some(f) = file {
        let Some(uri) = file_to_uri(f) else {
            return report_error(&format!("Cannot resolve path: {f}"), json);
        };
        params["uri"] = serde_json::json!(uri);
    }
    if let Some(r) = rule {
        params["rule"] = serde_json::json!(r);
    }
    match client.request("fix", Some(params)) {
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
        client.set_read_timeout(std::time::Duration::from_secs(120));
    }

    let mut params = serde_json::json!({ "cmd": cmd });
    if let Some(t) = tenant {
        params["tenant"] = serde_json::json!(t);
    }

    match client.request("authenticate", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                match cmd {
                    "status" => {
                        if let Some(tenants) = result.get("tenants").and_then(|v| v.as_array()) {
                            if tenants.is_empty() {
                                eprintln!("No tenants configured in this project.");
                            }
                            for t in tenants {
                                let id = t.get("tenant").and_then(|v| v.as_str()).unwrap_or("?");
                                let authed = t
                                    .get("authenticated")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let expired =
                                    t.get("expired").and_then(|v| v.as_bool()).unwrap_or(true);
                                let status = if authed && !expired {
                                    "authenticated"
                                } else if authed && expired {
                                    "expired"
                                } else {
                                    "not authenticated"
                                };
                                eprintln!("  {id}: {status}");
                            }
                        }
                    }
                    "clear" => {
                        let cleared = result.get("cleared").and_then(|v| v.as_u64()).unwrap_or(0);
                        eprintln!("Cleared {cleared} cached token(s).");
                    }
                    _ => {
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let tenant_id =
                            result.get("tenant").and_then(|v| v.as_str()).unwrap_or("?");
                        eprintln!("Status: {status}");
                        eprintln!("Tenant: {tenant_id}");
                        if let Some(msgs) = result.get("messages").and_then(|v| v.as_array()) {
                            for msg in msgs {
                                if let Some(s) = msg.as_str() {
                                    if !s.is_empty() {
                                        eprintln!("  {s}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_init_debug(project_root: &std::path::Path, json: bool) -> ExitCode {
    let zed_dir = project_root.join(".zed");
    let debug_path = zed_dir.join("debug.json");
    let debug_path_display = debug_path.display().to_string();
    if debug_path.exists() {
        if json {
            print_json(&serde_json::json!({"status": "exists", "path": debug_path_display}));
        } else {
            eprintln!("{}: already exists — not overwriting", debug_path.display());
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
            "authentication": "UserPassword",
            "startupObjectId": 22,
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "launchBrowser": true,
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "tenant": "default",
            "usePublicURLFromServer": true,
            "useMcpServerForDebugging": true,
            "build": {"command": "al-explorer", "args": ["compile"]}
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
            "numberOfSqlStatements": 10,
            "useMcpServerForDebugging": true,
            "build": {"command": "al-explorer", "args": ["compile"]}
        },
        {
            "adapter": "al",
            "label": "Attach: Your own server",
            "request": "attach",
            "environmentType": "OnPrem",
            "server": "http://bcserver",
            "serverInstance": "BC",
            "authentication": "UserPassword",
            "breakOnError": "All",
            "breakOnRecordWrite": "None",
            "enableSqlInformationDebugger": true,
            "enableLongRunningSqlStatements": true,
            "longRunningSqlStatementsThreshold": 500,
            "numberOfSqlStatements": 10,
            "breakOnNext": "WebServiceClient",
            "tenant": "default",
            "useMcpServerForDebugging": true
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
            "breakOnNext": "WebServiceClient",
            "useMcpServerForDebugging": true
        }
    ]);

    let content = match serde_json::to_string_pretty(&configs) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to serialize debug configurations: {e}");
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
                eprintln!("Created {} with 4 configurations:", debug_path.display());
                eprintln!("  - Publish: Your own server (launch)");
                eprintln!("  - Publish: Cloud Sandbox (launch)");
                eprintln!("  - Attach: Your own server (attach)");
                eprintln!("  - Attach: Cloud Sandbox (attach)");
                eprintln!(
                    "\nEdit {} to configure server URLs and authentication.",
                    debug_path.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(
            &format!("Failed to write {}: {e}", debug_path.display()),
            json,
        ),
    }
}

pub fn cmd_new(dir: &str, name: &str, publisher: &str, template: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({
        "dir": absolutize_path(dir),
        "name": name,
        "publisher": publisher,
        "template": template,
    });

    match client.request("newProject", Some(params)) {
        Ok(result) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            } else {
                let project_dir = result
                    .get("projectDir")
                    .and_then(|v| v.as_str())
                    .unwrap_or(dir);
                println!("Created AL project: {project_dir}");
                if let Some(files) = result.get("filesCreated").and_then(|v| v.as_array()) {
                    for f in files {
                        if let Some(name) = f.as_str() {
                            println!("  {name}");
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
