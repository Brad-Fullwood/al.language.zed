//! MCP (Model Context Protocol) server mode — `al-lsp mcp`.
//!
//! Exposes the complete daemon tool surface to MCP-compatible agents
//! (Claude Code, Zed's agent panel via `context_servers`, custom agents)
//! over stdio, using newline-delimited JSON-RPC 2.0 per the MCP spec.
//!
//! `al_call` forwards requests to any daemon method. Named
//! convenience tools mirror Microsoft's AL agent tools (`al_build`,
//! `al_symbolsearch`, `al_getdiagnostics`, …) so agents trained on the
//! official surface transfer, plus this project's differentiators
//! (dead-code, SQL anti-patterns, event tracing, impact analysis) that the
//! official tooling does not offer. Arguments are forwarded as
//! daemon-dispatch parameters; validation happens in the dispatchers, which
//! already return structured JSON-RPC errors.

use std::path::PathBuf;
use std::sync::Arc;

use al_protocol::jsonrpc::Request;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use al_workspace::Workspace;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const LEGACY_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum McpLifecycle {
    #[default]
    Uninitialized,
    AwaitingInitializedNotification,
    Ready,
}

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
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

fn object_result_schema(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true,
    })
}

fn array_result_schema(item: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": item,
    })
}

fn result_schema(tool_name: &str) -> serde_json::Value {
    let object_array = || array_result_schema(serde_json::json!({"type": "object"}));
    match tool_name {
        // `al_call` deliberately has no narrower result schema: it forwards
        // the complete daemon catalog, whose methods return heterogeneous JSON.
        "al_call" => serde_json::json!({}),
        "al_debug" => object_result_schema(
            serde_json::json!({
                "cmd": {"type": "string"},
                "status": {"type": "string"},
            }),
            &["cmd"],
        ),
        "al_build" => object_result_schema(
            serde_json::json!({
                "success": {"type": "boolean"},
                "diagnostics": {"type": "array", "items": {"type": "object"}},
                "appPath": {"type": ["string", "null"]},
                "output": {"type": "string"},
                "backend": {"type": "string"},
                "validated": {"type": "boolean"},
                "verificationLevel": {"type": "string"},
            }),
            &["success", "diagnostics", "appPath", "output"],
        ),
        "al_downloadsymbols" => object_result_schema(
            serde_json::json!({
                "source": {"type": "string"},
                "downloaded": {"type": "integer"},
                "failed": {"type": "integer"},
                "skipped": {"type": "integer"},
                "loaded_into_index": {"type": "integer"},
                "results": {"type": "array", "items": {"type": "object"}},
            }),
            &["source", "downloaded", "failed", "results"],
        ),
        "al_symbolsearch" | "al_getdiagnostics" | "al_deadcode" | "al_sqlscan"
        | "al_entrypoints" | "al_trace_event" => object_array(),
        "al_runtests" => object_result_schema(
            serde_json::json!({
                "summaries": {"type": "array", "items": {"type": "object"}},
                "routing": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "codeunitId": {"type": "integer"},
                            "codeunitName": {"type": "string"},
                            "methodName": {"type": ["string", "null"]},
                            "classifiedDecision": {"type": ["string", "null"]},
                            "decision": {"type": "string", "enum": ["interp", "interpRecord", "liveBc"]},
                            "runsLocally": {"type": "boolean"},
                            "execution": {"type": "string"},
                            "reasons": {"type": "array", "items": {"type": "object"}},
                        },
                        "required": ["codeunitId", "codeunitName", "methodName", "decision", "runsLocally", "execution", "reasons"],
                    }
                },
                "totals": {
                    "type": "object",
                    "properties": {
                        "total": {"type": "integer"},
                        "passed": {"type": "integer"},
                        "failed": {"type": "integer"},
                        "skipped": {"type": "integer"},
                    },
                    "required": ["total", "passed", "failed", "skipped"],
                },
                "coverage": {"type": "object"},
            }),
            &["summaries", "routing", "totals"],
        ),
        "al_impact" => object_result_schema(
            serde_json::json!({
                "symbol": {"type": "string"},
                "impacted": {"type": "array", "items": {"type": "object"}},
            }),
            &["symbol", "impacted"],
        ),
        "al_suggestevent" => object_result_schema(
            serde_json::json!({
                "integrationPoints": {"type": "array", "items": {"type": "object"}},
                "partial": {"type": "boolean"},
            }),
            &["integrationPoints", "partial"],
        ),
        "al_testclassify" => object_result_schema(
            serde_json::json!({
                "classifications": {"type": "array", "items": {"type": "object"}},
            }),
            &["classifications"],
        ),
        "al_testcoverage" => object_result_schema(
            serde_json::json!({
                "coverage": {"type": "array", "items": {"type": "object"}},
                "untested": {"type": "array", "items": {"type": "object"}},
            }),
            &["coverage", "untested"],
        ),
        "al_testsnapshot" => object_result_schema(
            serde_json::json!({
                "captured": {"type": "boolean"},
                "snapshotPath": {"type": "string"},
                "sampleCount": {"type": "integer"},
                "runId": {"type": "string"},
                "codeunitId": {"type": "integer"},
                "methodName": {"type": "string"},
                "bcVersion": {"type": "string"},
                "sourceHash": {"type": "string"},
                "testResult": {"type": "object"},
            }),
            &[
                "captured",
                "snapshotPath",
                "sampleCount",
                "runId",
                "codeunitId",
                "methodName",
                "bcVersion",
                "sourceHash",
                "testResult",
            ],
        ),
        "al_testsnapshotreplay" => object_result_schema(
            serde_json::json!({
                "replayed": {"type": "boolean"},
                "matched": {"type": "boolean"},
                "divergences": {"type": "array", "items": {"type": "object"}},
                "baseline": {"type": "object"},
                "observed": {"type": "object"},
            }),
            &["replayed", "matched", "divergences", "baseline", "observed"],
        ),
        "al_depgraph" => serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "rootApp": {"type": "object"},
                        "nodes": {"type": "array"},
                        "edges": {"type": "array"},
                        "transitive": {"type": "array"},
                        "conflicts": {"type": "array"},
                        "missing": {"type": "array"},
                    },
                    "required": ["rootApp", "nodes", "edges", "transitive", "conflicts", "missing"],
                },
                {
                    "type": "object",
                    "properties": {
                        "format": {"const": "dot"},
                        "content": {"type": "string"},
                    },
                    "required": ["format", "content"],
                }
            ]
        }),
        _ => serde_json::json!({}),
    }
}

