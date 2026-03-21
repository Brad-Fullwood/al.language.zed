use std::process::ExitCode;

use super::{connect, print_json, report_error};

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
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("entrypoints", None) {
        Ok(result) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            } else if let Some(entries) = result.as_array() {
                println!("Entry points ({} found):", entries.len());
                for e in entries {
                    let obj = e.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("  {obj}::{name}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
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
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("insightStats", None) {
        Ok(result) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                );
            } else {
                let nodes = result.get("nodes").and_then(|v| v.as_u64()).unwrap_or(0);
                let edges = result.get("edges").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("Insight graph: {nodes} nodes, {edges} edges");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
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
                    println!(
                        "{:<12} {:<30} {:<30} REASON",
                        "KIND", "NAME", "OBJECT"
                    );
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

    match client.request(
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
                    println!(
                        "{:<15} {:<30} {:<15} DETAIL",
                        "KIND", "NAME", "TYPE"
                    );
                    println!("{}", "-".repeat(75));
                    for entry in impacted {
                        let kind = entry.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = entry.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                        let impact_type =
                            entry.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let detail = entry
                            .get("proc")
                            .and_then(|v| v.as_str())
                            .or_else(|| entry.get("field").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        println!(
                            "{:<15} {:<30} {:<15} {}",
                            kind, name, impact_type, detail
                        );
                    }
                    eprintln!("\n{} consumers", impacted.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_suggest_event(description: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request(
        "suggestEvent",
        Some(serde_json::json!({ "description": description })),
    ) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let suggestions = result
                    .get("suggestions")
                    .and_then(|v| v.as_array())
                    .map(|v| &v[..])
                    .unwrap_or(&[]);
                if suggestions.is_empty() {
                    println!("No matching events found for: {description}");
                } else {
                    println!("Suggested events for: {description}\n");
                    for (i, s) in suggestions.iter().enumerate() {
                        let event = s.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = s.get("obj").and_then(|v| v.as_str()).unwrap_or("?");
                        let etype = s.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let why = s.get("why").and_then(|v| v.as_str()).unwrap_or("");
                        let example = s.get("example").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{}. {} ({}) — {}", i + 1, event, etype, obj);
                        println!("   {why}");
                        println!("   {example}\n");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
