use std::path::PathBuf;
use std::process::ExitCode;

use super::{
    collect_al_files, connect, file_to_uri, print_json, print_lint_diag, print_symbol_entries,
    project_root, report_error, run_command,
};

pub fn cmd_version(json: bool) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    if json {
        print_json(&serde_json::json!({ "version": version }));
    } else {
        println!("al {version}");
    }
    ExitCode::SUCCESS
}

pub fn cmd_clear_cache(json: bool) -> ExitCode {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("packages"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/packages"));

    // Notify a running daemon so it can flush in-memory handles before we
    // delete the files.  Use a try-connect pattern: if no daemon is running
    // (or any error occurs) just proceed with the local removal.
    let daemon_notified = if let Ok(mut client) = connect(None) {
        client.request("al.clearSymbolCache", None).is_ok()
    } else {
        false
    };

    let existed = cache_dir.exists();
    if existed {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    if json {
        print_json(&serde_json::json!({
            "deleted": existed,
            "path": cache_dir.display().to_string(),
            "daemonNotified": daemon_notified,
        }));
    } else if existed {
        eprintln!("Cleared cache: {}", cache_dir.display());
        if daemon_notified {
            eprintln!("Running daemon notified.");
        }
    } else {
        eprintln!("Cache directory does not exist: {}", cache_dir.display());
    }
    ExitCode::SUCCESS
}

fn fetch_setup_result(json: bool) -> Result<serde_json::Value, ExitCode> {
    let mut client = connect(None).map_err(|e| report_error(&e, json))?;
    client
        .request("setup", None)
        .map_err(|e| report_error(&e, json))
}

pub fn cmd_setup(json: bool) -> ExitCode {
    let result = match fetch_setup_result(json) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if json {
        print_json(&result);
    } else {
        let altool = result
            .get("altoolInstalled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dotnet = result.get("dotnetVersion").and_then(|v| v.as_str());
        let tc = result.get("toolchain");
        if altool {
            let version = tc
                .and_then(|t| t.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let alc = tc
                .and_then(|t| t.get("alc"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("[OK] ALTool v{version}");
            println!("     alc: {alc}");
        } else {
            println!("[!!] ALTool NOT installed");
            println!(
                "     Install: dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools"
            );
        }
        if let Some(v) = dotnet {
            println!("[OK] .NET SDK {v}");
        } else {
            println!("[!!] .NET SDK not found");
        }
    }
    ExitCode::SUCCESS
}

pub fn cmd_doctor(json: bool) -> ExitCode {
    let result = match fetch_setup_result(json) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if json {
        print_json(&result);
        return ExitCode::SUCCESS;
    }
    let checks = [
        (
            "ALTool",
            result
                .get("altoolInstalled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        (".NET SDK", result.get("dotnetVersion").is_some()),
        ("Project", result.get("project").is_some()),
    ];
    let mut any_failed = false;
    for (name, ok) in &checks {
        let status = if *ok { "[OK]" } else { "[!!]" };
        println!("{status} {name}");
        if !ok {
            any_failed = true;
        }
    }
    let symbols = result
        .get("indexedSymbols")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let files = result
        .get("workspaceFiles")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if symbols > 0 && files > 0 {
        println!(
            "[OK] {} symbols indexed, {} workspace files",
            symbols, files
        );
    } else {
        println!(
            "[!!] {} symbols indexed, {} workspace files — daemon may still be loading",
            symbols, files
        );
        any_failed = true;
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_download_symbols(
    project_dir: Option<&str>,
    source: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({
        "source": source.unwrap_or("nuget"),
    });
    match client.request("downloadSymbols", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let downloaded = result
                    .get("downloaded")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let failed = result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                let source_name = result
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("nuget");
                if let Some(results) = result.get("results").and_then(|v| v.as_array()) {
                    for r in results {
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        if status == "ok" {
                            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("");
                            eprintln!("[OK] {name} -> {path}");
                        } else {
                            let err = r.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                            eprintln!("[!!] {name} — {err}");
                        }
                    }
                }
                eprintln!("\n{downloaded} downloaded, {failed} failed (source: {source_name})");
            }
            if result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_search(query: &str, limit: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "query": query, "limit": limit });
    match client.request("search", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let entries = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if entries.is_empty() {
                    eprintln!("No results for '{query}'");
                    return ExitCode::SUCCESS;
                }
                println!("{:<18} {:>6}  {:<40} PACKAGE", "KIND", "ID", "NAME");
                println!("{}", "-".repeat(80));
                for e in entries {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let pkg = e.get("package").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{:<18} {:>6}  {:<40} {}", kind, id, name, pkg);
                }
                eprintln!("\n{} results", entries.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_object(kind: &str, name: &str, json: bool) -> ExitCode {
    let params = serde_json::json!({ "kind": kind, "name": name });
    run_command("object", Some(params), json, None, |result| {
        print_symbol_entries(result);
    })
}

pub fn cmd_by_id(kind: &str, id: i32, json: bool) -> ExitCode {
    let params = serde_json::json!({ "kind": kind, "id": id });
    run_command("byId", Some(params), json, None, |result| {
        print_symbol_entries(result);
    })
}

pub fn cmd_events(name: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "name": name });
    match client.request("events", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let events = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if events.is_empty() {
                    eprintln!("No event publishers matching '{name}'");
                    return ExitCode::SUCCESS;
                }
                for e in events {
                    let obj_kind = e.get("objectKind").and_then(|v| v.as_str()).unwrap_or("?");
                    let obj_name = e.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = e.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                    let event_type = e.get("eventType").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("[{event_type}] {obj_kind} \"{obj_name}\".{method}");
                    if let Some(params) = e.get("parameters").and_then(|v| v.as_array()) {
                        for p in params {
                            let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let ptype = p.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                            let is_var = p.get("is_var").and_then(|v| v.as_bool()).unwrap_or(false);
                            let var_prefix = if is_var { "var " } else { "" };
                            println!("  {var_prefix}{pname}: {ptype}");
                        }
                    }
                }
                eprintln!("\n{} publishers", events.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_subscribers(event: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "event": event });
    match client.request("subscribers", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let subs = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if subs.is_empty() {
                    eprintln!("No subscribers for '{event}'");
                    return ExitCode::SUCCESS;
                }
                for s in subs {
                    let obj = s.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = s.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                    let target_type = s
                        .get("targetObjectType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let target_name = s
                        .get("targetObjectName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let target_event = s
                        .get("targetEventName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    println!("{obj}.{method} → {target_type}::{target_name}.{target_event}");
                }
                eprintln!("\n{} subscribers", subs.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_composed(kind: &str, name: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "kind": kind, "name": name });
    match client.request("composed", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                if let Some(base) = result.get("base") {
                    let bname = base.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let bkind = base.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("Base: {bkind} \"{bname}\"");
                }
                if let Some(exts) = result.get("extensions").and_then(|v| v.as_array()) {
                    println!("Extensions: {}", exts.len());
                    for ext in exts {
                        let ename = ext.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let epkg = ext.get("package").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("  - \"{ename}\" ({epkg})");
                    }
                }
                if let Some(fields) = result.get("all_fields").and_then(|v| v.as_array()) {
                    println!("Total fields: {}", fields.len());
                }
                if let Some(methods) = result.get("all_methods").and_then(|v| v.as_array()) {
                    println!("Total methods: {}", methods.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_packages(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("packages", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let pkgs = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if pkgs.is_empty() {
                    eprintln!("No packages loaded (is .alpackages/ empty?)");
                    return ExitCode::SUCCESS;
                }
                println!(
                    "{:<40} {:<25} {:<15} {:>8}",
                    "NAME", "PUBLISHER", "VERSION", "OBJECTS"
                );
                println!("{}", "-".repeat(90));
                let mut total_objects = 0u64;
                for p in pkgs {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let publisher = p.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                    let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let count = p.get("object_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    total_objects += count;
                    println!(
                        "{:<40} {:<25} {:<15} {:>8}",
                        name, publisher, version, count
                    );
                }
                eprintln!("\n{} packages, {} total objects", pkgs.len(), total_objects);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_deps(json: bool) -> ExitCode {
    run_command("deps", None, json, None, |result| {
        if let Some(proj) = result.get("project") {
            let name = proj.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let publisher = proj
                .get("publisher")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let version = proj.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            println!("Project: {name} by {publisher} v{version}");
        }
        if let Some(deps) = result.get("explicit").and_then(|v| v.as_array()) {
            println!("\nExplicit dependencies ({}):", deps.len());
            for d in deps {
                let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let publisher = d.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                let version = d.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  {name} by {publisher} v{version}");
            }
        }
        if let Some(all) = result.get("all").and_then(|v| v.as_array()) {
            println!("\nAll dependencies (including implicit): {}", all.len());
        }
    })
}

pub fn cmd_lint(file: Option<&str>, all: bool, analyzers: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "all": all });
    if let Some(a) = analyzers {
        let analyzer_list: Vec<&str> = a.split(',').map(|s| s.trim()).collect();
        params["analyzers"] = serde_json::json!(analyzer_list);
    }
    if let Some(f) = file {
        if let Some(uri) = file_to_uri(f) {
            params["uri"] = serde_json::json!(uri);
        } else {
            params["file"] = serde_json::json!(f);
        }
    }
    match client.request("lint", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            let found_diagnostics = if all {
                let mut total = 0usize;
                if let Some(files) = result.as_array() {
                    for file_result in files {
                        let fname = file_result
                            .get("file")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        if let Some(diags) =
                            file_result.get("diagnostics").and_then(|v| v.as_array())
                        {
                            for d in diags {
                                print_lint_diag(Some(fname), d);
                            }
                            total += diags.len();
                        }
                    }
                    eprintln!("\n{} diagnostics across {} files", total, files.len());
                }
                total > 0
            } else {
                let diagnostics = result.as_array().cloned().unwrap_or_default();
                if diagnostics.is_empty() {
                    eprintln!("No issues found");
                } else {
                    for d in &diagnostics {
                        print_lint_diag(file, d);
                    }
                    eprintln!("\n{} diagnostics", diagnostics.len());
                }
                !diagnostics.is_empty()
            };
            if found_diagnostics {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_format(file: Option<&str>, check: bool, stdin: bool, all: bool, json: bool) -> ExitCode {
    if stdin {
        return cmd_format_stdin(check, json);
    }
    if all {
        return cmd_format_all(check, json);
    }

    let Some(file) = file else {
        return report_error("No file specified. Use --stdin or --all.", json);
    };

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "check": check });
    if let Some(uri) = file_to_uri(file) {
        params["uri"] = serde_json::json!(uri);
    } else {
        params["file"] = serde_json::json!(file);
    }
    match client.request("format", Some(params)) {
        Ok(result) => {
            let changed = result
                .get("changed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if json {
                print_json(&result);
            } else if check {
                if changed {
                    eprintln!("{file}: would reformat");
                } else {
                    eprintln!("{file}: already formatted");
                }
            } else if changed {
                eprintln!("{file}: formatted");
            } else {
                eprintln!("{file}: already formatted");
            }
            // Exit 1 when --check detects changes, regardless of --json.
            if check && changed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_stdin(check: bool, json: bool) -> ExitCode {
    use std::io::Read;
    let mut content = String::new();
    if std::io::stdin().read_to_string(&mut content).is_err() {
        return report_error("Failed to read from stdin", json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "content": content, "check": check });
    match client.request("format", Some(params)) {
        Ok(result) => {
            if check {
                let changed = result
                    .get("changed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if json {
                    print_json(&serde_json::json!({ "changed": changed }));
                }
                if changed {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            } else {
                let formatted = result
                    .get("formatted")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if json {
                    print_json(&serde_json::json!({ "formatted": formatted }));
                } else {
                    print!("{formatted}");
                }
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_all(check: bool, json: bool) -> ExitCode {
    let root = project_root(None);
    let al_files = collect_al_files(&root);
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut changed_count = 0;
    let mut error_count = 0;
    let mut total = 0;
    for path in &al_files {
        total += 1;
        if let Some(uri) = url::Url::from_file_path(path).ok().map(|u| u.to_string()) {
            let params = serde_json::json!({ "uri": uri, "check": check });
            match client.request("format", Some(params)) {
                Ok(result) => {
                    let changed = result
                        .get("changed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if changed {
                        changed_count += 1;
                        if !json {
                            let display = path.strip_prefix(&root).unwrap_or(path);
                            if check {
                                eprintln!("  would reformat: {}", display.display());
                            } else {
                                eprintln!("  formatted: {}", display.display());
                            }
                        }
                    }
                }
                Err(e) => {
                    error_count += 1;
                    eprintln!("Error formatting {}: {e}", path.display());
                }
            }
        }
    }
    if json {
        print_json(&serde_json::json!({
            "total": total,
            "changed": changed_count,
            "errors": error_count,
            "check": check
        }));
    } else {
        eprintln!(
            "\n{total} files, {changed_count} {}{}",
            if check { "would change" } else { "formatted" },
            if error_count > 0 {
                format!(", {error_count} error(s)")
            } else {
                String::new()
            }
        );
    }
    if (check && changed_count > 0) || error_count > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_hover(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request("hover", Some(params)) {
        Ok(result) => {
            if result.is_null() {
                if json {
                    print_json(&serde_json::json!(null));
                } else {
                    eprintln!("No symbol at {file}:{line}:{col}");
                }
            } else if json {
                print_json(&result);
            } else {
                let contents = result
                    .get("contents")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let display = contents
                    .replace("```al\n", "")
                    .replace("```\n", "")
                    .replace("```", "")
                    .replace("*(", "(")
                    .replace(")*", ")");
                println!("{}", display.trim());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_position_query(method: &str, file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if result.is_null() {
                eprintln!("No results at {file}:{line}:{col}");
            } else {
                print_position_query_human(method, &result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// Format a position query result in a human-readable way.
fn print_position_query_human(method: &str, result: &serde_json::Value) {
    /// Extract `file:line:col` from an LSP Location object.
    fn location_str(loc: &serde_json::Value) -> Option<String> {
        let uri = loc.get("uri").and_then(|v| v.as_str())?;
        // Convert file:// URI to a plain path when possible.
        let path = url::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| uri.to_string());
        let line = loc
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|v| v.as_u64())
            .map(|l| l + 1) // convert 0-based to 1-based
            .unwrap_or(0);
        let col = loc
            .get("range")
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("character"))
            .and_then(|v| v.as_u64())
            .map(|c| c + 1)
            .unwrap_or(0);
        Some(format!("{path}:{line}:{col}"))
    }

    match method {
        "definition" | "typeDefinition" | "declaration" | "implementation" => {
            // Result is either a single Location or an array of Locations.
            if let Some(locations) = result.as_array() {
                for loc in locations {
                    if let Some(s) = location_str(loc) {
                        println!("{s}");
                    }
                }
            } else if let Some(s) = location_str(result) {
                println!("{s}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        "references" => {
            if let Some(locations) = result.as_array() {
                for loc in locations {
                    if let Some(s) = location_str(loc) {
                        println!("{s}");
                    }
                }
                eprintln!("\n{} reference(s)", locations.len());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(result).unwrap_or_default()
                );
            }
        }
        _ => {
            // Generic fallback: pretty-print with a label.
            println!("Result:");
            println!(
                "{}",
                serde_json::to_string_pretty(result).unwrap_or_default()
            );
        }
    }
}

pub fn cmd_symbols(file: &str, json: bool) -> ExitCode {
    cmd_file_query("documentSymbols", file, json)
}

pub fn cmd_folding(file: &str, json: bool) -> ExitCode {
    cmd_file_query("foldingRanges", file, json)
}

pub fn cmd_tokens(file: &str, json: bool) -> ExitCode {
    cmd_file_query("semanticTokens", file, json)
}

fn cmd_file_query(method: &str, file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({ "uri": uri });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json || !result.is_null() {
                print_json(&result);
            } else {
                eprintln!("No results for {file}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_rename(
    file: &str,
    line: u32,
    col: u32,
    new_name: &str,
    dry_run: bool,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
        "newName": new_name,
    });
    match client.request("rename", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            if result.is_null() {
                eprintln!("Cannot rename symbol at {file}:{line}:{col}");
                return ExitCode::FAILURE;
            }
            if let Some(changes) = result.get("changes").and_then(|v| v.as_object()) {
                let mut total_edits = 0;
                for edits in changes.values() {
                    if let Some(arr) = edits.as_array() {
                        total_edits += arr.len();
                    }
                }
                if dry_run {
                    for (uri, edits) in changes {
                        if let Some(arr) = edits.as_array() {
                            println!("{uri}: {} edit(s)", arr.len());
                        }
                    }
                    eprintln!("\n{total_edits} total edits (dry run, not applied)");
                } else {
                    match apply_workspace_edit(changes) {
                        Ok(files_changed) => {
                            eprintln!(
                                "{total_edits} edit(s) applied across {} file(s)",
                                files_changed
                            );
                        }
                        Err(e) => {
                            eprintln!("Error applying edits: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// Apply a WorkspaceEdit `changes` map to disk.
///
/// Each key is a `file://` URI and each value is an array of LSP TextEdit
/// objects (`{ range: { start, end }, newText }`).  Edits are applied in
/// reverse position order so that later offsets are not invalidated by earlier
/// mutations.  Returns the number of files that were written.
fn apply_workspace_edit(
    changes: &serde_json::Map<String, serde_json::Value>,
) -> Result<usize, String> {
    let mut files_changed = 0;

    for (uri, edits_val) in changes {
        let edits = match edits_val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        // Resolve the URI to a filesystem path.
        let path = url::Url::parse(uri)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| format!("Cannot resolve URI to file path: {uri}"))?;

        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;

        let lines: Vec<&str> = content.split('\n').collect();

        /// Convert a 0-based LSP (line, character) — where character is a
        /// UTF-16 code unit index — to a byte offset in `content`.
        fn lsp_pos_to_byte_offset(
            lines: &[&str],
            line: u64,
            character: u64,
        ) -> Result<usize, String> {
            if line as usize >= lines.len() {
                // Position is past EOF — clamp to end of content.
                return Ok(lines
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1));
            }
            // Byte offset of the start of the target line.
            let line_start: usize = lines[..line as usize]
                .iter()
                .map(|l| l.len() + 1) // +1 for the '\n' we split on
                .sum();
            // Walk UTF-16 code units along the line to find the byte offset.
            let line_str = lines[line as usize];
            let mut utf16_count = 0u64;
            for (byte_pos, ch) in line_str.char_indices() {
                if utf16_count >= character {
                    return Ok(line_start + byte_pos);
                }
                utf16_count += ch.len_utf16() as u64;
            }
            // character is at or beyond the end of the line.
            Ok(line_start + line_str.len())
        }

        // Parse and sort edits in reverse start-position order so that
        // applying them from the end does not shift the offsets of earlier edits.
        let mut parsed_edits: Vec<(u64, u64, u64, u64, &str)> = edits
            .iter()
            .map(|e| {
                let range = e.get("range").ok_or("missing range")?;
                let start = range.get("start").ok_or("missing start")?;
                let end = range.get("end").ok_or("missing end")?;
                let sl = start
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing start.line")?;
                let sc = start
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing start.character")?;
                let el = end
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing end.line")?;
                let ec = end
                    .get("character")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing end.character")?;
                let new_text = e
                    .get("newText")
                    .and_then(|v| v.as_str())
                    .ok_or("missing newText")?;
                Ok((sl, sc, el, ec, new_text))
            })
            .collect::<Result<_, &str>>()
            .map_err(|e| format!("Invalid edit in {uri}: {e}"))?;

        // Sort descending by start position (line then character).
        parsed_edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

        // Pre-compute all byte ranges from the *original* lines before any
        // mutation so that later edits don't shift the offsets used by earlier
        // ones.  The edits are already sorted descending, so applying them in
        // order produces correct results.
        let mut byte_ranges: Vec<(usize, usize, &str)> = Vec::with_capacity(parsed_edits.len());
        for (sl, sc, el, ec, new_text) in &parsed_edits {
            let start_byte = lsp_pos_to_byte_offset(&lines, *sl, *sc)?;
            let end_byte = lsp_pos_to_byte_offset(&lines, *el, *ec)?;
            if start_byte > content.len() || end_byte > content.len() || start_byte > end_byte {
                return Err(format!(
                    "Edit range out of bounds in {uri}: {sl}:{sc}-{el}:{ec}"
                ));
            }
            byte_ranges.push((start_byte, end_byte, new_text));
        }

        let mut new_content = content.clone();
        for (start_byte, end_byte, new_text) in byte_ranges {
            new_content.replace_range(start_byte..end_byte, new_text);
        }

        if new_content != content {
            std::fs::write(&path, &new_content)
                .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
            files_changed += 1;
        }
    }

    Ok(files_changed)
}

pub fn cmd_rules(json: bool) -> ExitCode {
    run_command("rules", None, json, None, |result| {
        let rules = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
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
    let mut params = serde_json::json!({});
    if let Some(uri) = file_to_uri(file) {
        params["uri"] = serde_json::json!(uri);
    } else {
        params["file"] = serde_json::json!(file);
    }
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
        if let Some(uri) = file_to_uri(f) {
            params["uri"] = serde_json::json!(uri);
        } else {
            params["file"] = serde_json::json!(f);
        }
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
            "build": {"command": "al", "args": ["compile"]}
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
            "build": {"command": "al", "args": ["compile"]}
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
        "dir": dir,
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
        if let Some(uri) = file_to_uri(f) {
            params["uri"] = serde_json::json!(uri);
        } else {
            params["file"] = serde_json::json!(f);
        }
    }

    match client.request("metrics", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if all {
                // Print per-file hotspot summary
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
                // Single file: print all procedures with hotspots flagged
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
            ExitCode::SUCCESS
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
    run_command(
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
    )
}

// ---------------------------------------------------------------------------
// WP16: Bulk fix commands (T1603-T1605)
// ---------------------------------------------------------------------------

pub fn cmd_add_application_area(value: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request(
        "fix.applicationArea",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
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

pub fn cmd_add_tooltips(from_table: Option<&str>, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "dryRun": dry_run });
    if let Some(t) = from_table {
        params["fromTable"] = serde_json::Value::String(t.to_string());
    }
    match client.request("fix.tooltips", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
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
    match client.request(
        "fix.dataClassification",
        Some(serde_json::json!({ "value": value, "dryRun": dry_run })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let files = result
                    .get("filesModified")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let changes = result
                    .get("totalChanges")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
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

// ---------------------------------------------------------------------------
// WP15: Test runner commands
// ---------------------------------------------------------------------------

pub fn cmd_tests_discover(json: bool) -> ExitCode {
    run_command(
        "tests.discover",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let tests = result.as_array().cloned().unwrap_or_default();
            if tests.is_empty() {
                eprintln!("No test codeunits found");
            } else {
                for t in &tests {
                    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = t.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let count = t
                        .get("tests")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    println!("  Codeunit {id} \"{name}\" -- {count} test(s)");
                }
                eprintln!("\n{} test codeunit(s) found", tests.len());
            }
        },
    )
}

pub fn cmd_tests_coverage(json: bool) -> ExitCode {
    run_command(
        "tests.coverage",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let covered = result
                .get("coveredProcedures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = result
                .get("totalProcedures")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let pct = (covered * 100).checked_div(total).unwrap_or(0);
            println!("Test coverage: {covered}/{total} procedures ({pct}%)");
        },
    )
}

/// `al test-run <codeunit> [--name <name>] [--method <method>] [--config <config>]`
///
/// Sends a `tests.run` JSON-RPC request to the al-lsp daemon. The daemon
/// calls the BC REST dev API and returns the test results plus diagnostics.
pub fn cmd_test_run(
    codeunit: i64,
    name: Option<&str>,
    method: Option<&str>,
    config: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let mut params = serde_json::json!({ "codeunit": codeunit });
    if let Some(n) = name {
        params["codeunitName"] = serde_json::Value::String(n.to_string());
    }
    if let Some(m) = method {
        params["method"] = serde_json::Value::String(m.to_string());
    }
    if let Some(c) = config {
        params["config"] = serde_json::Value::String(c.to_string());
    }

    match client.request("tests.run", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                // Human-readable summary
                if let Some(run) = result.get("result") {
                    let cu_name = run.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let total = run.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                    let passed = run.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
                    let failed = run.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                    let skipped = run.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!(
                        "Codeunit \"{cu_name}\": {passed}/{total} passed, {failed} failed, {skipped} skipped"
                    );

                    if let Some(methods) = run.get("methods").and_then(|v| v.as_array()) {
                        for m in methods {
                            let mname = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let status = m.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                            let icon = match status {
                                "pass" => "✓",
                                "fail" => "✗",
                                _ => "~",
                            };
                            print!("  {icon} {mname}");
                            if let Some(err) = m.get("error").and_then(|v| v.as_str()) {
                                print!(" -- {err}");
                            }
                            println!();
                        }
                    }
                } else {
                    println!("No test results returned");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// `al-explorer test-affected <files...>` — show which tests touch any of the given files.
pub fn cmd_test_affected(files: &[String], json: bool) -> ExitCode {
    let params = serde_json::json!({ "changedFiles": files });
    run_command("tests.affected", Some(params), json, None, |result| {
        let affected = result
            .get("affected")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if affected.is_empty() {
            eprintln!("No tests touch the given files");
            return;
        }
        for t in &affected {
            let cu_name = t
                .get("codeunitName")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let cu_id = t.get("codeunitId").and_then(|v| v.as_i64()).unwrap_or(0);
            let method = t.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
            let line = t.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("  Codeunit {cu_id} \"{cu_name}\" :: {method} (line {line})");
        }
        eprintln!("\n{} affected test(s)", affected.len());
    })
}

/// `al-explorer test-results [--codeunit ID] [--method NAME]` — show persisted test history.
pub fn cmd_test_results(codeunit: Option<i64>, method: Option<&str>, json: bool) -> ExitCode {
    let mut params = serde_json::json!({});
    if let Some(id) = codeunit {
        params["codeunitId"] = serde_json::Value::from(id);
    }
    if let Some(m) = method {
        params["methodName"] = serde_json::Value::String(m.to_string());
    }
    run_command("tests.last_results", Some(params), json, None, |result| {
        // Single (codeunit, method) lookup short-circuits to lastResult.
        if let Some(last) = result.get("lastResult") {
            if last.is_null() {
                eprintln!("No prior runs recorded for that (codeunit, method) pair.");
                return;
            }
            print_history_row(last);
            return;
        }
        let results = result
            .get("results")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if results.is_empty() {
            eprintln!("No test history recorded yet — run tests first.");
            return;
        }
        for r in &results {
            print_history_row(r);
        }
        eprintln!("\n{} record(s)", results.len());
    })
}

fn print_history_row(r: &serde_json::Value) {
    let cu = r
        .get("codeunitName")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let cu_id = r.get("codeunitId").and_then(|v| v.as_i64()).unwrap_or(0);
    let method = r.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
    let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let icon = match status {
        "pass" => "✓",
        "fail" => "✗",
        "skip" => "⊘",
        _ => "?",
    };
    let dur = r
        .get("durationMs")
        .and_then(|v| v.as_u64())
        .map(|n| format!("{n}ms"))
        .unwrap_or_else(|| "—".to_string());
    let ts = r.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("  {icon} [{cu_id} {cu}::{method}] {status} {dur}  (ts={ts})");
    if let Some(err) = r.get("error").and_then(|v| v.as_str()) {
        if !err.is_empty() {
            println!("      {err}");
        }
    }
}

/// `al-explorer test-classify` — show the routing decision for every test.
pub fn cmd_test_classify(json: bool) -> ExitCode {
    run_command(
        "tests.classify",
        Some(serde_json::json!({})),
        json,
        None,
        |result| {
            let classifications = result
                .get("classifications")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if classifications.is_empty() {
                eprintln!("No test codeunits discovered");
                return;
            }
            for c in &classifications {
                let cu = c
                    .get("codeunitName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let method = c.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                let decision = c.get("decision").and_then(|v| v.as_str()).unwrap_or("?");
                let reasons = c
                    .get("reasons")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|r| r.get("message").and_then(|m| m.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                if reasons.is_empty() {
                    println!("  [{decision:>12}] {cu} :: {method}");
                } else {
                    println!("  [{decision:>12}] {cu} :: {method} -- {reasons}");
                }
            }
            eprintln!("\n{} test(s) classified", classifications.len());
        },
    )
}

/// `al test-run-all [--parallel] [--junit-out X] [--cobertura-out Y] [--filter PATTERN] [--timeout-ms N]`
///
/// Runs every discovered test codeunit through the daemon's `tests.run_auto`
/// endpoint. Streams a per-codeunit summary then a final totals line; exits
/// non-zero if any test failed.
pub fn cmd_test_run_all(
    parallel: bool,
    timeout_ms: Option<u64>,
    junit_out: Option<&str>,
    cobertura_out: Option<&str>,
    filter: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut params = serde_json::json!({ "parallel": parallel });
    if let Some(ms) = timeout_ms {
        params["timeoutMs"] = serde_json::Value::from(ms);
    }
    if let Some(p) = junit_out {
        params["junitOut"] = serde_json::Value::String(p.to_string());
    }
    if let Some(p) = cobertura_out {
        params["coberturaOut"] = serde_json::Value::String(p.to_string());
    }
    if let Some(p) = filter {
        params["filter"] = serde_json::Value::String(p.to_string());
    }

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("tests.run_auto", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                if let Some(failed) = result
                    .get("totals")
                    .and_then(|t| t.get("failed"))
                    .and_then(|v| v.as_u64())
                    && failed > 0
                {
                    return ExitCode::FAILURE;
                }
                return ExitCode::SUCCESS;
            }

            // Pretty per-codeunit summary
            let summaries = result
                .get("summaries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for cu in &summaries {
                let cu_name = cu.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let total = cu.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let passed = cu.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
                let failed = cu.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                let skipped = cu.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
                let icon = if failed > 0 { "✗" } else { "✓" };
                println!(
                    "{icon} {cu_name}: {passed}/{total} passed, {failed} failed, {skipped} skipped"
                );
            }

            let totals = result.get("totals");
            let total = totals
                .and_then(|t| t.get("total"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let passed = totals
                .and_then(|t| t.get("passed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let failed = totals
                .and_then(|t| t.get("failed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let skipped = totals
                .and_then(|t| t.get("skipped"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            eprintln!("\nTotal: {passed}/{total} passed, {failed} failed, {skipped} skipped");
            if failed > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

// ---------------------------------------------------------------------------
// WP16: Code generation command
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// WP17: Analysis differentiator commands
// ---------------------------------------------------------------------------

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

pub fn cmd_profiler_hints(hotspots: &[String], json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let hotspot_values: Vec<serde_json::Value> = hotspots
        .iter()
        .map(|h| serde_json::json!({ "name": h }))
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
pub fn cmd_test_snapshot(subcmd: &super::super::TestSnapshotCommands, json: bool) -> ExitCode {
    use super::super::TestSnapshotCommands;

    match subcmd {
        TestSnapshotCommands::Record {
            codeunit,
            method,
            breakpoints,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };

            // Parse "file:line" breakpoint specs
            let mut bp_array = Vec::new();
            for spec in breakpoints {
                let Some((file, line_str)) = spec.rsplit_once(':') else {
                    return report_error(
                        &format!("Invalid breakpoint spec '{spec}': expected <file>:<line>"),
                        json,
                    );
                };
                let line: u32 = match line_str.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        return report_error(
                            &format!("Invalid line number in breakpoint '{spec}'"),
                            json,
                        );
                    }
                };
                bp_array.push(serde_json::json!({"file": file, "line": line}));
            }

            let params = serde_json::json!({
                "codeunitId": codeunit,
                "methodName": method,
                "breakpoints": bp_array,
            });
            client.set_read_timeout(std::time::Duration::from_secs(120));
            match client.request("tests.snapshot_record", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path = result.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let count = result
                            .get("sampleCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!("Snapshot recorded: {path} ({count} samples)");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        TestSnapshotCommands::Replay { path } => {
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
            client.set_read_timeout(std::time::Duration::from_secs(120));
            let params = serde_json::json!({
                "snapshotPath": abs_path.display().to_string(),
            });
            match client.request("tests.snapshot_replay", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let verdict = result
                            .get("verdict")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        if verdict == "Match" {
                            println!("[PASS] Snapshot matches baseline.");
                        } else {
                            println!("[FAIL] Snapshot diverged from baseline:");
                            if let Some(divs) = result.get("divergences").and_then(|v| v.as_array())
                            {
                                for d in divs {
                                    let field =
                                        d.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                                    let expected = d.get("expected").cloned().unwrap_or_default();
                                    let actual = d.get("actual").cloned().unwrap_or_default();
                                    println!("  {field}: expected={expected}, actual={actual}");
                                }
                            }
                            return ExitCode::FAILURE;
                        }
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
                        let divs = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        if divs.is_empty() {
                            println!("Snapshots are identical.");
                        } else {
                            println!("{} divergence(s):", divs.len());
                            for d in divs {
                                let idx =
                                    d.get("sampleIndex").and_then(|v| v.as_u64()).unwrap_or(0);
                                let field = d.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                                let expected = d.get("expected").cloned().unwrap_or_default();
                                let actual = d.get("actual").cloned().unwrap_or_default();
                                println!("  [sample {idx}] {field}: {expected} -> {actual}");
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

            // Print table of survived variants
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
                        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?");
                        let file = v.get("file").and_then(|x| x.as_str()).unwrap_or("?");
                        let file_short = std::path::Path::new(file)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(file);
                        let line = v.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
                        let desc = v.get("description").and_then(|x| x.as_str()).unwrap_or("?");
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
