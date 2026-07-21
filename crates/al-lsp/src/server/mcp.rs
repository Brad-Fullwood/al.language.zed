//! MCP (Model Context Protocol) server mode — `al-lsp mcp`.
//!
//! Exposes the complete daemon tool surface to MCP-compatible agents
//! (Claude Code, Zed's agent panel via `context_servers`, custom agents)
//! over stdio, using newline-delimited JSON-RPC 2.0 per the MCP spec.
//!
//! `al_call` is the stable, zero-drift bridge to every daemon method. Named
//! convenience tools mirror Microsoft's AL agent tools (`al_build`,
//! `al_symbolsearch`, `al_getdiagnostics`, …) so agents trained on the
//! official surface transfer, plus this project's differentiators
//! (dead-code, SQL anti-patterns, event tracing, impact analysis) that the
//! official tooling does not offer. Arguments are forwarded VERBATIM as
//! daemon-dispatch params — validation happens in the dispatchers, which
//! already return structured JSON-RPC errors.

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
            name: "al_call",
            method: "",
            description: "Call any AL tool exposed by the shared daemon dispatcher. This is the \
                          complete, zero-drift MCP entry point used for methods that do not have a \
                          named convenience alias. Args: method (the daemon method name) and params \
                          (that method's parameter object, default {}). See the daemon method \
                          reference for the complete method catalog and parameter conventions.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "method": {
                            "type": "string",
                            "description": "Daemon method name, for example metrics, xlf.refresh, codeActions, or tests.affected."
                        },
                        "params": {
                            "type": "object",
                            "description": "Parameters accepted by the selected daemon method. Defaults to an empty object."
                        }
                    }),
                    &["method"],
                )
            },
        },
        ToolDef {
            name: "al_debug",
            method: "debug",
            description: "Control a persistent native Business Central debug session from an AI \
                          agent. Use cmd=start to attach using a named .zed/debug.json or \
                          .vscode/launch.json configuration; then breakpoint, state, stack, \
                          variables/globals/expand, eval, continue, step, history, and stop. The MCP process keeps the session alive between \
                          calls and routes commands through the same NativeDebugSession used by the \
                          CLI debug commands.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "cmd": {
                            "type": "string",
                            "enum": ["start", "breakpoint", "state", "stack", "variables", "globals", "expand", "eval", "continue", "step", "history", "stop"],
                            "description": "Debug operation. A typical agent loop is start, breakpoint, state, eval/step/continue, then stop."
                        },
                        "config": {
                            "type": "string",
                            "description": "For start: exact debug configuration name. When omitted, use the first AL configuration."
                        },
                        "accessToken": {
                            "type": "string",
                            "description": "For start: optional BC OAuth bearer token. When omitted for an OAuth target, al_debug acquires or refreshes the token through the shared keyring-backed authentication cache."
                        },
                        "server": {
                            "type": "string",
                            "description": "For an inline on-prem start: BC server URL. Omit when using a named config or BC online tenant."
                        },
                        "serverInstance": {
                            "type": "string",
                            "description": "For an inline on-prem start: BC server instance."
                        },
                        "port": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 65535,
                            "description": "For an inline on-prem start: service port (default 7049)."
                        },
                        "tenant": {
                            "type": "string",
                            "description": "For an inline start: BC tenant ID or domain. Supplying this selects inline configuration without requiring a server placeholder."
                        },
                        "environmentType": {
                            "type": "string",
                            "enum": ["Sandbox", "Production", "OnPrem"],
                            "description": "For an inline start: target environment type."
                        },
                        "environmentName": {
                            "type": "string",
                            "description": "For an inline BC online start: environment name, such as Sandbox."
                        },
                        "authentication": {
                            "type": "string",
                            "enum": ["AAD", "MicrosoftEntraID", "UserPassword", "Windows"],
                            "description": "For an inline start: authentication mode. BC online and AAD targets use the shared OAuth cache when accessToken is omitted."
                        },
                        "breakOnError": {
                            "type": ["boolean", "string"],
                            "description": "For start: configure breaking on AL errors."
                        },
                        "breakOnRecordWrite": {
                            "type": ["boolean", "string"],
                            "description": "For start: configure breaking before record writes."
                        },
                        "breakOnNext": {
                            "type": "string",
                            "description": "For start: attach to the next matching BC client session type, for example WebClient."
                        },
                        "sessionId": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "For start: attach to a specific existing BC session instead of breakOnNext."
                        },
                        "file": {
                            "type": "string",
                            "description": "For breakpoint: AL source path or file URI."
                        },
                        "line": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "For breakpoint: source line accepted by the BC debug service."
                        },
                        "condition": {
                            "type": "string",
                            "description": "For breakpoint: optional AL conditional expression."
                        },
                        "objectType": {
                            "type": "integer",
                            "description": "For breakpoint: optional BC object type when the file is not in the workspace index."
                        },
                        "objectId": {
                            "type": "integer",
                            "description": "For breakpoint: optional BC object ID when the file is not in the workspace index."
                        },
                        "expr": {
                            "type": "string",
                            "description": "For eval: AL watch expression evaluated in the paused frame."
                        },
                        "frameId": {
                            "type": "integer",
                            "description": "For variables, globals, expand, or eval: BC stack-frame ID; defaults to 0."
                        },
                        "path": {
                            "type": "string",
                            "description": "For expand: structured variable path to expand."
                        },
                        "stepType": {
                            "type": "string",
                            "enum": ["over", "in", "out"],
                            "description": "For step: step direction; defaults to over."
                        },
                        "var": {
                            "type": "string",
                            "description": "For history: optional case-insensitive variable-name filter."
                        }
                    }),
                    &["cmd"],
                )
            },
        },
        ToolDef {
            name: "al_build",
            method: "compile",
            description: "Compile and verify the AL project. By default this uses the pure-Rust \
                          native syntax/project/binding verifier and `.app` emitter with no alc or C# bridge; set \
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
            description: "Discover and run the project's AL tests. Pure-logic tests and \
                          supported workspace-record tests run on the built-in interpreter \
                          (no BC server needed); platform-dependent tests need a launch \
                          config + live BC.",
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
                          today. Pure-logic and supported workspace-record tests run locally \
                          on the built-in Rust interpreter (no Business Central server); UI, \
                          HTTP, transaction, package-table, and unsupported record behavior \
                          route to live BC. Each entry carries a routing `decision` \
                          (`interp`/`interpRecord` = runs locally; `liveBc`/`snapshot` = \
                          needs BC) plus the reasons \
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

            let (daemon_method, daemon_params) = if tool.name == "al_call" {
                let Some(method) = arguments.get("method").and_then(|v| v.as_str()) else {
                    return respond_err(
                        -32602,
                        "al_call requires a non-empty `method`".to_string(),
                    );
                };
                if method.trim().is_empty() {
                    return respond_err(
                        -32602,
                        "al_call requires a non-empty `method`".to_string(),
                    );
                }
                let method_params = arguments
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                (method, method_params)
            } else {
                (tool.method, arguments)
            };

            // Forward to the daemon dispatcher — same logic, different wire.
            let req = Request::new(0, daemon_method, Some(daemon_params));
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
    async fn tools_list_exposes_the_registry_and_complete_dispatch_bridge() {
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
            "al_call",
            "al_debug",
            "al_build",
            "al_symbolsearch",
            "al_getdiagnostics",
            "al_deadcode",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let debug = list
            .iter()
            .find(|tool| tool["name"] == "al_debug")
            .expect("al_debug definition");
        let commands = debug["inputSchema"]["properties"]["cmd"]["enum"]
            .as_array()
            .expect("al_debug cmd enum");
        for command in [
            "start",
            "breakpoint",
            "state",
            "stack",
            "variables",
            "globals",
            "expand",
            "eval",
            "continue",
            "step",
            "history",
            "stop",
        ] {
            assert!(
                commands.iter().any(|value| value == command),
                "al_debug schema missing {command}"
            );
        }
        let debug_properties = debug["inputSchema"]["properties"]
            .as_object()
            .expect("al_debug properties");
        for property in [
            "tenant",
            "environmentName",
            "server",
            "accessToken",
            "breakOnNext",
            "sessionId",
            "frameId",
            "path",
        ] {
            assert!(
                debug_properties.contains_key(property),
                "al_debug schema missing {property}"
            );
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

    /// Methods do not need a hand-written MCP alias to be available. This
    /// protects the architectural promise that MCP, CLI and Zed are entry
    /// points to the same dispatcher rather than separate feature sets.
    #[tokio::test]
    async fn al_call_reaches_methods_without_named_aliases() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0","id":5,"method":"tools/call",
                "params": {
                    "name": "al_call",
                    "arguments": {"method": "rules", "params": {}}
                }
            }),
        )
        .await
        .expect("response");
        assert_eq!(resp["result"]["isError"], false, "resp: {resp}");
    }

    #[tokio::test]
    async fn al_call_requires_a_method() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0","id":6,"method":"tools/call",
                "params": {"name": "al_call", "arguments": {}}
            }),
        )
        .await
        .expect("response");
        assert_eq!(resp["error"]["code"], -32602, "resp: {resp}");
    }

    /// `al_debug` must reach the stateful shared debug dispatcher rather than
    /// merely appearing in `tools/list`. `stop` is intentionally safe without
    /// a live BC connection and proves the whole MCP call path.
    #[tokio::test]
    async fn al_debug_reaches_the_shared_debug_dispatcher() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0","id":7,"method":"tools/call",
                "params": {
                    "name": "al_debug",
                    "arguments": {"cmd": "stop"}
                }
            }),
        )
        .await
        .expect("response");
        assert_eq!(resp["result"]["isError"], false, "resp: {resp}");
        let payload: serde_json::Value = serde_json::from_str(
            resp["result"]["content"][0]["text"]
                .as_str()
                .expect("text result"),
        )
        .expect("debug result JSON");
        assert_eq!(payload["cmd"], "stop");
        assert_eq!(payload["status"], "no active debug session");
    }

    /// Every registered tool must advertise a well-formed JSON Schema and a
    /// non-empty description, and every `required` field must actually be
    /// declared in `properties` (otherwise an agent cannot satisfy it).
    #[test]
    fn every_tool_has_a_valid_schema_and_description() {
        for t in tools() {
            assert!(!t.name.trim().is_empty(), "tool name must be non-empty");
            assert!(
                t.name == "al_call" || !t.method.trim().is_empty(),
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