fn agent_diagnostic_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "code": {"type": "string"},
            "severity": {"type": "string", "enum": ["information", "warning", "error"]},
            "summary": {"type": "string"},
            "reason": {"type": "string"},
            "actions": {"type": "array", "items": {"type": "string"}},
        },
        "required": ["code", "severity", "summary", "reason", "actions"],
        "additionalProperties": false,
    })
}

fn output_schema(tool_name: &str) -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "success": {"type": "boolean"},
            "tool": {"type": "string"},
            "method": {"type": "string"},
            "result": result_schema(tool_name),
            "error": {
                "type": "string",
                "description": "Daemon error message when success is false."
            },
            "diagnostics": {
                "type": "array",
                "items": agent_diagnostic_schema(),
                "description": "Agent-oriented explanations and concrete recovery actions for incomplete or blocked results."
            },
            "routing": {
                "type": "array",
                "items": {"type": "object"},
                "description": "Per-test routing context retained when al_runtests is blocked before execution."
            },
        },
        "required": ["success", "tool", "method"],
        "additionalProperties": false,
        "oneOf": [
            {"required": ["result"], "not": {"required": ["error"]}},
            {"required": ["error"], "not": {"required": ["result"]}}
        ]
    })
}

fn value_matches_type(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn validate_schema_value(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    if let Some(alternatives) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
        let errors = alternatives
            .iter()
            .map(|alternative| {
                validate_schema_value(value, alternative, path)
                    .err()
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let matches = errors.iter().filter(|error| error.is_empty()).count();
        if matches != 1 {
            let details = errors
                .into_iter()
                .filter(|error| !error.is_empty())
                .collect::<Vec<_>>()
                .join(" or ");
            return Err(format!(
                "{path} must match exactly one supported shape: {details}"
            ));
        }
    }

    if let Some(expected) = schema.get("type") {
        let matches = match expected {
            serde_json::Value::String(expected) => value_matches_type(value, expected),
            serde_json::Value::Array(expected) => expected
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|expected| value_matches_type(value, expected)),
            _ => false,
        };
        if !matches {
            return Err(format!(
                "{path} has the wrong JSON type; expected {}",
                expected
            ));
        }
    }

    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        if !values.contains(value) {
            return Err(format!(
                "{path} must be one of {}",
                serde_json::Value::Array(values.clone())
            ));
        }
    }

    if value.is_number() {
        if let Some(minimum) = schema.get("minimum").and_then(serde_json::Value::as_f64) {
            if value.as_f64().is_some_and(|number| number < minimum) {
                return Err(format!("{path} must be at least {minimum}"));
            }
        }
        if let Some(maximum) = schema.get("maximum").and_then(serde_json::Value::as_f64) {
            if value.as_f64().is_some_and(|number| number > maximum) {
                return Err(format!("{path} must be at most {maximum}"));
            }
        }
    }

    if let Some(object) = value.as_object() {
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object);
        if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
            for required in required.iter().filter_map(serde_json::Value::as_str) {
                if !object.contains_key(required) {
                    return Err(format!("{path}.{required} is required"));
                }
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate_schema_value(child, child_schema, &format!("{path}.{key}"))?;
                } else if schema.get("additionalProperties")
                    == Some(&serde_json::Value::Bool(false))
                {
                    return Err(format!("{path}.{key} is not a supported argument"));
                }
            }
        }
    }
    if let Some(array) = value.as_array() {
        // `minItems` is part of the published input schema (e.g.
        // `al_testsnapshot.breakpoints`), so an empty array must be rejected
        // here rather than reaching the daemon dispatcher.
        if let Some(minimum) = schema.get("minItems").and_then(serde_json::Value::as_u64) {
            if (array.len() as u64) < minimum {
                return Err(format!(
                    "{path} must contain at least {minimum} item{}",
                    if minimum == 1 { "" } else { "s" }
                ));
            }
        }
        if let Some(maximum) = schema.get("maxItems").and_then(serde_json::Value::as_u64) {
            if (array.len() as u64) > maximum {
                return Err(format!("{path} must contain at most {maximum} items"));
            }
        }
        if let Some(item_schema) = schema.get("items").filter(|schema| schema.is_object()) {
            for (index, item) in array.iter().enumerate() {
                validate_schema_value(item, item_schema, &format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn validate_tool_arguments(tool: &ToolDef, arguments: &serde_json::Value) -> Result<(), String> {
    validate_schema_value(arguments, &(tool.schema)(), "arguments")
}

fn tools() -> &'static [ToolDef] {
    &[
        ToolDef {
            name: "al_call",
            method: "",
            description: "Call any AL tool exposed by the shared daemon dispatcher. This entry \
                          point supports methods that do not have a \
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
                            "enum": ["AAD", "MicrosoftEntraID"],
                            "description": "For an inline start: native debugging supports OAuth bearer authentication. The shared OAuth cache is used when accessToken is omitted."
                        },
                        "breakOnError": {
                            "oneOf": [
                                {"type": "boolean"},
                                {"type": "string", "enum": ["False", "True", "None", "All", "ExcludeTry"]}
                            ],
                            "description": "For start: configure breaking on AL errors."
                        },
                        "breakOnRecordWrite": {
                            "oneOf": [
                                {"type": "boolean"},
                                {"type": "string", "enum": ["False", "True", "None", "All", "ExcludeTemporary"]}
                            ],
                            "description": "For start: configure breaking before record writes."
                        },
                        "breakOnNext": {
                            "type": "string",
                            "enum": ["WebServiceClient", "WebClient", "Background", "ClientService", "Agent"],
                            "description": "For start: attach to the next matching BC client session type, for example WebClient."
                        },
                        "sessionId": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "For start: attach to a specific existing BC session instead of breakOnNext."
                        },
                        "startupObjectType": {
                            "type": "string",
                            "enum": ["Page", "Table", "Report", "Query"],
                            "description": "For an inline start: startup object type used in the returned debug browser URL; defaults to Page."
                        },
                        "startupObjectId": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "For an inline start: startup object ID used in the returned debug browser URL; defaults to 22."
                        },
                        "startupCompany": {
                            "type": "string",
                            "description": "For an inline start: company opened in the returned debug browser URL."
                        },
                        "launchBrowser": {
                            "type": "boolean",
                            "description": "For an inline start: whether browser launch is requested."
                        },
                        "schemaUpdateMode": {
                            "type": "string",
                            "enum": ["Synchronize", "Recreate", "ForceSync"]
                        },
                        "dependencyPublishingOption": {
                            "type": "string",
                            "enum": ["Default", "Ignore", "Strict"]
                        },
                        "enableSqlInformationDebugger": {
                            "type": "boolean"
                        },
                        "enableLongRunningSqlStatements": {
                            "type": "boolean"
                        },
                        "longRunningSqlStatementsThreshold": {
                            "type": "integer",
                            "minimum": 0
                        },
                        "numberOfSqlStatements": {
                            "type": "integer",
                            "minimum": 0
                        },
                        "validateServerCertificate": {
                            "type": "boolean"
                        },
                        "file": {
                            "type": "string",
                            "description": "For breakpoint: AL source path or file URI."
                        },
                        "line": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "For breakpoint: one-based AL source line."
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
                          source. Args: query (string), limit (integer, default 20, maximum 500000).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "query": {"type": "string"},
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 500000,
                            "default": 20
                        }
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
                          Args: event (string), depth (integer, default 10, maximum 50).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "event": {"type": "string"},
                        "depth": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 50,
                            "default": 10
                        }
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
            name: "al_testsnapshot",
            method: "tests.snapshot_capture",
            description: "Capture breakpoint-sampled variables while one exact [Test] method \
                          runs on live Business Central. This is a live mutation and requires \
                          a launch configuration; snapshot validation and file-to-file diff \
                          remain separate daemon methods available through al_call.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "codeunitId": {"type": "integer"},
                        "codeunitName": {"type": "string"},
                        "methodName": {"type": "string"},
                        "bcVersion": {"type": "string"},
                        "breakpoints": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": {"type": "string"},
                                    "line": {"type": "integer", "minimum": 1},
                                    "condition": {"type": "string"}
                                },
                                "required": ["file", "line"],
                                "additionalProperties": false
                            }
                        },
                        "outputPath": {"type": "string"},
                        "config": {"type": "string"},
                        "timeoutMs": {"type": "integer", "minimum": 1}
                    }),
                    &[
                        "codeunitId",
                        "codeunitName",
                        "methodName",
                        "bcVersion",
                        "breakpoints",
                        "outputPath",
                    ],
                )
            },
        },
        ToolDef {
            name: "al_testsnapshotreplay",
            method: "tests.snapshot_replay",
            description: "Re-run the exact test recorded by a baseline snapshot on live \
                          Business Central, recapture the same source breakpoints, and return \
                          field-level divergences. The baseline must be inside the project and \
                          the current BC runtime version is required explicitly.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "snapshotPath": {"type": "string"},
                        "bcVersion": {"type": "string"},
                        "config": {"type": "string"},
                        "timeoutMs": {"type": "integer", "minimum": 1}
                    }),
                    &["snapshotPath", "bcVersion"],
                )
            },
        },
        ToolDef {
            name: "al_depgraph",
            method: "deps.graph",
            description: "Build the GUID-keyed project dependency graph from the current typed \
                          app.json and every loaded .app manifest, including implicit BC \
                          dependencies, transitive edges, unsatisfied minimum versions, duplicate \
                          loaded versions, and missing packages. Args: format (string) — 'json' \
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

fn agent_diagnostic(
    code: &str,
    severity: &str,
    summary: &str,
    reason: impl Into<String>,
    actions: &[&str],
) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "severity": severity,
        "summary": summary,
        "reason": reason.into(),
        "actions": actions,
    })
}

