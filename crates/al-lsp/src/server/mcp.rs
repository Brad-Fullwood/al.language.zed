//! MCP (Model Context Protocol) server mode — `al-lsp mcp`.
//!
//! Exposes a curated set of AL development tools to MCP-compatible agents
//! (Claude Code, Zed's agent panel via `context_servers`, custom agents)
//! over stdio, using newline-delimited JSON-RPC 2.0 per the MCP spec.
//!
//! Tool names mirror Microsoft's AL agent tools (`al_build`,
//! `al_symbolsearch`, `al_getdiagnostics`, …) so agents trained on the
//! official surface transfer, plus this project's differentiators
//! (dead-code, SQL anti-patterns, event tracing, impact analysis) that the
//! official tooling does not offer. Tool arguments are forwarded VERBATIM
//! as daemon-dispatch params — validation happens in the dispatchers,
//! which already return structured JSON-RPC errors.

use std::path::PathBuf;
use std::sync::Arc;

use al_protocol::jsonrpc::Request;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use al_workspace::Workspace;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// One exposed MCP tool: its public name, the daemon method it forwards to,
/// a description for the agent, and a JSON Schema for its arguments.
struct ToolDef {
    name: &'static str,
    method: &'static str,
    description: &'static str,
    schema: fn() -> serde_json::Value,
}

fn obj_schema(props: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": required,
    })
}

fn tools() -> &'static [ToolDef] {
    &[
        ToolDef {
            name: "al_build",
            method: "compile",
            description: "Compile the AL project. By default this uses the pure-Rust \
                          native `.app` emitter with no alc or C# bridge; set \
                          al.useOfficialCompiler=true to opt into dotnet alc for \
                          Microsoft compiler diagnostics. Returns success, \
                          diagnostics and the .app path.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_downloadsymbols",
            method: "downloadSymbols",
            description: "Download dependent symbol packages (.app) from the configured \
                          NuGet feeds (public Microsoft feeds by default) into .alpackages.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_symbolsearch",
            method: "search",
            description: "Fuzzy-search AL objects across loaded packages AND workspace \
                          source. Args: query (string), limit (number, default 20).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "query": {"type": "string"},
                        "limit": {"type": "number"}
                    }),
                    &["query"],
                )
            },
        },
        ToolDef {
            name: "al_getdiagnostics",
            method: "lint",
            description: "Run diagnostics on an AL file. Args: file (path).",
            schema: || obj_schema(serde_json::json!({"file": {"type": "string"}}), &["file"]),
        },
        ToolDef {
            name: "al_runtests",
            method: "tests.run_auto",
            description: "Discover and run the project's AL tests. Pure-logic tests run \
                          on the built-in interpreter (no BC server needed); others need \
                          a launch config + live BC.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_deadcode",
            method: "deadCode",
            description: "Find unused procedures, unreferenced table fields and orphaned \
                          event subscribers across the workspace.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_sqlscan",
            method: "sqlPatterns",
            description: "Detect SQL anti-patterns (FindFirst in loops, unfiltered \
                          FindSet, missing SetLoadFields, ...) across the workspace.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_entrypoints",
            method: "entrypoints",
            description: "List entry-point procedures (no incoming calls) from the \
                          workspace-enriched insight graph.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_trace_event",
            method: "trace",
            description: "Trace an event's propagation chain (publishers to subscribers). \
                          Args: event (string), depth (number, default 10).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "event": {"type": "string"},
                        "depth": {"type": "number"}
                    }),
                    &["event"],
                )
            },
        },
        ToolDef {
            name: "al_impact",
            method: "impact",
            description: "Dependency impact analysis - who consumes this symbol? \
                          Args: symbol (e.g. 'Customer', 'Sales-Post.PostDocument').",
            schema: || {
                obj_schema(
                    serde_json::json!({"symbol": {"type": "string"}}),
                    &["symbol"],
                )
            },
        },
        ToolDef {
            name: "al_suggestevent",
            method: "suggestEvent",
            description: "Suggest integration event publishers to subscribe to for a \
                          given starting point, by tracing the call/event graph. \
                          Args: query (object) with a `source` discriminated by `type`: \
                          {type:'procedure', object, procedure?}, {type:'table', table}, \
                          or {type:'event', object, event}; optional `filterTable` / \
                          `filterField` restrict results to events exposing that table \
                          (field) as a `var` parameter.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "query": {
                            "type": "object",
                            "description": "Structured event-discovery query.",
                            "properties": {
                                "source": {
                                    "type": "object",
                                    "description": "Where to start tracing. One of \
                                        {type:'procedure', object, procedure?}, \
                                        {type:'table', table}, \
                                        {type:'event', object, event}.",
                                    "properties": {
                                        "type": {
                                            "type": "string",
                                            "enum": ["procedure", "table", "event"]
                                        },
                                        "object": {"type": "string"},
                                        "procedure": {"type": "string"},
                                        "table": {"type": "string"},
                                        "event": {"type": "string"}
                                    },
                                    "required": ["type"]
                                },
                                "filterTable": {"type": "string"},
                                "filterField": {"type": "string"}
                            },
                            "required": ["source"]
                        }
                    }),
                    &["query"],
                )
            },
        },
        ToolDef {
            name: "al_testclassify",
            method: "tests.classify",
            description: "Classify every discovered AL test by WHERE it actually runs \
                          today. Pure-logic tests run locally on the built-in Rust \
                          interpreter (no Business Central server); tests that touch the \
                          database, UI, HTTP or transactions route to live BC. Each entry \
                          carries a routing `decision` (`interp` = runs locally; \
                          `interpRecord`/`liveBc`/`snapshot` = needs BC) plus the reasons \
                          that drove it — so an agent can see the local-vs-needs-BC split \
                          before running anything.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_testcoverage",
            method: "tests.coverage",
            description: "Report static test coverage across the workspace: which objects \
                          and procedures are reached by the project's AL tests (via the \
                          call graph) and which are uncovered. No live BC required.",
            schema: || obj_schema(serde_json::json!({}), &[]),
        },
        ToolDef {
            name: "al_depgraph",
            method: "deps.graph",
            description: "Build the project's dependency graph from app.json (this app plus \
                          its declared dependencies). Args: format (string) — 'json' \
                          (default, structured nodes/edges) or 'dot' (Graphviz source).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "format": {
                            "type": "string",
                            "enum": ["json", "dot"],
                            "description": "Output format; defaults to json."
                        }
                    }),
                    &[],
                )
            },
        },
    ]
}

