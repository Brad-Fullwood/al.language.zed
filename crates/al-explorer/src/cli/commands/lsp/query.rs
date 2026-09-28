//! Symbol-lookup and dependency queries: search, object/by-id lookup, events/subscribers, composed objects, packages, and deps.

use std::process::ExitCode;

use crate::cli::commands::*;

/// `limit` is the global `--limit`. For `search` it is the index's own search
/// bound as well as the page size, which is why it is passed rather than left
/// to the projection layer: a bound of 20 keeps the index from ranking every
/// symbol in Base Application before the page is cut.
pub fn cmd_search(query: &str, limit: Option<usize>, json: bool) -> ExitCode {
    const DEFAULT_SEARCH_LIMIT: usize = 20;
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({
        "query": query,
        "limit": limit.unwrap_or(DEFAULT_SEARCH_LIMIT),
    });
    match request_checked(&mut client, "search", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let entries = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
                if entries.is_empty() {
                    eprintln!("No results for '{query}'");
                    return ExitCode::SUCCESS;
                }
                print!("{}", search_table_text(entries));
                eprintln!("\n{} results", entries.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The table `search` prints, a header and one row per entry.
fn search_table_text(entries: &[serde_json::Value]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<18} {:>6}  {:<34} {:<24} SOURCE",
        "KIND", "ID", "NAME", "PACKAGE"
    );
    let _ = writeln!(out, "{}", "-".repeat(105));
    for e in entries {
        let kind = text_field(e, "kind", "?");
        let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = text_field(e, "name", "?");
        let pkg = text_field(e, "package", "?");
        let source = text_field(e, "source_availability", "unknown").replace('_', " ");
        // Interfaces & co. have no developer-visible object ID —
        // symbol packages put an internal compiler hash in the
        // Id slot. Render blank instead of the hash or -1.
        let id_text = if id > 0 && kind_has_numeric_id(&kind) {
            id.to_string()
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "{:<18} {:>6}  {:<34} {:<24} {}",
            kind, id_text, name, pkg, source
        );
    }
    out
}

pub fn cmd_object(kind: &str, name: &str, wait_for_members: bool, json: bool) -> ExitCode {
    let params = serde_json::json!({
        "kind": kind,
        "name": name,
        "waitForMembers": wait_for_members,
    });
    run_command("object", Some(params), json, None, |result| {
        print_symbol_entries(result);
    })
}

pub fn cmd_by_id(kind: &str, id: i32, wait_for_members: bool, json: bool) -> ExitCode {
    let params = serde_json::json!({
        "kind": kind,
        "id": id,
        "waitForMembers": wait_for_members,
    });
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
    list_procedures: bool,
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
    if list_procedures {
        params.insert("listProcedures".into(), true.into());
    }

    match request_checked(
        &mut client,
        "source",
        Some(serde_json::Value::Object(params)),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if list_procedures {
                let members = result
                    .get("members")
                    .and_then(|value| value.as_array())
                    .map(|value| &value[..])
                    .unwrap_or(&[]);
                println!("{} members of '{name}':", members.len());
                for member in members {
                    let start = member
                        .get("startLine")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let end = member.get("endLine").and_then(|v| v.as_u64()).unwrap_or(0);
                    let signature = member
                        .get("signature")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    println!("  {start:>6}-{end:<6} {signature}");
                }
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

/// Where an object is declared, over the daemon's existing `location` method.
///
/// The method had no CLI subcommand and no MCP tool, so agents asked for a
/// whole object's source and read the path off it, or fell back to `find`.
pub fn cmd_location(name: &str, kind: Option<&str>, package: Option<&str>, json: bool) -> ExitCode {
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
    match request_checked(
        &mut client,
        "location",
        Some(serde_json::Value::Object(params)),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                // A package object is materialised as a virtual .al file, so
                // there is a real path either way.
                let path = result
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?");
                let line = result
                    .get("line")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(1);
                println!("{path}:{line}");
                if let Some(availability) = result
                    .get("source_availability")
                    .and_then(|value| value.as_str())
                {
                    eprintln!("Source: {}", availability.replace('_', " "));
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
    match request_checked(&mut client, "events", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let events = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
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
                        let subs = match request_checked(
                            &mut client,
                            "subscribers",
                            Some(serde_json::json!({ "event": method })),
                        ) {
                            Ok(subscribers) => subscribers,
                            Err(error) => {
                                return report_error(
                                    &format!(
                                        "subscriber enrichment for event '{method}' failed: {error}"
                                    ),
                                    json,
                                );
                            }
                        };
                        let subs = subs
                            .as_array()
                            .expect("checked subscriber response must be an array");
                        let mine: Vec<_> = subs
                            .iter()
                            .filter(|s| {
                                s.get("targetObjectName")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|t| t.eq_ignore_ascii_case(obj_name))
                            })
                            .collect();
                        if mine.is_empty() {
                            println!("  ← no workspace subscribers");
                        }
                        for s in mine {
                            let sobj = s
                                .get("objectName")
                                .and_then(|v| v.as_str())
                                .expect("checked subscriber has objectName");
                            let smethod = s
                                .get("methodName")
                                .and_then(|v| v.as_str())
                                .expect("checked subscriber has methodName");
                            println!("  ← subscribed by {sobj}.{smethod}");
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
    match request_checked(&mut client, "subscribers", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let subs = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
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
    let abs = match absolutize_path(file) {
        Ok(path) => path,
        Err(error) => return report_error(&error, json),
    };
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "file": abs, "line": line });
    match request_checked(&mut client, "eventSource", Some(params)) {
        Ok(result) => {
            let exit_code = event_source_exit_code(&result);
            if json {
                print_json(&result);
                return exit_code;
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

fn event_source_exit_code(result: &serde_json::Value) -> ExitCode {
    if result
        .get("path")
        .and_then(|value| value.as_str())
        .is_some()
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
    match request_checked(&mut client, "composed", Some(params)) {
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
    match request_checked(&mut client, "packages", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let pkgs = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
                if pkgs.is_empty() {
                    eprintln!("No packages loaded (is .alpackages/ empty?)");
                    return ExitCode::SUCCESS;
                }
                let (table, total_objects) = packages_table_text(pkgs);
                print!("{table}");
                eprintln!("\n{} packages, {} total objects", pkgs.len(), total_objects);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The table `packages` prints, and the sum of the packages' object counts.
fn packages_table_text(pkgs: &[serde_json::Value]) -> (String, u64) {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<34} {:<22} {:<15} {:>8}  SOURCE E/O/M",
        "NAME", "PUBLISHER", "VERSION", "OBJECTS"
    );
    let _ = writeln!(out, "{}", "-".repeat(105));
    let mut total_objects = 0u64;
    for p in pkgs {
        let name = text_field(p, "name", "?");
        let publisher = text_field(p, "publisher", "?");
        let version = text_field(p, "version", "?");
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
        let _ = writeln!(
            out,
            "{:<34} {:<22} {:<15} {:>8}  {embedded}/{outline}/{metadata}",
            name, publisher, version, count
        );
    }
    (out, total_objects)
}

pub fn cmd_deps(json: bool) -> ExitCode {
    run_command("deps", None, json, None, |result| {
        print!("{}", deps_text(result));
    })
}

/// The text `deps` prints: the project, its explicit dependencies, the total.
fn deps_text(result: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(proj) = result.get("project") {
        let name = text_field(proj, "name", "?");
        let publisher = text_field(proj, "publisher", "?");
        let version = text_field(proj, "version", "?");
        let _ = writeln!(out, "Project: {name} by {publisher} v{version}");
    }
    if let Some(deps) = result.get("explicit").and_then(|v| v.as_array()) {
        let _ = writeln!(out, "\nExplicit dependencies ({}):", deps.len());
        for d in deps {
            let name = text_field(d, "name", "?");
            let publisher = text_field(d, "publisher", "?");
            let version = text_field(d, "version", "?");
            let _ = writeln!(out, "  {name} by {publisher} v{version}");
        }
    }
    if let Some(all) = result.get("all").and_then(|v| v.as_array()) {
        let _ = writeln!(
            out,
            "\nAll dependencies (including implicit): {}",
            all.len()
        );
    }
    out
}

#[cfg(test)]
mod exit_status_tests {
    use super::*;

    #[test]
    fn metadata_only_event_source_is_not_reported_as_resolved() {
        assert_eq!(
            event_source_exit_code(&serde_json::json!({
                "path": null,
                "sourceAvailability": "metadata_only"
            })),
            ExitCode::FAILURE
        );
        assert_eq!(
            event_source_exit_code(&serde_json::json!({"path": "/tmp/publisher.al"})),
            ExitCode::SUCCESS
        );
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{deps_text, packages_table_text, search_table_text};

    const CRAFTED_NAME: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const ESCAPED_NAME: &str = r"Bad\u{1b}[31m Name\u{1b}[0m";

    fn assert_no_raw_control(text: &str) {
        assert!(
            !text.chars().any(|ch| ch.is_control() && ch != '\n'),
            "a control character reached the terminal text: {text:?}"
        );
    }

    #[test]
    fn a_search_row_prints_an_object_and_package_name_escaped() {
        let rows = [serde_json::json!({
            "kind": "Codeunit",
            "id": 50150,
            "name": CRAFTED_NAME,
            "package": "Dep\u{1b}[2J\u{1b}[H",
            "source_availability": "workspace"
        })];
        let text = search_table_text(&rows);
        assert_no_raw_control(&text);
        assert!(text.contains(ESCAPED_NAME), "got: {text}");
        assert!(text.contains(r"Dep\u{1b}[2J\u{1b}[H"), "got: {text}");
    }

    #[test]
    fn a_package_row_prints_its_name_publisher_and_version_escaped() {
        let rows = [serde_json::json!({
            "name": CRAFTED_NAME,
            "publisher": "Pub\u{1b}]0;pwned\u{7}lisher",
            "version": "1.0\u{1b}[H",
            "object_count": 2
        })];
        let (text, total) = packages_table_text(&rows);
        assert_eq!(total, 2);
        assert_no_raw_control(&text);
        assert!(text.contains(ESCAPED_NAME), "got: {text}");
        assert!(
            text.contains(r"Pub\u{1b}]0;pwned\u{7}lisher"),
            "got: {text}"
        );
        assert!(text.contains(r"1.0\u{1b}[H"), "got: {text}");
    }

    #[test]
    fn deps_prints_the_manifest_and_dependency_names_escaped() {
        let result = serde_json::json!({
            "project": {
                "name": "Test\u{1b}[31m App\u{7}",
                "publisher": "Pub\u{1b}]0;pwned\u{7}lisher",
                "version": "1.0.0.0"
            },
            "explicit": [{
                "name": "Dep\u{1b}[2J\u{1b}[H Missing",
                "publisher": "X",
                "version": "1.0.0.0"
            }],
            "all": []
        });
        let text = deps_text(&result);
        assert_no_raw_control(&text);
        assert!(
            text.contains(
                r"Project: Test\u{1b}[31m App\u{7} by Pub\u{1b}]0;pwned\u{7}lisher v1.0.0.0"
            ),
            "got: {text}"
        );
        assert!(
            text.contains(r"  Dep\u{1b}[2J\u{1b}[H Missing by X v1.0.0.0"),
            "got: {text}"
        );
    }
}