fn result_is_empty(result: &serde_json::Value) -> bool {
    result.is_null()
        || result.as_array().is_some_and(Vec::is_empty)
        || result.as_object().is_some_and(serde_json::Map::is_empty)
}

async fn agent_diagnostics(
    workspace: &Workspace,
    method: &str,
    result: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut diagnostics = Vec::new();

    let missing_bc_config = error.is_some_and(|message| {
        message.contains("No launch config found")
            || message.contains("No BC server config found")
            || message.contains("No tenant found")
            || message.contains("No debug configuration found")
            || message.contains("debug configuration file has no configs")
    });
    if missing_bc_config {
        diagnostics.push(agent_diagnostic(
            "AL_AGENT_MISSING_BC_CONFIGURATION",
            "error",
            "Business Central configuration is required for this operation",
            error.unwrap_or_default(),
            &[
                "Create .zed/debug.json or .vscode/launch.json with an AL configuration.",
                "Choose a configuration containing the target tenant/environment or on-premises server.",
                "For tests, call al_testclassify first to see which methods can run locally.",
            ],
        ));
    }

    let has_declared_dependencies = workspace
        .project
        .read()
        .await
        .as_ref()
        .is_some_and(|project| !project.all_dependencies().is_empty());
    let missing_symbol_result = match method {
        "search" => result.is_some_and(result_is_empty),
        "source" | "location" => error
            .is_some_and(|message| message.contains("not found in workspace or symbol packages")),
        _ => false,
    };
    if missing_symbol_result && has_declared_dependencies && workspace.symbols.is_empty() {
        diagnostics.push(agent_diagnostic(
            "AL_AGENT_MISSING_SYMBOLS",
            "warning",
            "Declared package symbols are not loaded",
            "The project declares Business Central or extension dependencies, but the package symbol index is empty; package objects and members cannot be resolved.",
            &[
                "Call al_downloadsymbols, then retry the query.",
                "Check packageCachePath/appLocalFolderPaths when symbols already exist on disk.",
                "Inspect the download result's failed entries before treating an empty search as authoritative.",
            ],
        ));
    }

    if matches!(method, "lint" | "hover" | "completions" | "signatureHelp") {
        let bridge_enabled = workspace.config.read().await.enable_code_analysis;
        let bridge_available =
            bridge_enabled && al_workspace::get_or_init_bridge(workspace).await.is_some();
        if !bridge_available {
            let reason = if !bridge_enabled {
                "The optional Microsoft CodeAnalysis semantic bridge is disabled by al.enableCodeAnalysis. Native syntax and workspace diagnostics still run, but bridge-only semantic enrichment is absent."
            } else if workspace.toolchain.read().await.is_none() {
                "The Microsoft AL toolchain was not discovered, so the optional semantic bridge cannot initialize. Native syntax and workspace diagnostics still run, but Microsoft CodeAnalysis enrichment is absent."
            } else {
                "The optional Microsoft CodeAnalysis semantic bridge could not initialize or exhausted its bounded restart attempts. Native results remain available but may lack bridge-only semantic detail."
            };
            diagnostics.push(agent_diagnostic(
                "AL_AGENT_SEMANTIC_BRIDGE_UNAVAILABLE",
                "warning",
                "Semantic bridge enrichment is unavailable",
                reason,
                &[
                    "Enable al.enableCodeAnalysis when bridge enrichment is desired.",
                    "Install or configure a compatible Microsoft AL toolchain.",
                    "Check AL semantic bridge initialization logs for the first failure.",
                ],
            ));
        }
    }

    if matches!(method, "source" | "location") {
        let availability = result
            .and_then(|value| value.get("source_availability"))
            .and_then(serde_json::Value::as_str);
        if matches!(availability, Some("metadata_only" | "generated_outline")) {
            let (summary, reason) = if availability == Some("metadata_only") {
                (
                    "Package navigation reached metadata only",
                    "The package exposes object identity but no extractable AL source or rich public API metadata; the returned declaration is a stable navigation target, not the implementation.",
                )
            } else {
                (
                    "Package navigation returned a generated outline",
                    "Original AL source was unavailable. The result was reconstructed from SymbolReference.json and contains public signatures/fields but no implementation bodies.",
                )
            };
            diagnostics.push(agent_diagnostic(
                "AL_AGENT_PACKAGE_SOURCE_UNAVAILABLE",
                "warning",
                summary,
                reason,
                &[
                    "Use source_availability before relying on implementation details.",
                    "Install a package containing embedded source when implementation navigation is required.",
                    "Treat generated outlines as public API metadata, not executable source.",
                ],
            ));
        } else if error.is_some_and(|message| {
            message.contains("source could not be materialised")
                || message.contains(" is unavailable:")
        }) {
            diagnostics.push(agent_diagnostic(
                "AL_AGENT_PACKAGE_SOURCE_UNAVAILABLE",
                "error",
                "Package source could not be opened",
                error.unwrap_or_default(),
                &[
                    "Inspect the package path and verify the .app is readable.",
                    "Download or replace the package, then retry navigation.",
                    "Use symbol search/API metadata when implementation source is not shipped.",
                ],
            ));
        }
    }

    diagnostics
}

