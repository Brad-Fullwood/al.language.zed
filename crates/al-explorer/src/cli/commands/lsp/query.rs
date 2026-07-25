//! Symbol-lookup and dependency queries: search, object/by-id lookup, events/subscribers, composed objects, packages, and deps.

use std::process::ExitCode;

use crate::cli::commands::*;

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
                println!(
                    "{:<18} {:>6}  {:<34} {:<24} SOURCE",
                    "KIND", "ID", "NAME", "PACKAGE"
                );
                println!("{}", "-".repeat(105));
                for e in entries {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let pkg = e.get("package").and_then(|v| v.as_str()).unwrap_or("?");
                    let source = e
                        .get("source_availability")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .replace('_', " ");
                    // Interfaces & co. have no developer-visible object ID —
                    // symbol packages put an internal compiler hash in the
                    // Id slot. Render blank instead of the hash or -1.
                    let id_text = if id > 0 && kind_has_numeric_id(kind) {
                        id.to_string()
                    } else {
                        String::new()
                    };
                    println!(
                        "{:<18} {:>6}  {:<34} {:<24} {}",
                        kind, id_text, name, pkg, source
                    );
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

pub fn cmd_source(
    name: &str,
    kind: Option<&str>,
    package: Option<&str>,
    procedure: Option<&str>,
    trigger: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut client = match connect(None) {
        Ok(client) => client,
        Err(error) => return report_error(&error, json),
    };
    let mut params = serde_json::Map::new();
    params.insert("name".into(), name.into());
    if let Some(kind) = kind {
        params.insert("kind".into(), kind.into());
    }
    if let Some(package) = package {
        params.insert("package".into(), package.into());
    }
    if let Some(procedure) = procedure {
        params.insert("proc".into(), procedure.into());
    }
    if let Some(trigger) = trigger {
        params.insert("trigger".into(), trigger.into());
    }

    match client.request("source", Some(serde_json::Value::Object(params))) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let availability = result
                    .get("source_availability")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .replace('_', " ");
                let package = result.get("pkg").and_then(|value| value.as_str());
                match package {
                    Some(package) => println!("Source: {availability} ({package})"),
                    None => println!("Source: {availability}"),
                }
                if let Some(note) = result.get("note").and_then(|value| value.as_str()) {
                    eprintln!("Note: {note}");
                }
                if let Some(code) = result.get("code").and_then(|value| value.as_str()) {
                    println!("\n{code}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error, json),
    }
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
                // enrich each publisher with its workspace subscribers
                // so the result is a chain view, not a flat name list. One
                // extra daemon round-trip per distinct event name, capped.
                const SUBSCRIBER_LOOKUP_CAP: usize = 25;
                for (i, e) in events.iter().enumerate() {
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
                    if i < SUBSCRIBER_LOOKUP_CAP {
                        if let Ok(subs) = client
                            .request("subscribers", Some(serde_json::json!({ "event": method })))
                        {
                            let subs = subs.as_array().map(|v| &v[..]).unwrap_or(&[]);
                            let mine: Vec<_> = subs
                                .iter()
                                .filter(|s| {
                                    s.get("targetObjectName")
                                        .and_then(|v| v.as_str())
                                        .map(|t| t.eq_ignore_ascii_case(obj_name))
                                        .unwrap_or(false)
                                })
                                .collect();
                            if mine.is_empty() {
                                println!("  ← no workspace subscribers");
                            }
                            for s in mine {
                                let sobj =
                                    s.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                                let smethod =
                                    s.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                                println!("  ← subscribed by {sobj}.{smethod}");
                            }
                        }
                    }
                }
                if events.len() > SUBSCRIBER_LOOKUP_CAP {
                    eprintln!(
                        "(subscriber lookup shown for the first {SUBSCRIBER_LOOKUP_CAP} \
                         publishers — narrow the search for full chains)"
                    );
                }
                eprintln!("\n{} publishers", events.len());
                eprintln!(
                    "Tip: `al-explorer trace <event>` shows the full chain; \
                     subscriber coverage is workspace source only (symbol packages \
                     don't carry subscriber metadata)."
                );
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
                // be explicit about coverage — Microsoft symbol
                // packages strip EventSubscriber attributes, so package
                // subscribers are fundamentally invisible to any tool.
                eprintln!(
                    "Note: covers source code in this workspace. Symbol (.app) packages do \
                     not carry subscriber metadata, so subscribers living in referenced \
                     packages cannot be listed (platform limitation)."
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// /resolve and show the actual publisher declaration behind the
/// `[EventSubscriber(...)]` attribute at FILE:LINE.
pub fn cmd_event_source(file: &str, line: u32, json: bool) -> ExitCode {
    let abs = absolutize_path(file);
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "file": abs, "line": line });
    match client.request("eventSource", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
                return ExitCode::SUCCESS;
            }
            let kind = result
                .get("targetKind")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let obj = result
                .get("targetObject")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let evt = result
                .get("targetEvent")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("Publisher: {kind} \"{obj}\" — event {evt}");
            if let Some(sig) = result.get("signature").and_then(|v| v.as_str()) {
                println!("  {sig}");
            }
            if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
                let decl_line = result.get("line").and_then(|v| v.as_u64()).unwrap_or(1);
                println!("  at {path}:{decl_line}");
                if let Some(availability) = result
                    .get("sourceAvailability")
                    .and_then(|value| value.as_str())
                {
                    eprintln!("  ({})", availability.replace('_', " "));
                }
                if result
                    .get("fromPackage")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    eprintln!("  (source extracted from symbol package)");
                }
                ExitCode::SUCCESS
            } else {
                if let Some(note) = result.get("note").and_then(|v| v.as_str()) {
                    eprintln!("{note}");
                }
                ExitCode::FAILURE
            }
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_composed(kind_or_name: &str, name: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    // Two forms (the Zed task only has the symbol under
    // the cursor): `composed <kind> <name>` and `composed <name>` (kind
    // resolved daemon-side, with an actionable error when ambiguous).
    let params = match name {
        Some(n) => serde_json::json!({ "kind": kind_or_name, "name": n }),
        None => serde_json::json!({ "name": kind_or_name }),
    };
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
                    "{:<34} {:<22} {:<15} {:>8}  SOURCE E/O/M",
                    "NAME", "PUBLISHER", "VERSION", "OBJECTS"
                );
                println!("{}", "-".repeat(105));
                let mut total_objects = 0u64;
                for p in pkgs {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let publisher = p.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                    let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let count = p.get("object_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    let source = p.get("source_availability");
                    let embedded = source
                        .and_then(|value| value.get("embedded_source"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let outline = source
                        .and_then(|value| value.get("generated_outline"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let metadata = source
                        .and_then(|value| value.get("metadata_only"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    total_objects += count;
                    println!(
                        "{:<34} {:<22} {:<15} {:>8}  {embedded}/{outline}/{metadata}",
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
