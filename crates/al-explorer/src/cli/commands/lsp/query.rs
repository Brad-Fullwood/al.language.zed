//! Symbol-lookup and dependency queries: search, object/by-id lookup, events/subscribers, composed objects, packages, and deps.

use std::fmt::Write as _;
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
                    eprintln!("No results for '{}'", terminal_text(query));
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
                print!("{}", source_members_text(name, &result));
            } else {
                print!("{}", source_origin_text(&result));
                if let Some(note) = result.get("note").and_then(|value| value.as_str()) {
                    eprintln!("Note: {}", terminal_text(note));
                }
                print!("{}", source_code_text(&result));
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error, json),
    }
}

/// The member list `source --list-procedures` prints.
fn source_members_text(name: &str, result: &serde_json::Value) -> String {
    let members = result
        .get("members")
        .and_then(|value| value.as_array())
        .map(|value| &value[..])
        .unwrap_or(&[]);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} members of '{}':",
        members.len(),
        terminal_text(name)
    );
    for member in members {
        let start = member
            .get("startLine")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let end = member.get("endLine").and_then(|v| v.as_u64()).unwrap_or(0);
        let signature = text_field(member, "signature", "?");
        let _ = writeln!(out, "  {start:>6}-{end:<6} {signature}");
    }
    out
}

/// The line `source` prints before the code: where the source comes from.
fn source_origin_text(result: &serde_json::Value) -> String {
    let availability = text_field(result, "source_availability", "unknown").replace('_', " ");
    match result.get("pkg").and_then(|value| value.as_str()) {
        Some(package) => format!("Source: {availability} ({})\n", terminal_text(package)),
        None => format!("Source: {availability}\n"),
    }
}

/// The code `source` prints, after a blank line, with its line breaks and
/// tabs kept.
fn source_code_text(result: &serde_json::Value) -> String {
    match result.get("code").and_then(|value| value.as_str()) {
        Some(code) => format!("\n{}\n", terminal_lines(code)),
        None => String::new(),
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
                print!("{}", location_text(&result));
                if let Some(availability) = result
                    .get("source_availability")
                    .and_then(|value| value.as_str())
                {
                    eprintln!("Source: {}", terminal_text(availability).replace('_', " "));
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error, json),
    }
}

/// The `path:line` that `location` prints. A package object is materialised
/// as a virtual .al file, so there is a real path either way.
fn location_text(result: &serde_json::Value) -> String {
    let path = text_field(result, "path", "?");
    let line = result
        .get("line")
        .and_then(|value| value.as_u64())
        .unwrap_or(1);
    format!("{path}:{line}\n")
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
                    eprintln!("No event publishers matching '{}'", terminal_text(name));
                    return ExitCode::SUCCESS;
                }
                // enrich each publisher with its workspace subscribers
                // so the result is a chain view, not a flat name list. One
                // extra daemon round-trip per distinct event name, capped.
                const SUBSCRIBER_LOOKUP_CAP: usize = 25;
                for (i, e) in events.iter().enumerate() {
                    let obj_name = e.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = e.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                    print!("{}", event_publisher_text(e));
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
                        print!("{}", workspace_subscribers_text(&mine));
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

/// The lines `events` prints for one publisher: the event and its
/// parameters.
fn event_publisher_text(event: &serde_json::Value) -> String {
    let mut out = String::new();
    let obj_kind = text_field(event, "objectKind", "?");
    let obj_name = text_field(event, "objectName", "?");
    let method = text_field(event, "methodName", "?");
    let event_type = text_field(event, "eventType", "?");
    let _ = writeln!(out, "[{event_type}] {obj_kind} \"{obj_name}\".{method}");
    if let Some(params) = event.get("parameters").and_then(|v| v.as_array()) {
        for p in params {
            let pname = text_field(p, "name", "?");
            let ptype = text_field(p, "type_name", "?");
            let is_var = p.get("is_var").and_then(|v| v.as_bool()).unwrap_or(false);
            let var_prefix = if is_var { "var " } else { "" };
            let _ = writeln!(out, "  {var_prefix}{pname}: {ptype}");
        }
    }
    out
}

/// The lines `events` prints under a publisher for the workspace
/// subscribers of its event.
fn workspace_subscribers_text(subscribers: &[&serde_json::Value]) -> String {
    if subscribers.is_empty() {
        return "  ← no workspace subscribers\n".to_string();
    }
    let mut out = String::new();
    for s in subscribers {
        let sobj = text_field(s, "objectName", "?");
        let smethod = text_field(s, "methodName", "?");
        let _ = writeln!(out, "  ← subscribed by {sobj}.{smethod}");
    }
    out
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
                    eprintln!("No subscribers for '{}'", terminal_text(event));
                    return ExitCode::SUCCESS;
                }
                print!("{}", subscribers_text(subs));
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

/// The lines `subscribers` prints, one per subscriber and the event it
/// subscribes to.
fn subscribers_text(subscribers: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for s in subscribers {
        let obj = text_field(s, "objectName", "?");
        let method = text_field(s, "methodName", "?");
        let target_type = text_field(s, "targetObjectType", "?");
        let target_name = text_field(s, "targetObjectName", "?");
        let target_event = text_field(s, "targetEventName", "?");
        let _ = writeln!(
            out,
            "{obj}.{method} → {target_type}::{target_name}.{target_event}"
        );
    }
    out
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
            print!("{}", event_source_text(&result));
            if result.get("path").and_then(|v| v.as_str()).is_some() {
                if let Some(availability) = result
                    .get("sourceAvailability")
                    .and_then(|value| value.as_str())
                {
                    eprintln!("  ({})", terminal_text(availability).replace('_', " "));
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
                    eprintln!("{}", terminal_text(note));
                }
                ExitCode::FAILURE
            }
        }
        Err(e) => report_error(&e, json),
    }
}