/// Handle one parsed MCP message. Returns the response to write, or `None`
/// for notifications (which get no response).
pub(crate) async fn handle_mcp_message(
    workspace: &Arc<Workspace>,
    shutdown: &Notify,
    msg: serde_json::Value,
) -> Option<serde_json::Value> {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // Only a *missing* id makes the message a notification. `"id": null` is a
    // legal (if discouraged) request id, and the session layer already treats it
    // as one — answering it here keeps both paths consistent instead of leaving
    // the client waiting forever for a reply that never comes.
    let Some(id) = msg.get("id").cloned() else {
        tracing::debug!(method, "mcp: notification");
        return None;
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
        "initialize" => {
            let requested = msg
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str);
            let negotiated = match requested {
                Some(MCP_PROTOCOL_VERSION) => MCP_PROTOCOL_VERSION,
                Some(LEGACY_MCP_PROTOCOL_VERSION) => LEGACY_MCP_PROTOCOL_VERSION,
                _ => MCP_PROTOCOL_VERSION,
            };
            respond(serde_json::json!({
                "protocolVersion": negotiated,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "al-lsp",
                    "title": "AL Language Tools",
                    "version": env!("CARGO_PKG_VERSION"),
                    "description": "Business Central AL analysis, build, test, and debug tools."
                },
                "instructions": "Use the named tools for validated common operations. Use al_call only for daemon methods without a named tool."
            }))
        }
        "ping" => respond(serde_json::json!({})),
        "tools/list" => {
            let list: Vec<serde_json::Value> = tools()
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": (t.schema)(),
                        "outputSchema": output_schema(t.name),
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
            if let Err(error) = validate_tool_arguments(tool, &arguments) {
                return respond_err(
                    -32602,
                    format!("Invalid arguments for {tool_name}: {error}"),
                );
            }

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
            let error_message = resp.error.as_ref().map(|error| error.message.as_str());
            let diagnostics = agent_diagnostics(
                workspace,
                daemon_method,
                resp.result.as_ref(),
                error_message,
            )
            .await;
            let blocked_routing = if tool.name == "al_runtests"
                && error_message.is_some_and(|message| {
                    message.contains("No launch config found")
                        || message.contains("No BC server config found")
                }) {
                let classify = super::daemon::dispatch_request(
                    workspace,
                    Request::new(0, "tests.classify", Some(serde_json::json!({}))),
                    shutdown,
                )
                .await;
                classify
                    .result
                    .and_then(|result| result.get("classifications").cloned())
            } else {
                None
            };

            let (text, is_error, mut structured_content) = match (resp.result, resp.error) {
                (_, Some(err)) => {
                    let message = err.message;
                    (
                        message.clone(),
                        true,
                        serde_json::json!({
                            "success": false,
                            "tool": tool.name,
                            "method": daemon_method,
                            "error": message,
                        }),
                    )
                }
                (result, None) => {
                    let result = result.unwrap_or(serde_json::Value::Null);
                    (
                        serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string()),
                        false,
                        serde_json::json!({
                            "success": true,
                            "tool": tool.name,
                            "method": daemon_method,
                            "result": result,
                        }),
                    )
                }
            };
            if !diagnostics.is_empty() {
                structured_content["diagnostics"] = serde_json::Value::Array(diagnostics);
            }
            if let Some(routing) = blocked_routing {
                structured_content["routing"] = routing;
            }
            respond(serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": structured_content,
                "isError": is_error,
            }))
        }
        other => respond_err(-32601, format!("Method not found: {other}")),
    }
}