/// Handle one parsed MCP message. Returns the response to write, or `None`
/// for notifications (which get no response).
pub(crate) async fn handle_mcp_message(
    workspace: &Arc<Workspace>,
    shutdown: &Notify,
    msg: serde_json::Value,
) -> Option<serde_json::Value> {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned();
    // Notifications (no id) get no response per JSON-RPC.
    let id = match id {
        Some(v) if !v.is_null() => v,
        _ => {
            tracing::debug!(method, "mcp: notification");
            return None;
        }
    };

    let respond = |result: serde_json::Value| {
        Some(serde_json::json!({"jsonrpc": "2.0", "id": id.clone(), "result": result}))
    };
    let respond_err = |code: i64, message: String| {
        Some(serde_json::json!({
            "jsonrpc": "2.0", "id": id.clone(),
            "error": {"code": code, "message": message}
        }))
    };

    match method {
        "initialize" => respond(serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "al-lsp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),
        "ping" => respond(serde_json::json!({})),
        "tools/list" => {
            let list: Vec<serde_json::Value> = tools()
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": (t.schema)(),
                    })
                })
                .collect();
            respond(serde_json::json!({"tools": list}))
        }
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_default();
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let Some(tool) = tools().iter().find(|t| t.name == tool_name) else {
                return respond_err(-32602, format!("Unknown tool: {tool_name}"));
            };
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            // Forward to the daemon dispatcher — same logic, different wire.
            let req = Request::new(0, tool.method, Some(arguments));
            let resp = super::daemon::dispatch_request(workspace, req, shutdown).await;

            let (text, is_error) = match (resp.result, resp.error) {
                (_, Some(err)) => (err.message, true),
                (Some(result), None) => (
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()),
                    false,
                ),
                (None, None) => ("null".to_string(), false),
            };
            respond(serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "isError": is_error,
            }))
        }
        other => respond_err(-32601, format!("Method not found: {other}")),
    }
}

