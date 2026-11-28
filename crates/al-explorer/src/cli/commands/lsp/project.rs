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
        eprintln!("\n{}", count_text(rules.len(), "rule"));
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

/// Why an empty `errorCodes` or `builtinTypes` catalog is an error. The daemon
/// fills both from the semantic bridge, which loads the Microsoft AL compiler
/// that ALTool installs. An empty list means one of the two is missing, and the
/// daemon does not say which, so the message names both.
fn catalog_missing_text(what: &str) -> String {
    format!(
        "No {} loaded: the semantic bridge (AlBridge.dll) was not found next to al-lsp \
         or in AL_BRIDGE_DIR, or ALTool (the AL compiler) was not found. \
         Run `al-explorer setup` to check ALTool.",
        terminal_text(what)
    )
}

/// Exit code for the catalog commands: a missing catalog is a failure.
fn catalog_exit(result: &serde_json::Value) -> ExitCode {
    if list_rows(result)
        .as_array()
        .is_none_or(|rows| rows.is_empty())
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_error_codes(json: bool) -> ExitCode {
    run_command_with_exit(
        "errorCodes",
        None,
        json,
        None,
        |result| {
            let codes = list_rows(result).as_array().map(|v| &v[..]).unwrap_or(&[]);
            if codes.is_empty() {
                eprintln!("{}", catalog_missing_text("error codes"));
            } else {
                print!("{}", error_codes_text(codes));
                eprintln!("\n{} error codes", codes.len());
            }
        },
        catalog_exit,
    )
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
    run_command_with_exit(
        "builtinTypes",
        None,
        json,
        None,
        |result| {
            let types = list_rows(result).as_array().map(|v| &v[..]).unwrap_or(&[]);
            if types.is_empty() {
                eprintln!("{}", catalog_missing_text("builtin types"));
            } else {
                print!("{}", builtins_text(types));
                eprintln!("\n{} builtin types", types.len());
            }
        },
        catalog_exit,
    )
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
                    eprintln!("\n{}", count_text(hints.len(), "hint"));
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

pub fn cmd_authenticate(
    cmd: &str,
    tenant: Option<&str>,
    json: bool,
    device_code: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    if cmd == "login" {
        return login_in_process(tenant, json, device_code);
    }

    let mut params = serde_json::json!({ "cmd": cmd });
    if let Some(t) = tenant {
        params["tenant"] = serde_json::json!(t);
    }

    match request_checked(&mut client, "authenticate", Some(params)) {
        Ok(result) => {
            let usable = cmd != "status" || tenant_authenticated(&result, tenant);
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

/// `authenticate login` runs the sign-in in this process. The daemon answers
/// one request with one response, so a device code it printed would reach the
/// user only after the flow ended. Here the prompt goes to stderr as it arrives.
/// The token cache is shared on disk (or the keyring), so the daemon finds the
/// token afterwards. The tenant is the typed `--tenant` or the global one, and
/// the tenant guard checks it against the launch configuration first.
fn login_in_process(tenant: Option<&str>, json: bool, device_code: bool) -> ExitCode {
    let declared = caller_declaration();
    let declared_tenant = declared.and_then(|d| d.tenant.as_deref());
    let purpose = declared.and_then(|d| d.purpose.as_deref());
    let typed = match (tenant, declared_tenant) {
        (Some(arg), Some(global)) if !arg.trim().eq_ignore_ascii_case(global.trim()) => {
            let msg = format!(
                "{} --tenant {arg} differs from the global --tenant {global}. Pass one tenant.",
                al_bc::tenant_guard::REFUSAL_PREFIX
            );
            return report_error(&msg, json);
        }
        (arg, global) => arg.or(global),
    };
    let root = match project_root(None) {
        Ok(root) => root,
        Err(e) => return report_error(&e, json),
    };
    let configs = match al_bc::launch::find_launch_config(&root) {
        Ok(Some(file)) => file.configs,
        Ok(None) => Vec::new(),
        Err(e) => return report_error(&e.to_string(), json),
    };
    let registry = match al_bc::tenant_guard::load_registry(&al_bc::tenant_guard::registry_path()) {
        Ok(registry) => registry,
        Err(refusal) => return report_error(&refusal.0, json),
    };
    let tenant = match login_tenant(typed, purpose, &configs, &registry) {
        Ok(tenant) => tenant,
        Err(refusal) => return report_error(&refusal.0, json),
    };
    if let Some(line) = al_bc::tenant_guard::banner_for(&tenant, purpose) {
        eprintln!("{}", terminal_text(&line));
    }
    let mode = if device_code {
        al_symbols::oauth::SignInMode::DeviceCode
    } else {
        al_symbols::oauth::SignInMode::Auto
    };
    let messages = std::sync::Mutex::new(Vec::<String>::new());
    let settings = match project_root(None).and_then(|root| {
        al_symbols::oauth::AuthSettings::for_project_root(&root, &tenant)
            .map_err(|e| format!("Invalid entraIdAuthentication: {e}"))
    }) {
        Ok(s) => s,
        Err(e) => return report_error(&e, json),
    };
    let result = al_symbols::oauth::acquire_token_blocking(&tenant, &settings, mode, |msg| {
        al_symbols::oauth::announce(&terminal_text(msg));
        if let Ok(mut guard) = messages.lock() {
            guard.push(msg.to_string());
        }
    });
    match result {
        Ok(_token) => {
            let result = serde_json::json!({
                "status": "authenticated",
                "tenant": tenant,
                "messages": messages.into_inner().unwrap_or_default(),
            });
            if json {
                print_json(&result);
            } else {
                eprintln!("Authenticated to tenant {}.", terminal_text(&tenant));
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&format!("Authentication failed: {e}"), json),
    }
}

/// The tenant `authenticate login` signs in to, checked by the tenant guard
/// against the launch configuration. Refuses a missing tenant too.
fn login_tenant(
    tenant: Option<&str>,
    purpose: Option<&str>,
    configs: &[al_bc::launch::BcServerConfig],
    registry: &al_bc::tenant_guard::Registry,
) -> Result<String, al_bc::tenant_guard::TenantRefused> {
    let Some(typed) = tenant.map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(al_bc::tenant_guard::TenantRefused(format!(
            "{} {}",
            al_bc::tenant_guard::REFUSAL_PREFIX,
            al_bc::tenant_guard::missing_tenant_message("authenticate login")
        )));
    };
    // With no matching configuration, the first one's tenant is passed on so
    // the guard reports the mismatch.
    let config_tenant = configs
        .iter()
        .find(|config| {
            config
                .tenant
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(typed))
        })
        .or_else(|| configs.first())
        .and_then(|config| config.tenant.as_deref());
    al_bc::tenant_guard::require(Some(typed), purpose, config_tenant, registry)
        .map(|info| info.guid)
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

/// A status entry counts as signed in when its token is present and unexpired.
fn tenant_entry_authenticated(tenant: &serde_json::Value) -> bool {
    tenant
        .get("authenticated")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        && !tenant
            .get("expired")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
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
        .is_some_and(|tenants| tenants.iter().any(tenant_entry_authenticated))
}

/// `authenticate status --tenant X` is usable only when that tenant is signed
/// in. Without `--tenant` any tenant is enough.
pub(crate) fn tenant_authenticated(result: &serde_json::Value, tenant: Option<&str>) -> bool {
    let Some(typed) = tenant else {
        return any_tenant_authenticated(result);
    };
    result
        .get("tenants")
        .and_then(|value| value.as_array())
        .is_some_and(|tenants| {
            tenants.iter().any(|entry| {
                entry
                    .get("tenant")
                    .and_then(|value| value.as_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case(typed))
                    && tenant_entry_authenticated(entry)
            })
        })
}

pub fn cmd_init_debug(project_root: &std::path::Path, json: bool) -> ExitCode {
    let zed_dir = project_root.join(".zed");
    let debug_path = zed_dir.join("debug.json");
    let debug_path_display = debug_path.display().to_string();
    // `exists` and `write` follow links, so a link is refused before either runs.
    for link in [&zed_dir, &debug_path] {
        if std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
            let msg = format!(
                "Refusing to write {}: it is a symbolic link",
                link.display()
            );
            return report_error(&msg, json);
        }
    }
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
    // `create_new` fails on any existing entry, a link included, so nothing
    // outside the project is written.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&debug_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, content.as_bytes()));
    match written {
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
/// What `al-explorer new` was asked for.
pub struct NewOptions<'a> {
    pub dir: &'a str,
    pub name: &'a str,
    pub publisher: &'a str,
    pub template: &'a str,
    pub runtime: &'a str,
    pub app_id: Option<&'a str>,
    pub tenant: Option<&'a str>,
    pub environment: Option<&'a str>,
}

/// Ask the tenant for its BaseApp version and write app.json to its release.
/// Standard route checked: the daemon's `environmentVersion` request reuses the
/// call download-symbols makes. A failed lookup keeps the runtime table values.
fn retarget_to_tenant(project_dir: &str) -> String {
    let kept = |why: &str| {
        format!("app.json keeps the application and platform versions of the AL runtime: {why}")
    };
    let mut client = match connect(Some(project_dir)) {
        Ok(client) => client,
        Err(error) => return kept(&error),
    };
    let answer = match request_checked(&mut client, "environmentVersion", None) {
        Ok(answer) => answer,
        Err(error) => return kept(&error),
    };
    let Some(version) = answer["version"].as_str() else {
        return kept(
            answer["reason"]
                .as_str()
                .unwrap_or("the tenant gave no version"),
        );
    };
    match al_project::scaffold::retarget_app_json_to_environment(
        std::path::Path::new(project_dir),
        version,
    ) {
        Ok(target) => format!(
            "The environment runs BaseApp {version}, so app.json application and platform are {target}."
        ),
        Err(error) => kept(&error),
    }
}

pub fn cmd_new(options: NewOptions<'_>, json: bool) -> ExitCode {
    use al_project::scaffold;

    let NewOptions {
        dir,
        name,
        publisher,
        template,
        runtime,
        app_id,
        tenant,
        environment,
    } = options;
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
    let mut config = scaffold::ScaffoldConfig {
        name: name.to_string(),
        publisher: publisher.to_string(),
        runtime: runtime.to_string(),
        template,
        environment: environment.map(str::to_string),
        ..scaffold::ScaffoldConfig::default()
    };
    if let Some(app_id) = app_id {
        match scaffold::normalize_guid(app_id, "--app-id") {
            Ok(id) => config.id = id,
            Err(error) => return report_error(&error, json),
        }
    }
    if let Some(tenant) = tenant {
        match scaffold::normalize_guid(tenant, "--tenant") {
            Ok(tenant) => config.tenant = Some(tenant),
            Err(error) => return report_error(&error, json),
        }
    }

    match scaffold::create_project(std::path::Path::new(&directory), &config) {
        Ok(result) => {
            let environment_note = config
                .tenant
                .as_deref()
                .map(|_| retarget_to_tenant(&result.project_dir));
            if json {
                let mut value = match serde_json::to_value(&result) {
                    Ok(value) => value,
                    Err(error) => return report_error(&error.to_string(), json),
                };
                if let (Some(note), Some(object)) = (&environment_note, value.as_object_mut()) {
                    object.insert("environmentNote".to_string(), serde_json::json!(note));
                }
                print_json(&value);
            } else {
                print!(
                    "{}",
                    new_project_text(&result.project_dir, &result.files_created)
                );
                if !result.fields_to_fill.is_empty() {
                    println!("Before AppSourceCop passes, fill:");
                    for line in &result.fields_to_fill {
                        println!("  {}", terminal_text(line));
                    }
                }
                if let Some(note) = environment_note {
                    println!("{}", terminal_text(&note));
                }
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

    /// An empty catalog (no bridge or no ALTool) is a failed command.
    #[test]
    fn empty_catalog_exits_non_zero_and_names_the_bridge() {
        assert_eq!(catalog_exit(&serde_json::json!([])), ExitCode::FAILURE);
        assert_eq!(
            catalog_exit(&serde_json::json!([{"code": "AL0001"}])),
            ExitCode::SUCCESS
        );
        let message = catalog_missing_text("error codes");
        assert!(message.contains("AlBridge.dll") && message.contains("ALTool"));
    }

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

    #[test]
    fn tenant_authenticated_needs_the_typed_tenant() {
        let status = serde_json::json!({
            "tenants": [
                {"tenant": "A", "authenticated": true, "expired": false},
                {"tenant": "B", "authenticated": false, "expired": false},
            ]
        });
        assert!(!tenant_authenticated(&status, Some("B")));
        assert!(tenant_authenticated(&status, Some("a")));
        assert!(!tenant_authenticated(&status, Some("C")));
        assert!(tenant_authenticated(&status, None));
    }

    #[test]
    fn login_tenant_needs_the_configured_tenant() {
        const CDX: &str = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d";
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("tenants.json");
        std::fs::write(
            &path,
            r#"{"tenants":{"1b7a9471-d313-4aa4-99e8-7a6c762c8e7d":{"name":"CDX","kind":"demo"}}}"#,
        )
        .expect("registry file");
        let registry = al_bc::tenant_guard::load_registry(&path).expect("registry");
        let config = al_bc::launch::BcServerConfig {
            name: "CDX".to_string(),
            environment_type: al_bc::launch::EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("CDX".to_string()),
            tenant: Some(CDX.to_string()),
            authentication: al_bc::launch::AuthMethod::AAD,
            accept_invalid_certs: false,
            debug_args: serde_json::Value::Null,
        };
        let configs = [config];
        let missing = login_tenant(None, None, &configs, &registry).unwrap_err();
        // The prefix makes report_error exit 3.
        assert!(
            missing.0.starts_with(al_bc::tenant_guard::REFUSAL_PREFIX),
            "{missing}"
        );
        assert!(
            login_tenant(
                Some("00000000-0000-0000-0000-000000000001"),
                None,
                &configs,
                &registry
            )
            .is_err()
        );
        let guid =
            login_tenant(Some(CDX), None, &configs, &registry).expect("the configured tenant");
        assert_eq!(guid, CDX);
    }

    #[cfg(unix)]
    #[test]
    fn init_debug_refuses_symlinked_zed_dir() {
        let root = tempfile::tempdir().expect("temporary directory");
        let outside = root.path().join("outside");
        let project = root.path().join("project");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::fs::create_dir_all(&project).expect("project directory");
        std::os::unix::fs::symlink(&outside, project.join(".zed")).expect("symlink");
        assert_eq!(cmd_init_debug(&project, false), ExitCode::FAILURE);
        assert_eq!(
            std::fs::read_dir(&outside)
                .expect("outside directory")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn init_debug_refuses_symlinked_debug_json() {
        let root = tempfile::tempdir().expect("temporary directory");
        let project = root.path().join("project");
        let outside = root.path().join("outside");
        let target = outside.join("x.json");
        std::fs::create_dir_all(project.join(".zed")).expect(".zed directory");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::os::unix::fs::symlink(&target, project.join(".zed").join("debug.json"))
            .expect("symlink");
        assert_eq!(cmd_init_debug(&project, false), ExitCode::FAILURE);
        assert!(!target.exists());
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