/// Enforce the MCP connection lifecycle around the stateless request
/// dispatcher. Keeping this state at the stdio-session boundary ensures the
/// first interaction is capability negotiation and normal operations cannot
/// start until the client sends `notifications/initialized`.
async fn handle_mcp_session_message(
    workspace: &Arc<Workspace>,
    shutdown: &Notify,
    lifecycle: &mut McpLifecycle,
    msg: serde_json::Value,
) -> Option<serde_json::Value> {
    let id = msg
        .get("id")
        .filter(|id| !id.is_null())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let error = |code: i64, message: &str| {
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": {"code": code, "message": message}
        }))
    };

    let Some(object) = msg.as_object() else {
        return error(-32600, "Invalid Request: MCP messages must be JSON objects");
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return error(-32600, "Invalid Request: jsonrpc must be \"2.0\"");
    }
    let Some(method) = object.get("method").and_then(serde_json::Value::as_str) else {
        return error(-32600, "Invalid Request: method must be a string");
    };

    if object.get("id").is_none() {
        if method == "notifications/initialized" {
            if *lifecycle == McpLifecycle::AwaitingInitializedNotification {
                *lifecycle = McpLifecycle::Ready;
            } else {
                tracing::warn!(
                    state = ?lifecycle,
                    "mcp: ignored initialized notification in the wrong lifecycle phase"
                );
            }
        }
        // Notifications never receive JSON-RPC responses. Other notifications
        // (including cancellation) are currently informational for this
        // sequential stdio dispatcher.
        return None;
    }

    if method == "ping" {
        return handle_mcp_message(workspace, shutdown, msg).await;
    }

    if method == "initialize" {
        if *lifecycle != McpLifecycle::Uninitialized {
            return error(
                -32600,
                "Invalid Request: MCP session is already initialized",
            );
        }
        let params = object.get("params");
        let valid_params = params
            .and_then(serde_json::Value::as_object)
            .is_some_and(|params| {
                params
                    .get("protocolVersion")
                    .is_some_and(serde_json::Value::is_string)
                    && params
                        .get("capabilities")
                        .is_some_and(serde_json::Value::is_object)
                    && params
                        .get("clientInfo")
                        .is_some_and(serde_json::Value::is_object)
            });
        if !valid_params {
            return error(
                -32602,
                "Invalid initialize params: protocolVersion, capabilities, and clientInfo are required",
            );
        }
        let response = handle_mcp_message(workspace, shutdown, msg).await;
        if response
            .as_ref()
            .is_some_and(|response| response.get("result").is_some())
        {
            *lifecycle = McpLifecycle::AwaitingInitializedNotification;
        }
        return response;
    }

    if *lifecycle != McpLifecycle::Ready {
        return error(
            -32002,
            "MCP session is not initialized; send initialize then notifications/initialized",
        );
    }

    handle_mcp_message(workspace, shutdown, msg).await
}

/// Hard cap on one MCP stdio line, mirroring the daemon transport.
const MAX_MCP_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// Stable key for an in-flight request id, so `notifications/cancelled` can
/// match string and numeric ids alike.
fn cancellation_key(id: &serde_json::Value) -> String {
    id.to_string()
}

/// Whether a message is a well-formed `tools/call` request — the only method
/// that can run long enough to justify concurrent dispatch.
fn is_long_running_call(msg: &serde_json::Value) -> bool {
    msg.get("jsonrpc").and_then(serde_json::Value::as_str) == Some("2.0")
        && msg.get("method").and_then(serde_json::Value::as_str) == Some("tools/call")
        && msg.get("id").is_some()
}

