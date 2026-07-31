use std::process::ExitCode;

use super::{connect, print_json, report_error, request_checked, run_command};

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
            } else if let Some(steps) = result.as_array() {
                if steps.is_empty() {
                    // be explicit when the input isn't an event rather
                    // than silently printing nothing.
                    println!("No event chain found for '{event}'.");
                    println!(
                        "'{event}' may not be an event — trace follows \
                         IntegrationEvent/BusinessEvent publishers. For consumers of a \
                         procedure or object, use `al-explorer impact {event}`."
                    );
                } else {
                    for step in steps {
                        let depth = step.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);
                        let indent = "  ".repeat(depth as usize);
                        let edge = step.get("edgeType").and_then(|v| v.as_str()).unwrap_or("?");
                        let node_type =
                            step.get("nodeType").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let object = step.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{indent}[{edge}] {node_type}: {object}::{name}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_entrypoints(json: bool) -> ExitCode {
    run_command("entrypoints", None, json, None, |result| {
        if let Some(entries) = result.as_array() {
            println!("Entry points ({} found):", entries.len());
            for e in entries {
                let obj = e.get("object_name").and_then(|v| v.as_str()).unwrap_or("?");
                let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  {obj}::{name}");
            }
        }
    })
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
                    println!("{content}");
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

pub fn cmd_dead_code(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match request_checked(&mut client, "deadCode", None) {
        Ok(result) => {
            let has_high_confidence = result.as_array().is_some_and(|findings| {
                findings.iter().any(|finding| {
                    finding
                        .get("confidence")
                        .and_then(|value| value.as_str())
                        .is_some_and(|confidence| confidence.eq_ignore_ascii_case("high"))
                })
            });
            if json {
                print_json(&result);
            } else {
                let unused = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
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

                    let print_table = |items: &[&serde_json::Value]| {
                        println!("{:<12} {:<32} {:<32} LOCATION", "KIND", "NAME", "OBJECT");
                        println!("{}", "-".repeat(100));
                        for item in items {
                            let kind = item.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                            let name = item.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                            let obj = item.get("obj").and_then(|v| v.as_str()).unwrap_or("?");
                            let file = item.get("f").and_then(|v| v.as_str()).unwrap_or("");
                            let line = item.get("l").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!("{:<12} {:<32} {:<32} {}:{}", kind, name, obj, file, line);
                        }
                    };

                    if !high.is_empty() {
                        println!(
                            "DEAD CODE — high confidence ({} findings, safe to act on):\n",
                            high.len()
                        );
                        print_table(&high);
                    }
                    if !medium.is_empty() {
                        if !high.is_empty() {
                            println!();
                        }
                        println!(
                            "POSSIBLY UNUSED — medium confidence ({} findings):",
                            medium.len()
                        );
                        println!(
                            "These have no name references in workspace AL source, but may \
                             be used via\nFieldRef/RecordRef by number, report layouts, other \
                             extensions, or the platform.\nVerify before removing.\n"
                        );
                        print_table(&medium);
                    }
                    eprintln!(
                        "\n{} findings ({} high, {} possibly-unused)",
                        unused.len(),
                        high.len(),
                        medium.len()
                    );
                }
            }
            if has_high_confidence {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
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
                let sym = result
                    .get("symbol")
                    .and_then(|v| v.as_str())
                    .unwrap_or(symbol);
                let impacted = result
                    .get("impacted")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                if impacted.is_empty() {
                    println!("No consumers found for '{sym}'.");
                } else {
                    println!("Impact analysis for '{sym}':\n");
                    println!("{:<15} {:<30} {:<15} DETAIL", "KIND", "NAME", "TYPE");
                    println!("{}", "-".repeat(75));
                    for entry in impacted {
                        let kind = entry.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = entry.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                        let impact_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let detail = entry
                            .get("proc")
                            .and_then(|v| v.as_str())
                            .or_else(|| entry.get("field").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        println!("{:<15} {:<30} {:<15} {}", kind, name, impact_type, detail);
                    }
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
                let name = result
                    .get("tableName")
                    .and_then(|v| v.as_str())
                    .unwrap_or(table);
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
                    println!("No objects reference table '{name}'.");
                } else {
                    println!(
                        "Table impact for '{name}' ({total} site(s) across {} object(s)):\n",
                        objects.len()
                    );
                    for obj in objects {
                        let kind = obj
                            .get("objectKind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let oname = obj
                            .get("objectName")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        println!("  {kind} {oname}");
                        if let Some(impacts) = obj.get("impacts").and_then(|v| v.as_array()) {
                            for imp in impacts {
                                let op =
                                    imp.get("operation").and_then(|v| v.as_str()).unwrap_or("?");
                                let hint = imp
                                    .get("locationHint")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                println!("      [{op}] {hint}");
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
                if chains.is_empty() {
                    println!("No event chain found for '{event}'.");
                    println!(
                        "'{event}' may not be an event — trace follows \
                         IntegrationEvent/BusinessEvent publishers."
                    );
                } else {
                    let pubobj = result
                        .get("publisherObject")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("Event propagation tree for '{event}' (publisher: {pubobj}):\n");
                    for root in chains {
                        print_chain_node(root, 0);
                    }
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

fn print_chain_node(node: &serde_json::Value, indent: usize) {
    let pad = "  ".repeat(indent);
    let edge = node.get("edgeKind").and_then(|v| v.as_str()).unwrap_or("?");
    let ntype = node.get("nodeType").and_then(|v| v.as_str()).unwrap_or("?");
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let object = node.get("object").and_then(|v| v.as_str()).unwrap_or("?");
    let cycle = node.get("cycle").and_then(|v| v.as_bool()).unwrap_or(false);
    let marker = if cycle { " (cycle)" } else { "" };
    println!("{pad}[{edge}] {ntype}: {object}::{name}{marker}");
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            print_chain_node(child, indent + 1);
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
                    println!("No events or subscribers found in the workspace.");
                } else {
                    let total = result
                        .get("totalEvents")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    println!("Event interception map ({total} event(s)):\n");
                    for ev in events {
                        let ename = ev.get("eventName").and_then(|v| v.as_str()).unwrap_or("?");
                        let pubobj = ev
                            .get("publisher")
                            .and_then(|p| p.get("objectName"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let count = ev
                            .get("subscriberCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!("  {pubobj}::{ename}  ({count} subscriber(s))");
                        if let Some(subs) = ev.get("subscribers").and_then(|v| v.as_array()) {
                            for s in subs {
                                let so =
                                    s.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                                let sm =
                                    s.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                                println!("      <- {so}.{sm}");
                            }
                        }
                    }
                    if !orphans.is_empty() {
                        println!(
                            "\nOrphan subscribers ({}) — target an event with no workspace publisher:",
                            orphans.len()
                        );
                        for o in orphans {
                            let oo = o.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                            let om = o.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                            let to = o
                                .get("targetObject")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let te = o.get("targetEvent").and_then(|v| v.as_str()).unwrap_or("?");
                            println!("  {oo}.{om} -> {to}::{te} (missing)");
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
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
                if points.is_empty() {
                    println!("No integration points found.");
                } else {
                    println!("Integration points ({} found):\n", points.len());
                    for (i, ip) in points.iter().enumerate() {
                        let evt = ip.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = ip.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                        let etype = ip.get("eventType").and_then(|v| v.as_str()).unwrap_or("?");
                        let example = ip.get("example").and_then(|v| v.as_str()).unwrap_or("");

                        println!("{}. {} ({}) — {}", i + 1, evt, etype, obj);

                        if let Some(params) = ip.get("params").and_then(|v| v.as_array()) {
                            let var_params: Vec<_> = params
                                .iter()
                                .filter(|p| {
                                    p.get("isVar").and_then(|v| v.as_bool()).unwrap_or(false)
                                })
                                .collect();
                            if !var_params.is_empty() {
                                let param_strs: Vec<String> = var_params
                                    .iter()
                                    .map(|p| {
                                        let name =
                                            p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                        let typ = p
                                            .get("typeName")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("?");
                                        format!("var {name}: {typ}")
                                    })
                                    .collect();
                                println!("   Var params: {}", param_strs.join(", "));
                            }
                        }

                        println!("   {example}\n");
                    }
                }
                if partial {
                    eprintln!(
                        "Note: Some call paths are still being analyzed. Results may be incomplete."
                    );
                }
            }
            if partial {
                ExitCode::from(75)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}