/// Run the MCP server on stdio: newline-delimited JSON-RPC 2.0.
pub async fn run_mcp(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Arc::new(Workspace::new());
    let _ = workspace.notify_sink.set(Arc::new(|msg: &str| {
        tracing::warn!("mcp: {msg}");
    }));
    super::daemon::initialize_daemon_workspace(&workspace, &project_root).await;
    tracing::info!(project = %project_root.display(), tools = tools().len(), "MCP server ready");

    // Never triggered in MCP mode — exists because the shared dispatcher's
    // "shutdown" route signals it (an agent calling it just ends our loop
    // via stdin EOF anyway).
    let shutdown = Notify::new();

    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            tracing::info!("mcp: stdin closed, exiting");
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "mcp: malformed JSON line");
                let err = serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": format!("Parse error: {e}")}
                });
                stdout.write_all(err.to_string().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };
        if let Some(resp) = handle_mcp_message(&workspace, &shutdown, msg).await {
            stdout.write_all(resp.to_string().as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Arc<Workspace> {
        Arc::new(Workspace::new())
    }

    #[tokio::test]
    async fn initialize_reports_tools_capability_and_server_info() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await
        .expect("response");
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "al-lsp");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn tools_list_exposes_the_curated_registry() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await
        .expect("response");
        let list = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(list.len(), tools().len());
        let names: Vec<&str> = list.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "al_build",
            "al_symbolsearch",
            "al_getdiagnostics",
            "al_deadcode",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        for t in list {
            assert!(
                t["inputSchema"]["type"] == "object",
                "schema for {}",
                t["name"]
            );
        }
    }

    /// End-to-end through the shared dispatcher: a workspace object must be
    /// findable via the al_symbolsearch tool.
    #[tokio::test(flavor = "multi_thread")]
    async fn tools_call_symbolsearch_finds_workspace_objects() {
        let ws = ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/HelloWorld.al"),
            "codeunit 50100 \"Hello World\"\n{\n}\n".to_string(),
        );
        let resp = handle_mcp_message(
            &ws,
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params": {"name": "al_symbolsearch", "arguments": {"query": "Hello"}}
            }),
        )
        .await
        .expect("response");
        assert_eq!(resp["result"]["isError"], false, "resp: {resp}");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(text.contains("Hello World"), "search must find it: {text}");
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_an_error() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0","id":4,"method":"tools/call",
                "params": {"name": "al_frobnicate", "arguments": {}}
            }),
        )
        .await
        .expect("response");
        assert_eq!(resp["error"]["code"], -32602);
    }

    /// Every registered tool must advertise a well-formed JSON Schema and a
    /// non-empty description, and every `required` field must actually be
    /// declared in `properties` (otherwise an agent cannot satisfy it).
    #[test]
    fn every_tool_has_a_valid_schema_and_description() {
        for t in tools() {
            assert!(!t.name.trim().is_empty(), "tool name must be non-empty");
            assert!(
                !t.method.trim().is_empty(),
                "{}: method must be non-empty",
                t.name
            );
            assert!(
                !t.description.trim().is_empty(),
                "{}: description must be non-empty",
                t.name
            );

            let schema = (t.schema)();
            assert_eq!(
                schema["type"], "object",
                "{}: input schema must be an object",
                t.name
            );
            let props = schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{}: schema must declare `properties`", t.name));
            let required = schema["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{}: schema must declare `required` (array)", t.name));
            for field in required {
                let field = field
                    .as_str()
                    .unwrap_or_else(|| panic!("{}: required entries must be strings", t.name));
                assert!(
                    props.contains_key(field),
                    "{}: required field `{field}` is not declared in properties",
                    t.name
                );
            }
        }
    }

    /// The agent surface (suggest-event, test-classify,
    /// test-coverage, dependency-graph) is registered and listed.
    #[tokio::test]
    async fn tools_list_includes_the_broadened_agent_surface() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}),
        )
        .await
        .expect("response");
        let list = resp["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = list.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "al_suggestevent",
            "al_testclassify",
            "al_testcoverage",
            "al_depgraph",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }

    #[tokio::test]
    async fn notifications_get_no_response() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert!(resp.is_none());
    }
}