async fn write_mcp_frame(
    stdout: &Arc<tokio::sync::Mutex<tokio::io::Stdout>>,
    frame: &serde_json::Value,
) -> std::io::Result<()> {
    let mut guard = stdout.lock().await;
    guard.write_all(frame.to_string().as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await
}

/// Run the MCP server on stdio: newline-delimited JSON-RPC 2.0.
pub async fn run_mcp(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Arc::new(Workspace::new());
    *workspace.config.write().await = al_project::config::AlConfig::load_effective(&project_root)?;
    let _ = workspace.notify_sink.set(Arc::new(|msg: &str| {
        tracing::warn!("mcp: {msg}");
    }));
    super::daemon::initialize_daemon_workspace(&workspace, &project_root).await?;
    tracing::info!(project = %project_root.display(), tools = tools().len(), "MCP server ready");

    // Never triggered in MCP mode — exists because the shared dispatcher's
    // "shutdown" route signals it (an agent calling it just ends our loop
    // via stdin EOF anyway).
    let shutdown = Arc::new(Notify::new());

    let mut reader = BufReader::new(tokio::io::stdin());
    // stdout is shared with the concurrently dispatched `tools/call` tasks, so
    // every frame is written under one lock — MCP frames must not interleave.
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let mut lifecycle = McpLifecycle::default();
    let mut in_flight: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
        std::collections::HashMap::new();
    loop {
        // Bounded read: an unbounded `read_line` lets one huge client line
        // allocate without limit before it is even parsed. Same 64 MB cap the
        // daemon transport enforces.
        let Some(line) =
            super::daemon::read_bounded_line(&mut reader, MAX_MCP_MESSAGE_SIZE).await?
        else {
            tracing::info!("mcp: stdin closed, exiting");
            return Ok(());
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "mcp: malformed JSON line");
                write_mcp_frame(
                    &stdout,
                    &serde_json::json!({
                        "jsonrpc": "2.0", "id": null,
                        "error": {"code": -32700, "message": format!("Parse error: {e}")}
                    }),
                )
                .await?;
                continue;
            }
        };

        in_flight.retain(|_, handle| !handle.is_finished());

        // `notifications/cancelled` aborts the matching in-flight tool call.
        if msg.get("method").and_then(serde_json::Value::as_str) == Some("notifications/cancelled")
        {
            if let Some(request_id) = msg.pointer("/params/requestId") {
                match in_flight.remove(&cancellation_key(request_id)) {
                    Some(handle) => {
                        handle.abort();
                        tracing::info!(request = %request_id, "mcp: cancelled in-flight tool call");
                    }
                    None => {
                        tracing::debug!(request = %request_id, "mcp: cancellation for an unknown or finished request");
                    }
                }
            }
            continue;
        }

        // A `tools/call` can run for minutes (live-BC test snapshot, build).
        // Dispatch it concurrently so cheap lifecycle traffic — `ping` above
        // all — keeps being answered while it runs, instead of the client
        // concluding the server is dead.
        if lifecycle == McpLifecycle::Ready && is_long_running_call(&msg) {
            let key = msg
                .get("id")
                .map(cancellation_key)
                .unwrap_or_else(|| "null".to_string());
            let task_workspace = Arc::clone(&workspace);
            let task_shutdown = Arc::clone(&shutdown);
            let task_stdout = Arc::clone(&stdout);
            let handle = tokio::spawn(async move {
                if let Some(response) =
                    handle_mcp_message(&task_workspace, &task_shutdown, msg).await
                {
                    if let Err(error) = write_mcp_frame(&task_stdout, &response).await {
                        tracing::warn!(%error, "mcp: failed to write tools/call response");
                    }
                }
            });
            in_flight.insert(key, handle);
            continue;
        }

        if let Some(resp) =
            handle_mcp_session_message(&workspace, &shutdown, &mut lifecycle, msg).await
        {
            write_mcp_frame(&stdout, &resp).await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Arc<Workspace> {
        Arc::new(Workspace::new())
    }

    async fn install_project(ws: &Workspace, root: &std::path::Path, application: Option<&str>) {
        *ws.project.write().await = Some(al_project::project::AlProject {
            root: root.to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: "00000000-0000-0000-0000-000000000001".to_string(),
                name: "MCP Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: application.map(str::to_string),
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        });
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
        assert!(resp["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains("named tools")));
    }

    #[tokio::test]
    async fn initialize_honors_the_supported_legacy_protocol_version() {
        let resp = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{"protocolVersion": LEGACY_MCP_PROTOCOL_VERSION}
            }),
        )
        .await
        .expect("response");
        assert_eq!(
            resp["result"]["protocolVersion"],
            LEGACY_MCP_PROTOCOL_VERSION
        );
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
            assert_eq!(t["outputSchema"]["type"], "object");
        }
        let runtests = resp["result"]["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == "al_runtests"))
            .expect("al_runtests definition");
        assert!(
            runtests["outputSchema"]["properties"]["result"]["properties"]["routing"].is_object(),
            "al_runtests must advertise its per-test routing result: {runtests}"
        );
        let search = resp["result"]["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == "al_symbolsearch"))
            .expect("al_symbolsearch definition");
        assert_eq!(
            search["outputSchema"]["properties"]["result"]["type"],
            "array"
        );
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
        assert_eq!(resp["result"]["structuredContent"]["success"], true);
        assert_eq!(
            resp["result"]["structuredContent"]["tool"],
            "al_symbolsearch"
        );
        assert!(
            resp["result"]["structuredContent"]["result"].is_array(),
            "structured result must preserve the daemon JSON: {resp}"
        );
        validate_schema_value(
            &resp["result"]["structuredContent"]["result"],
            &result_schema("al_symbolsearch"),
            "result",
        )
        .expect("symbol search output must match its advertised result schema");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(text.contains("Hello World"), "search must find it: {text}");
    }

    #[tokio::test]
    async fn agent_diagnostics_explain_all_four_actionable_environment_gaps() {
        let workspace = ws();
        let temp = tempfile::tempdir().expect("temp project");
        install_project(&workspace, temp.path(), Some("26.0.0.0")).await;

        let missing_symbols =
            agent_diagnostics(&workspace, "search", Some(&serde_json::json!([])), None).await;
        assert_eq!(missing_symbols[0]["code"], "AL_AGENT_MISSING_SYMBOLS");
        assert!(missing_symbols[0]["actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty()));

        let missing_bc = agent_diagnostics(
            &workspace,
            "tests.run_auto",
            None,
            Some("No launch config found — create .vscode/launch.json or .zed/debug.json"),
        )
        .await;
        assert_eq!(missing_bc[0]["code"], "AL_AGENT_MISSING_BC_CONFIGURATION");

        let missing_bridge =
            agent_diagnostics(&workspace, "lint", Some(&serde_json::json!([])), None).await;
        assert!(missing_bridge
            .iter()
            .any(|diagnostic| diagnostic["code"] == "AL_AGENT_SEMANTIC_BRIDGE_UNAVAILABLE"));

        let unavailable_source = agent_diagnostics(
            &workspace,
            "source",
            Some(&serde_json::json!({"source_availability": "metadata_only"})),
            None,
        )
        .await;
        assert!(unavailable_source
            .iter()
            .any(|diagnostic| diagnostic["code"] == "AL_AGENT_PACKAGE_SOURCE_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn symbolsearch_surfaces_missing_symbol_recovery_in_structured_content() {
        let workspace = ws();
        let temp = tempfile::tempdir().expect("temp project");
        install_project(&workspace, temp.path(), Some("26.0.0.0")).await;
        let response = handle_mcp_message(
            &workspace,
            &Notify::new(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 87,
                "method": "tools/call",
                "params": {
                    "name": "al_symbolsearch",
                    "arguments": {"query": "Customer"}
                }
            }),
        )
        .await
        .expect("response");
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert!(response["result"]["structuredContent"]["diagnostics"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["code"] == "AL_AGENT_MISSING_SYMBOLS")));
    }

    #[tokio::test]
    async fn al_runtests_blocked_by_bc_config_keeps_routing_and_diagnostic_context() {
        let workspace = ws();
        let temp = tempfile::tempdir().expect("temp project");
        install_project(&workspace, temp.path(), None).await;
        let path = temp.path().join("LiveOnly.Codeunit.al");
        let source = r#"codeunit 50100 "Live Only"
{
    Subtype = Test;

    [Test]
    procedure CallsHttp()
    var
        Client: HttpClient;
    begin
    end;
}
"#;
        workspace.file_index.add_file(path, source.to_string());

        let response = handle_mcp_message(
            &workspace,
            &Notify::new(),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 88,
                "method": "tools/call",
                "params": {"name": "al_runtests", "arguments": {}}
            }),
        )
        .await
        .expect("response");
        assert_eq!(response["result"]["isError"], true, "{response}");
        let structured = &response["result"]["structuredContent"];
        assert!(structured["diagnostics"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["code"] == "AL_AGENT_MISSING_BC_CONFIGURATION")
        }));
        let routing = structured["routing"].as_array().expect("routing context");
        assert_eq!(routing.len(), 1, "{response}");
        assert_eq!(routing[0]["decision"], "liveBc");
        assert_eq!(routing[0]["runsLocally"], false);
        assert!(routing[0]["reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty()));
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

    #[tokio::test]
    async fn named_tools_reject_fractional_out_of_range_and_unknown_arguments() {
        for (name, arguments, expected) in [
            (
                "al_symbolsearch",
                serde_json::json!({"query": "Customer", "limit": 1.5}),
                "wrong JSON type",
            ),
            (
                "al_trace_event",
                serde_json::json!({"event": "OnPost", "depth": 0}),
                "at least 1",
            ),
            (
                "al_symbolsearch",
                serde_json::json!({"query": "Customer", "limt": 20}),
                "not a supported argument",
            ),
            (
                "al_debug",
                serde_json::json!({"cmd": "start", "breakOnError": "Sometimes"}),
                "supported shape",
            ),
            (
                "al_debug",
                serde_json::json!({"cmd": "start", "authentication": "UserPassword"}),
                "must be one of",
            ),
        ] {
            let resp = handle_mcp_message(
                &ws(),
                &Notify::new(),
                serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":8,
                    "method":"tools/call",
                    "params":{"name":name, "arguments":arguments}
                }),
            )
            .await
            .expect("response");
            assert_eq!(resp["error"]["code"], -32602, "{resp}");
            assert!(
                resp["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(expected)),
                "{resp}"
            );
        }
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
            assert_eq!(
                schema["additionalProperties"], false,
                "{}: unknown top-level arguments must fail closed",
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

            let result = result_schema(t.name);
            if t.name == "al_call" {
                assert_eq!(
                    result,
                    serde_json::json!({}),
                    "al_call must retain its heterogeneous generic result"
                );
            } else {
                assert!(
                    result.get("type").is_some() || result.get("oneOf").is_some(),
                    "{}: named tools need a materially useful result schema",
                    t.name
                );
            }
        }
    }

    #[test]
    fn mcp_reference_lists_exactly_the_named_tool_registry() {
        let registered = tools()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let reference = include_str!("../../../../Docs/reference/mcp-tools.md");
        let documented = reference
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|line| line.split_once('`').map(|(tool, _)| tool))
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            documented, registered,
            "Docs/reference/mcp-tools.md must list exactly every named MCP tool"
        );
    }

    /// The agent surface (suggest-event, test-classify,
    /// test-coverage, live test snapshots, dependency-graph) is registered and listed.
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
            "al_testsnapshot",
            "al_testsnapshotreplay",
            "al_depgraph",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }

    #[test]
    fn min_items_is_enforced_for_array_arguments() {
        let tool = tools()
            .iter()
            .find(|tool| tool.name == "al_testsnapshot")
            .expect("al_testsnapshot is registered");
        let error = validate_tool_arguments(
            tool,
            &serde_json::json!({
                "codeunitId": 50100,
                "codeunitName": "Tests",
                "methodName": "Run",
                "bcVersion": "26.0",
                "outputPath": "snapshots/base.json",
                "breakpoints": [],
            }),
        )
        .expect_err("an empty breakpoints array violates the published minItems: 1");
        assert!(
            error.contains("at least 1 item"),
            "unexpected message: {error}"
        );

        validate_tool_arguments(
            tool,
            &serde_json::json!({
                "codeunitId": 50100,
                "codeunitName": "Tests",
                "methodName": "Run",
                "bcVersion": "26.0",
                "outputPath": "snapshots/base.json",
                "breakpoints": [{"file": "T.al", "line": 12}],
            }),
        )
        .expect("one breakpoint satisfies the schema");
    }

    /// `handle_mcp_message` treated `"id": null` as a notification while the
    /// session layer treated it as a request, so such a call silently hung.
    #[tokio::test]
    async fn a_null_id_is_answered_on_both_paths() {
        let response = handle_mcp_message(
            &ws(),
            &Notify::new(),
            serde_json::json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        )
        .await
        .expect("a request with a null id must receive a response");
        assert!(response["id"].is_null());
        assert_eq!(response["result"], serde_json::json!({}));
    }

    #[test]
    fn only_tools_call_requests_are_dispatched_concurrently() {
        assert!(is_long_running_call(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "al_build", "arguments": {}}
        })));
        // Lifecycle traffic stays on the reader loop so ordering is preserved.
        assert!(!is_long_running_call(
            &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})
        ));
        // A notification has no response to write.
        assert!(!is_long_running_call(
            &serde_json::json!({"jsonrpc": "2.0", "method": "tools/call"})
        ));
        // A malformed envelope is rejected by the session layer, not spawned.
        assert!(!is_long_running_call(
            &serde_json::json!({"id": 3, "method": "tools/call"})
        ));
    }

    #[test]
    fn cancellation_keys_distinguish_string_and_numeric_ids() {
        assert_eq!(
            cancellation_key(&serde_json::json!(7)),
            cancellation_key(&serde_json::json!(7))
        );
        assert_ne!(
            cancellation_key(&serde_json::json!(7)),
            cancellation_key(&serde_json::json!("7"))
        );
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

    fn initialize_message(id: u64) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"}
            }
        })
    }

    #[tokio::test]
    async fn session_enforces_initialize_then_initialized_notification() {
        let ws = ws();
        let shutdown = Notify::new();
        let mut lifecycle = McpLifecycle::default();

        let before = handle_mcp_session_message(
            &ws,
            &shutdown,
            &mut lifecycle,
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await
        .expect("pre-initialize request must receive an error");
        assert_eq!(before["error"]["code"], -32002);

        let initialized =
            handle_mcp_session_message(&ws, &shutdown, &mut lifecycle, initialize_message(2))
                .await
                .expect("initialize response");
        assert!(initialized["result"].is_object());
        assert_eq!(lifecycle, McpLifecycle::AwaitingInitializedNotification);

        let too_early = handle_mcp_session_message(
            &ws,
            &shutdown,
            &mut lifecycle,
            serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        )
        .await
        .expect("request before initialized notification must error");
        assert_eq!(too_early["error"]["code"], -32002);

        assert!(handle_mcp_session_message(
            &ws,
            &shutdown,
            &mut lifecycle,
            serde_json::json!({
                "jsonrpc":"2.0",
                "method":"notifications/initialized"
            }),
        )
        .await
        .is_none());
        assert_eq!(lifecycle, McpLifecycle::Ready);

        let list = handle_mcp_session_message(
            &ws,
            &shutdown,
            &mut lifecycle,
            serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
        )
        .await
        .expect("ready request");
        assert!(list["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn session_rejects_incomplete_or_duplicate_initialization() {
        let ws = ws();
        let shutdown = Notify::new();
        let mut lifecycle = McpLifecycle::default();
        let incomplete = handle_mcp_session_message(
            &ws,
            &shutdown,
            &mut lifecycle,
            serde_json::json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{"protocolVersion": MCP_PROTOCOL_VERSION}
            }),
        )
        .await
        .expect("invalid params response");
        assert_eq!(incomplete["error"]["code"], -32602);
        assert_eq!(lifecycle, McpLifecycle::Uninitialized);

        handle_mcp_session_message(&ws, &shutdown, &mut lifecycle, initialize_message(2))
            .await
            .expect("first initialize");
        let duplicate =
            handle_mcp_session_message(&ws, &shutdown, &mut lifecycle, initialize_message(3))
                .await
                .expect("duplicate response");
        assert_eq!(duplicate["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn session_rejects_malformed_json_rpc_envelopes() {
        for message in [
            serde_json::json!([]),
            serde_json::json!({"jsonrpc":"1.0","id":1,"method":"initialize"}),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":42}),
        ] {
            let response = handle_mcp_session_message(
                &ws(),
                &Notify::new(),
                &mut McpLifecycle::default(),
                message,
            )
            .await
            .expect("invalid request response");
            assert_eq!(response["error"]["code"], -32600, "{response}");
        }
    }
}