/// The lines `event-source` prints: the publisher, its signature and where
/// it is declared.
fn event_source_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    let kind = text_field(result, "targetKind", "");
    let obj = text_field(result, "targetObject", "?");
    let evt = text_field(result, "targetEvent", "?");
    let _ = writeln!(out, "Publisher: {kind} \"{obj}\" — event {evt}");
    if let Some(sig) = result.get("signature").and_then(|v| v.as_str()) {
        let _ = writeln!(out, "  {}", terminal_text(sig));
    }
    if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
        let decl_line = result.get("line").and_then(|v| v.as_u64()).unwrap_or(1);
        let _ = writeln!(out, "  at {}:{decl_line}", terminal_text(path));
    }
    out
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
                print!("{}", composed_text(&result));
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The text `composed` prints: the base object, its extensions and the
/// field and method totals.
fn composed_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(base) = result.get("base") {
        let bname = text_field(base, "name", "?");
        let bkind = text_field(base, "kind", "?");
        let _ = writeln!(out, "Base: {bkind} \"{bname}\"");
    }
    if let Some(exts) = result.get("extensions").and_then(|v| v.as_array()) {
        let _ = writeln!(out, "Extensions: {}", exts.len());
        for ext in exts {
            let ename = text_field(ext, "name", "?");
            let epkg = text_field(ext, "package", "?");
            let _ = writeln!(out, "  - \"{ename}\" ({epkg})");
        }
    }
    if let Some(fields) = result.get("all_fields").and_then(|v| v.as_array()) {
        let _ = writeln!(out, "Total fields: {}", fields.len());
    }
    if let Some(methods) = result.get("all_methods").and_then(|v| v.as_array()) {
        let _ = writeln!(out, "Total methods: {}", methods.len());
    }
    out
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
    use super::{
        composed_text, deps_text, event_publisher_text, event_source_text, location_text,
        packages_table_text, search_table_text, source_code_text, source_members_text,
        source_origin_text, subscribers_text, workspace_subscribers_text,
    };

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

    #[test]
    fn source_and_location_print_a_crafted_name_escaped_and_keep_the_code_lines() {
        let result = serde_json::json!({
            "members": [{"startLine": 3, "endLine": 9, "signature": CRAFTED_NAME}]
        });
        let text = source_members_text("Sec7\u{1b}[2J", &result);
        assert_no_raw_control(&text);
        assert!(
            text.contains(r"1 members of 'Sec7\u{1b}[2J':"),
            "got: {text}"
        );
        assert!(text.contains(ESCAPED_NAME), "got: {text}");

        let result = serde_json::json!({
            "source_availability": "embedded_source",
            "pkg": "Pub\u{1b}]0;pwned\u{7}",
            "code": "codeunit 50170 \"Bad\u{1b}[31m Name\"\n{\n\tprocedure Run()\n}"
        });
        let text = source_origin_text(&result);
        assert_eq!(text, "Source: embedded source (Pub\\u{1b}]0;pwned\\u{7})\n");
        let code = source_code_text(&result);
        assert!(
            !code.contains('\u{1b}') && code.contains("\n\tprocedure Run()\n"),
            "got: {code:?}"
        );
        assert!(code.contains(r#""Bad\u{1b}[31m Name""#), "got: {code}");

        let text = location_text(&serde_json::json!({"path": CRAFTED_NAME, "line": 4}));
        assert_eq!(text, format!("{ESCAPED_NAME}:4\n"));
    }

    #[test]
    fn events_subscribers_and_event_source_print_crafted_names_escaped() {
        let publisher = serde_json::json!({
            "objectKind": "Codeunit",
            "objectName": CRAFTED_NAME,
            "methodName": "OnRun\u{1b}[2J",
            "eventType": "IntegrationEvent",
            "parameters": [{"name": "Rec\u{1b}[H", "type_name": "Record", "is_var": true}]
        });
        let text = event_publisher_text(&publisher);
        assert_no_raw_control(&text);
        assert!(
            text.contains(
                r#"[IntegrationEvent] Codeunit "Bad\u{1b}[31m Name\u{1b}[0m".OnRun\u{1b}[2J"#
            ),
            "got: {text}"
        );
        assert!(text.contains(r"  var Rec\u{1b}[H: Record"), "got: {text}");

        let subscriber = serde_json::json!({
            "objectName": "Sec7 Caller\u{1b}]0;pwned\u{7}",
            "methodName": "OnAfter",
            "targetObjectType": "Codeunit",
            "targetObjectName": CRAFTED_NAME,
            "targetEventName": "OnRun"
        });
        let text = workspace_subscribers_text(&[&subscriber]);
        assert_eq!(
            text,
            "  ← subscribed by Sec7 Caller\\u{1b}]0;pwned\\u{7}.OnAfter\n"
        );
        let text = subscribers_text(std::slice::from_ref(&subscriber));
        assert_no_raw_control(&text);
        assert!(
            text.contains(r"Sec7 Caller\u{1b}]0;pwned\u{7}.OnAfter → Codeunit::Bad\u{1b}[31m Name\u{1b}[0m.OnRun"),
            "got: {text}"
        );

        let result = serde_json::json!({
            "targetKind": "Codeunit",
            "targetObject": CRAFTED_NAME,
            "targetEvent": "OnRun",
            "signature": "procedure OnRun\u{1b}[2J()",
            "path": "/src/Bad\u{1b}[31m.al",
            "line": 7
        });
        let text = event_source_text(&result);
        assert_no_raw_control(&text);
        assert!(text.contains(ESCAPED_NAME), "got: {text}");
        assert!(
            text.contains(r"  procedure OnRun\u{1b}[2J()"),
            "got: {text}"
        );
        assert!(
            text.contains(r"  at /src/Bad\u{1b}[31m.al:7"),
            "got: {text}"
        );
    }

    #[test]
    fn composed_prints_the_base_and_extension_names_escaped() {
        let result = serde_json::json!({
            "base": {"name": CRAFTED_NAME, "kind": "Table"},
            "extensions": [{"name": "Ext\u{1b}[2J", "package": "Pub\u{1b}]0;pwned\u{7}"}],
            "all_fields": [1, 2],
            "all_methods": []
        });
        let text = composed_text(&result);
        assert_no_raw_control(&text);
        assert!(
            text.contains(r#"Base: Table "Bad\u{1b}[31m Name\u{1b}[0m""#),
            "got: {text}"
        );
        assert!(
            text.contains(r#"  - "Ext\u{1b}[2J" (Pub\u{1b}]0;pwned\u{7})"#),
            "got: {text}"
        );
        assert!(
            text.contains("Total fields: 2\nTotal methods: 0\n"),
            "got: {text}"
        );
    }
}
