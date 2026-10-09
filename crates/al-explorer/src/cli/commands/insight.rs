//! `al-explorer` insight subcommands: event traces, call graphs, entry points,
//! dead code and impact reports, printed as tables or raw JSON.

use std::fmt::Write as _;
use std::process::ExitCode;

use super::{
    connect, list_rows, print_json, report_error, request_checked, run_command, terminal_lines,
    terminal_text, text_field,
};

pub fn cmd_trace(event: &str, depth: usize, tree: bool, json: bool) -> ExitCode {
    if tree {
        return cmd_trace_chain(event, depth, json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({ "event": event, "depth": depth });
    match request_checked(&mut client, "trace", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if let Some(steps) = list_rows(&result).as_array() {
                print!("{}", trace_text(event, steps));
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The text `trace` prints: one line per step, or why there is none.
fn trace_text(event: &str, steps: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let event = terminal_text(event);
    if steps.is_empty() {
        // be explicit when the input isn't an event rather
        // than silently printing nothing.
        let _ = writeln!(out, "No event chain found for '{event}'.");
        let _ = writeln!(
            out,
            "'{event}' may not be an event — trace follows \
             IntegrationEvent/BusinessEvent publishers. For consumers of a \
             procedure or object, use `al-explorer impact {event}`."
        );
    }
    for step in steps {
        let depth = step.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);
        let indent = "  ".repeat(depth as usize);
        let edge = text_field(step, "edgeType", "?");
        let node_type = text_field(step, "nodeType", "?");
        let name = text_field(step, "name", "?");
        let object = text_field(step, "object", "?");
        let _ = writeln!(out, "{indent}[{edge}] {node_type}: {object}::{name}");
    }
    out
}

pub fn cmd_entrypoints(json: bool) -> ExitCode {
    run_command("entrypoints", None, json, None, |result| {
        print!("{}", entrypoints_text(result));
    })
}

/// The text `entrypoints` prints: a count and one `object::procedure` line
/// per entry point.
fn entrypoints_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(entries) = list_rows(result).as_array() {
        let _ = writeln!(out, "Entry points ({} found):", entries.len());
        for e in entries {
            let obj = text_field(e, "object_name", "?");
            let name = text_field(e, "name", "?");
            let _ = writeln!(out, "  {obj}::{name}");
        }
    }
    out
}

pub fn cmd_graph(format: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({ "format": format });
    match request_checked(&mut client, "graphExport", Some(params)) {
        Ok(result) => {
            if format == "dot" {
                if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                    println!("{}", terminal_lines(content));
                }
            } else if json {
                print_json(&result);
            } else {
                let node_count = result
                    .get("nodes")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let edge_count = result
                    .get("edges")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                println!("Graph: {node_count} nodes, {edge_count} edges");
                println!("Use --json for full graph data or --format dot for Graphviz");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_insight_stats(json: bool) -> ExitCode {
    run_command("insightStats", None, json, None, |result| {
        let nodes = result.get("nodes").and_then(|v| v.as_u64()).unwrap_or(0);
        let edges = result.get("edges").and_then(|v| v.as_u64()).unwrap_or(0);
        println!("Insight graph: {nodes} nodes, {edges} edges");
    })
}

/// Any finding fails the `dead-code` gate.
///
/// Gating on high confidence alone let a workspace whose findings are all
/// `Confidence::Medium` — what al-analysis emits for an unreferenced object —
/// print the whole "possibly unused" table and exit 0, against the contract in
/// `Docs/reference/cli-commands.md`.
pub(crate) fn dead_code_exit_code(result: &serde_json::Value) -> ExitCode {
    let empty = list_rows(result)
        .as_array()
        .is_none_or(|findings| findings.is_empty());
    if empty {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn cmd_dead_code(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match request_checked(&mut client, "deadCode", None) {
        Ok(result) => {
            let exit = dead_code_exit_code(&result);
            if json {
                print_json(&result);
            } else {
                let unused = list_rows(&result).as_array().map(|v| &v[..]).unwrap_or(&[]);
                if unused.is_empty() {
                    println!("No dead code found.");
                } else {
                    // group by confidence so provably-dead findings
                    // are not mixed with "no references found, but could be
                    // used through channels static analysis can't see".
                    let (high, medium): (Vec<_>, Vec<_>) = unused.iter().partition(|item| {
                        item.get("confidence")
                            .and_then(|v| v.as_str())
                            .map(|c| c.eq_ignore_ascii_case("high"))
                            .unwrap_or(true)
                    });
                    print!("{}", dead_code_text(&high, &medium));
                    eprintln!(
                        "\n{} findings ({} high, {} possibly-unused)",
                        unused.len(),
                        high.len(),
                        medium.len()
                    );
                }
            }
            exit
        }
        Err(e) => report_error(&e, json),
    }
}

/// The tables `dead-code` prints, high confidence findings first.
fn dead_code_text(high: &[&serde_json::Value], medium: &[&serde_json::Value]) -> String {
    let mut out = String::new();
    if !high.is_empty() {
        let _ = writeln!(
            out,
            "DEAD CODE — high confidence ({} findings, safe to act on):\n",
            high.len()
        );
        dead_code_table(&mut out, high);
    }
    if !medium.is_empty() {
        if !high.is_empty() {
            let _ = writeln!(out);
        }
        let _ = writeln!(
            out,
            "POSSIBLY UNUSED — medium confidence ({} findings):",
            medium.len()
        );
        let _ = writeln!(
            out,
            "These have no name references in workspace AL source, but may \
             be used via\nFieldRef/RecordRef by number, report layouts, other \
             extensions, or the platform.\nVerify before removing.\n"
        );
        dead_code_table(&mut out, medium);
    }
    out
}

fn dead_code_table(out: &mut String, items: &[&serde_json::Value]) {
    let _ = writeln!(
        out,
        "{:<12} {:<32} {:<32} LOCATION",
        "KIND", "NAME", "OBJECT"
    );
    let _ = writeln!(out, "{}", "-".repeat(100));
    for item in items {
        let kind = text_field(item, "k", "?");
        let name = text_field(item, "n", "?");
        let obj = text_field(item, "obj", "?");
        let file = text_field(item, "f", "");
        let line = item.get("l").and_then(|v| v.as_u64()).unwrap_or(0);
        let _ = writeln!(
            out,
            "{:<12} {:<32} {:<32} {}:{}",
            kind, name, obj, file, line
        );
    }
}

pub fn cmd_impact(symbol: &str, table: bool, json: bool) -> ExitCode {
    if table {
        return cmd_table_impact(symbol, json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match request_checked(
        &mut client,
        "impact",
        Some(serde_json::json!({ "symbol": symbol })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let impacted = result
                    .get("impacted")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                print!("{}", impact_text(symbol, &result, impacted));
                if !impacted.is_empty() {
                    eprintln!("\n{} consumers", impacted.len());
                }
                // Be explicit about coverage. Call-site consumers (who calls /
                // reads / writes this symbol) are derived from workspace AL
                // source bodies; structural relations (extends, table relations,
                // event subscriptions) include loaded symbol packages. Microsoft
                // `.app` symbol files carry no method bodies, so call-sites that
                // live inside dependency code cannot be enumerated.
                eprintln!(
                    "Note: call-site consumers come from workspace source; consumers inside \
                     referenced .app packages aren't listed (symbol files carry no method bodies)."
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The table `impact` prints for `symbol`, one row per consumer.
fn impact_text(symbol: &str, result: &serde_json::Value, impacted: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let sym = terminal_text(
        result
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or(symbol),
    );
    if impacted.is_empty() {
        let _ = writeln!(out, "No consumers found for '{sym}'.");
        return out;
    }
    let _ = writeln!(out, "Impact analysis for '{sym}':\n");
    let _ = writeln!(out, "{:<15} {:<30} {:<15} DETAIL", "KIND", "NAME", "TYPE");
    let _ = writeln!(out, "{}", "-".repeat(75));
    for entry in impacted {
        let kind = text_field(entry, "k", "?");
        let name = text_field(entry, "n", "?");
        let impact_type = text_field(entry, "type", "?");
        let detail = terminal_text(
            entry
                .get("proc")
                .and_then(|v| v.as_str())
                .or_else(|| entry.get("field").and_then(|v| v.as_str()))
                .unwrap_or(""),
        );
        let _ = writeln!(
            out,
            "{:<15} {:<30} {:<15} {}",
            kind, name, impact_type, detail
        );
    }
    out
}

fn cmd_table_impact(table: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(
        &mut client,
        "tableImpact",
        Some(serde_json::json!({ "table": table })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                print!("{}", table_impact_text(table, &result));
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The text `impact --table` prints: each object that touches the table and
/// how.
fn table_impact_text(table: &str, result: &serde_json::Value) -> String {
    let mut out = String::new();
    let name = terminal_text(
        result
            .get("tableName")
            .and_then(|v| v.as_str())
            .unwrap_or(table),
    );
    let objects = result
        .get("objects")
        .and_then(|v| v.as_array())
        .map(|v| &v[..])
        .unwrap_or(&[]);
    let total = result
        .get("totalImpacts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if objects.is_empty() {
        let _ = writeln!(out, "No objects reference table '{name}'.");
        return out;
    }
    let _ = writeln!(
        out,
        "Table impact for '{name}' ({total} site(s) across {} object(s)):\n",
        objects.len()
    );
    for obj in objects {
        let kind = text_field(obj, "objectKind", "?");
        let oname = text_field(obj, "objectName", "?");
        let _ = writeln!(out, "  {kind} {oname}");
        if let Some(impacts) = obj.get("impacts").and_then(|v| v.as_array()) {
            for imp in impacts {
                let operation = text_field(imp, "operation", "?");
                let hint = text_field(imp, "locationHint", "");
                let _ = writeln!(out, "      [{operation}] {hint}");
            }
        }
    }
    out
}

fn cmd_trace_chain(event: &str, depth: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "event": event, "depth": depth });
    match request_checked(&mut client, "traceChain", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let chains = result
                    .get("chains")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                print!("{}", trace_chain_text(event, &result, chains));
                if !chains.is_empty() {
                    let visited = result
                        .get("nodesVisited")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    eprintln!("\n{visited} node(s) visited");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The tree `trace --tree` prints, or why there is none.
fn trace_chain_text(
    event: &str,
    result: &serde_json::Value,
    chains: &[serde_json::Value],
) -> String {
    let mut out = String::new();
    let event = terminal_text(event);
    if chains.is_empty() {
        let _ = writeln!(out, "No event chain found for '{event}'.");
        let _ = writeln!(
            out,
            "'{event}' may not be an event — trace follows \
             IntegrationEvent/BusinessEvent publishers."
        );
        return out;
    }
    let pubobj = text_field(result, "publisherObject", "");
    let _ = writeln!(
        out,
        "Event propagation tree for '{event}' (publisher: {pubobj}):\n"
    );
    for root in chains {
        chain_node_text(&mut out, root, 0);
    }
    out
}

fn chain_node_text(out: &mut String, node: &serde_json::Value, indent: usize) {
    let pad = "  ".repeat(indent);
    let edge = text_field(node, "edgeKind", "?");
    let ntype = text_field(node, "nodeType", "?");
    let name = text_field(node, "name", "?");
    let object = text_field(node, "object", "?");
    let cycle = node.get("cycle").and_then(|v| v.as_bool()).unwrap_or(false);
    let marker = if cycle { " (cycle)" } else { "" };
    let _ = writeln!(out, "{pad}[{edge}] {ntype}: {object}::{name}{marker}");
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            chain_node_text(out, child, indent + 1);
        }
    }
}

pub fn cmd_intercept(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(&mut client, "eventMap", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                print!("{}", event_map_text(&result));
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

/// The text `intercept` prints: each event with its subscribers, then the
/// subscribers whose event has no workspace publisher.
fn event_map_text(result: &serde_json::Value) -> String {
    let mut out = String::new();
    let events = result
        .get("events")
        .and_then(|v| v.as_array())
        .map(|v| &v[..])
        .unwrap_or(&[]);
    let orphans = result
        .get("orphanSubscribers")
        .and_then(|v| v.as_array())
        .map(|v| &v[..])
        .unwrap_or(&[]);
    if events.is_empty() && orphans.is_empty() {
        let _ = writeln!(out, "No events or subscribers found in the workspace.");
        return out;
    }
    let total = result
        .get("totalEvents")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let _ = writeln!(out, "Event interception map ({total} event(s)):\n");
    for ev in events {
        let ename = text_field(ev, "eventName", "?");
        let pubobj = ev
            .get("publisher")
            .map_or_else(|| "?".to_string(), |p| text_field(p, "objectName", "?"));
        let count = ev
            .get("subscriberCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let _ = writeln!(out, "  {pubobj}::{ename}  ({count} subscriber(s))");
        if let Some(subs) = ev.get("subscribers").and_then(|v| v.as_array()) {
            for subscriber in subs {
                let sub_object = text_field(subscriber, "objectName", "?");
                let sub_method = text_field(subscriber, "methodName", "?");
                let _ = writeln!(out, "      <- {sub_object}.{sub_method}");
            }
        }
    }
    if !orphans.is_empty() {
        let _ = writeln!(
            out,
            "\nOrphan subscribers ({}) — target an event with no workspace publisher:",
            orphans.len()
        );
        for orphan in orphans {
            let orphan_object = text_field(orphan, "objectName", "?");
            let orphan_method = text_field(orphan, "methodName", "?");
            let target_object = text_field(orphan, "targetObject", "?");
            let target_event = text_field(orphan, "targetEvent", "?");
            let _ = writeln!(
                out,
                "  {orphan_object}.{orphan_method} -> {target_object}::{target_event} (missing)"
            );
        }
    }
    out
}

/// Why a suggest-event answer leaves something out.
fn print_suggest_event_gaps(result: &serde_json::Value) {
    if result.get("depthCut").and_then(|v| v.as_bool()) == Some(true) {
        let depth = result
            .get("maxDepth")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);
        eprintln!(
            "Note: the trace stops {depth} calls deep, so events further down are not \
             listed. Start from a deeper --procedure to see them."
        );
    }
    let count = result
        .get("withoutSourceCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if count > 0 {
        let names: Vec<String> = result
            .get("withoutSource")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .take(5)
            .map(terminal_text)
            .collect();
        let more = if count as usize > names.len() {
            format!(" and {} more", count as usize - names.len())
        } else {
            String::new()
        };
        eprintln!(
            "Note: {count} procedure(s) on the trace have no source loaded (package code), so \
             the events they raise are not followed: {}{more}.",
            names.join(", ")
        );
    }
}

/// The integration points `suggest-event` prints, each with its `var`
/// parameters and a subscriber attribute to copy.
fn integration_points_text(points: &[serde_json::Value]) -> String {
    let mut out = String::new();
    if points.is_empty() {
        let _ = writeln!(out, "No integration points found.");
        return out;
    }
    let _ = writeln!(out, "Integration points ({} found):\n", points.len());
    for (i, ip) in points.iter().enumerate() {
        let evt = text_field(ip, "event", "?");
        let obj = text_field(ip, "object", "?");
        let etype = text_field(ip, "eventType", "?");
        let example = text_field(ip, "example", "");

        let _ = writeln!(out, "{}. {} ({}) — {}", i + 1, evt, etype, obj);

        if let Some(params) = ip.get("params").and_then(|v| v.as_array()) {
            let param_strs: Vec<String> = params
                .iter()
                .filter(|p| p.get("isVar").and_then(|v| v.as_bool()).unwrap_or(false))
                .map(|p| {
                    let name = text_field(p, "name", "?");
                    let typ = text_field(p, "typeName", "?");
                    format!("var {name}: {typ}")
                })
                .collect();
            if !param_strs.is_empty() {
                let _ = writeln!(out, "   Var params: {}", param_strs.join(", "));
            }
        }

        let _ = writeln!(out, "   {example}\n");
    }
    out
}

pub fn cmd_suggest_event(
    object: Option<String>,
    kind: Option<String>,
    procedure: Option<String>,
    table: Option<String>,
    field: Option<String>,
    event: Option<String>,
    json: bool,
) -> ExitCode {
    let source = if let Some(ref evt) = event {
        let obj = match &object {
            Some(o) => o.as_str(),
            None => {
                eprintln!("Error: --event requires --object");
                return ExitCode::FAILURE;
            }
        };
        serde_json::json!({ "type": "event", "object": obj, "event": evt })
    } else if let Some(ref obj) = object {
        let mut src = serde_json::Map::new();
        src.insert("type".to_string(), serde_json::json!("procedure"));
        src.insert("object".to_string(), serde_json::json!(obj));
        if let Some(ref proc_name) = procedure {
            src.insert("procedure".to_string(), serde_json::json!(proc_name));
        }
        serde_json::Value::Object(src)
    } else if let Some(ref tbl) = table {
        serde_json::json!({ "type": "table", "table": tbl })
    } else {
        eprintln!("Error: specify --object, --table, or --event");
        return ExitCode::FAILURE;
    };

    let mut query_map = serde_json::Map::new();
    query_map.insert("source".to_string(), source);
    if let Some(ref object_kind) = kind {
        query_map.insert("objectKind".to_string(), serde_json::json!(object_kind));
    }
    // If --table is provided alongside --object, it becomes a filter
    if object.is_some() {
        if let Some(ref tbl) = table {
            query_map.insert("filterTable".to_string(), serde_json::json!(tbl));
        }
    }
    if let Some(ref f) = field {
        query_map.insert("filterField".to_string(), serde_json::json!(f));
    }
    let query = serde_json::Value::Object(query_map);

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match request_checked(
        &mut client,
        "suggestEvent",
        Some(serde_json::json!({ "query": query })),
    ) {
        Ok(result) => {
            let partial = result
                .get("partial")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if json {
                print_json(&result);
            } else {
                let points = result
                    .get("integrationPoints")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                print!("{}", integration_points_text(points));
                if partial {
                    print_suggest_event_gaps(&result);
                }
            }
            // A cut trace is the whole answer to this query, not a transient
            // state: retrying gives the same result, so it is not exit 75.
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

#[cfg(test)]
mod terminal_text_tests {
    use super::{dead_code_text, entrypoints_text, impact_text, trace_chain_text};

    const COLOURED: &str = "Bad\u{1b}[31m Name\u{1b}[0m";
    const TITLE: &str = "Sec7 Caller\u{1b}]0;pwned\u{7}";
    const CLEAR: &str = "Sec7 Tests\u{1b}[2J";

    fn assert_escaped(text: &str) {
        assert!(!text.contains('\u{1b}'), "a raw escape byte: {text:?}");
        assert!(!text.contains('\u{7}'), "a raw bell byte: {text:?}");
        assert!(text.contains(r"Bad\u{1b}[31m Name\u{1b}[0m"), "got: {text}");
        assert!(
            text.contains(r"Sec7 Caller\u{1b}]0;pwned\u{7}"),
            "got: {text}"
        );
        assert!(text.contains(r"Sec7 Tests\u{1b}[2J"), "got: {text}");
    }

    #[test]
    fn entry_points_dead_code_trace_and_impact_print_crafted_names_escaped() {
        let entrypoints = serde_json::json!([
            {"object_name": CLEAR, "name": "Adds"},
            {"object_name": TITLE, "name": "CallIt"},
            {"object_name": COLOURED, "name": "Run"}
        ]);
        let text = entrypoints_text(&entrypoints);
        assert_escaped(&text);
        assert!(text.contains(r"  Sec7 Tests\u{1b}[2J::Adds"), "got: {text}");

        let finding = |name: &str, obj: &str| serde_json::json!({"k": "procedure", "n": name, "obj": obj, "f": "src/A.al", "l": 3});
        let high = finding("CallIt", TITLE);
        let medium = finding(COLOURED, CLEAR);
        let text = dead_code_text(&[&high], &[&medium]);
        assert_escaped(&text);
        assert!(text.contains("src/A.al:3"), "got: {text}");

        let chain = serde_json::json!({
            "edgeKind": "raises",
            "nodeType": "event",
            "name": "OnRun",
            "object": COLOURED,
            "children": [{"edgeKind": "subscribes", "nodeType": "subscriber", "name": CLEAR, "object": TITLE}]
        });
        let text = trace_chain_text(COLOURED, &serde_json::json!({}), &[chain]);
        assert_escaped(&text);

        let impacted =
            [serde_json::json!({"k": "Codeunit", "n": TITLE, "type": "call", "proc": CLEAR})];
        let text = impact_text(COLOURED, &serde_json::json!({}), &impacted);
        assert_escaped(&text);
    }
}
