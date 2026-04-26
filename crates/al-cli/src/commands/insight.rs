use std::process::ExitCode;

use super::{connect, print_json, report_error, run_command};

pub fn cmd_trace(event: &str, depth: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({ "event": event, "depth": depth });
    match client.request("trace", Some(params)) {
        Ok(result) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            } else if let Some(steps) = result.as_array() {
                if steps.is_empty() {
                    println!("No event chain found for '{event}'");
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
    match client.request("graphExport", Some(params)) {
        Ok(result) => {
            if format == "dot" {
                if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                    println!("{content}");
                }
            } else if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
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

    match client.request("deadCode", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let unused = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if unused.is_empty() {
                    println!("No dead code found.");
                } else {
                    println!("{:<12} {:<30} {:<30} REASON", "KIND", "NAME", "OBJECT");
                    println!("{}", "-".repeat(85));
                    for item in unused {
                        let kind = item.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = item.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = item.get("obj").and_then(|v| v.as_str()).unwrap_or("?");
                        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{:<12} {:<30} {:<30} {}", kind, name, obj, reason);
                    }
                    eprintln!("\n{} unused symbols", unused.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_impact(symbol: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("impact", Some(serde_json::json!({ "symbol": symbol }))) {
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
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_suggest_event(
    object: Option<String>,
    procedure: Option<String>,
    table: Option<String>,
    field: Option<String>,
    event: Option<String>,
    json: bool,
) -> ExitCode {
    // Build the query source from CLI args
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

    match client.request("suggestEvent", Some(serde_json::json!({ "query": query }))) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let points = result
                    .get("integrationPoints")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                let partial = result
                    .get("partial")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

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

                        // Show var params
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
                    eprintln!("Note: Some call paths are still being analyzed. Results may be incomplete.");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
