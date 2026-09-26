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

/// A tool's declared schema plus the parameters the dispatch boundary adds.
///
/// `projection` and `scope` are applied to every method that lists, so each
/// tool would otherwise have to repeat their four properties, and a tool whose
/// schema forgot them would reject them (`additionalProperties: false`).
/// Deriving them from the method keeps the advertised schema and the accepted
/// arguments the same thing.
fn tool_schema(tool: &ToolDef) -> serde_json::Value {
    let mut schema = (tool.schema)();
    // `al_call` forwards an arbitrary method, so its `params` object carries
    // whatever that method takes; there is nothing to add here.
    if tool.name == "al_call" {
        return schema;
    }
    let reads_document = super::daemon::reads_document(tool.method);
    if reads_document {
        // Either spelling of the path names the document on its own, so a
        // schema that required one of them rejected a call carrying the other.
        if let Some(required) = schema
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
        {
            required.retain(|field| !matches!(field.as_str(), Some("uri" | "file")));
        }
    }
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return schema;
    };
    if reads_document {
        properties.entry("uri").or_insert_with(|| {
            serde_json::json!({
                "type": "string",
                "description": "File URI of the document to work on. Use this or 'file'.",
            })
        });
        properties.entry("file").or_insert_with(|| {
            serde_json::json!({
                "type": "string",
                "description": "Path of the document to work on. Use this or 'uri'.",
            })
        });
        properties.entry("text").or_insert_with(|| {
            serde_json::json!({
                "type": "string",
                "description": "Contents of the document, for a path outside the project that \
                                the daemon may not open. Needs the 'uri' or 'file' it stands for.",
            })
        });
    }
    if super::daemon::list_target(tool.method).is_some() {
        properties.insert(
            "limit".into(),
            serde_json::json!({
                "type": "integer",
                "minimum": 0,
                "description": format!(
                    "Rows to return; defaults to {MCP_DEFAULT_LIMIT}. The result reports total and truncated."
                ),
            }),
        );
        properties.insert(
            "offset".into(),
            serde_json::json!({
                "type": "integer",
                "minimum": 0,
                "description": "Rows to skip, for reading past a truncated page.",
            }),
        );
        properties.insert(
            "fields".into(),
            serde_json::json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Keep only these fields on each row. Omit for whole rows.",
            }),
        );
    }
    if super::daemon::accepts_scope(tool.method) {
        properties.insert(
            "scope".into(),
            serde_json::json!({
                "type": "string",
                "enum": ["workspace", "packages", "all"],
                "description": format!(
                    "Which code to report on; defaults to {MCP_DEFAULT_SCOPE}, the code this project can change. The result reports outOfScopeCount."
                ),
            }),
        );
    }
    schema
}

fn object_result_schema(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true,
    })
}

/// The shape a list-returning method answers with once the projection layer
/// has been through it.
///
/// MCP calls carry a default `limit`, so these never come back as a bare
/// array. `total` and `truncated` are the part an agent needs: without them a
/// page of 50 reads exactly like a complete answer of 50.
fn array_result_schema(item: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "items": {"type": "array", "items": item},
            "total": {"type": "integer", "description": "Rows before limit and offset."},
            "returned": {"type": "integer", "description": "Rows in items."},
            "offset": {"type": "integer"},
            "truncated": {
                "type": "boolean",
                "description": "True when rows follow this page. Raise offset by returned to read them."
            },
        },
        "required": ["items", "total", "returned", "offset", "truncated"],
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
        "al_freeids" => object_result_schema(
            serde_json::json!({
                "mode": {"type": "string", "enum": ["object", "summary", "field", "value"]},
                "kind": {"type": "string"},
                "object": {"type": "string"},
                "baseObject": {"type": "string"},
                "ranges": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "from": {"type": "integer"},
                            "to": {"type": "integer"},
                            "used": {"type": "integer"},
                            "free": {"type": "integer"},
                        },
                        "required": ["from", "to", "used", "free"],
                    }
                },
                "kinds": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "used": {"type": "integer"},
                            "free": {"type": "integer"},
                            "nextFree": {"type": "integer"},
                        },
                        "required": ["kind", "used", "free"],
                    }
                },
                "nextFree": {"type": "integer"},
                "free": {"type": "array", "items": {"type": "integer"}},
                "usedCount": {"type": "integer"},
                "freeCount": {"type": "integer"},
                "sources": {"type": "array", "items": {"type": "string"}},
                "used": {"type": "array", "items": {"type": "integer"}},
                "truncated": {"type": "boolean"},
                "warnings": {"type": "array", "items": {"type": "string"}},
            }),
            &["mode", "usedCount"],
        ),
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
                "depthCut": {
                    "type": "boolean",
                    "description": "A branch was cut at maxDepth calls; start deeper to see past it.",
                },
                "maxDepth": {"type": "integer"},
                "withoutSource": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Procedures on the trace whose source is not loaded, so the events they raise are not followed.",
                },
                "withoutSourceCount": {"type": "integer"},
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
    validate_schema_value(arguments, &tool_schema(tool), "arguments")
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
                          NuGet feeds (public Microsoft feeds by default) into .alpackages. \
                          Args: source (\"nuget\" or \"server\", default nuget), config \
                          (exact launch-configuration name, only for source=server; the \
                          project's first configuration is used when omitted, and the \
                          result names it either way).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "source": {"type": "string", "enum": ["nuget", "server"]},
                        "config": {"type": "string"}
                    }),
                    &[],
                )
            },
        },
        ToolDef {
            name: "al_symbolsearch",
            method: "search",
            description: "Fuzzy-search AL objects across loaded packages AND workspace \
                          source. Returns each object's kind, id, name and package; \
                          fetch its members with al_call object or byId. Args: query \
                          (string), limit (integer, default 20, maximum 500000), summary \
                          (boolean, default true; false includes every member).",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "query": {"type": "string"},
                        "summary": {"type": "boolean", "default": true},
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
            description: "Run diagnostics on an AL file. Args: file (path) or uri, plus \
                          text for a file outside the project the daemon may not open.",
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
                          or {type:'event', object, event}; optional `filterTable` \
                          restricts results to events taking that table as a `var` \
                          parameter, and `filterField` to events whose `var` record \
                          parameter's table declares a field of that name.",
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
            name: "al_freeids",
            method: "freeIds",
            description: "Pick the next free object ID, table field number or enum value ordinal \
                          inside the idRanges declared in app.json. Use before creating any new \
                          table, page, codeunit, report, query, xmlport, enum, permission set or \
                          extension object, and before adding a field to a table extension or a \
                          value to an enum extension. Counts every object in the workspace \
                          (including the second and later objects in a multi-object file) and \
                          every dependency package object inside the same range. Args: kind (an \
                          object-kind keyword such as table or tableextension; omit for a \
                          per-kind summary), object (a table, tableextension, enum or \
                          enumextension whose next free field number or ordinal is wanted, which \
                          takes precedence over kind), count (1 to 100, default 1) and \
                          includeUsed (default false; the answer carries counts, not the whole \
                          used list). An exhausted range is an error naming the range.",
            schema: || {
                obj_schema(
                    serde_json::json!({
                        "kind": {
                            "type": "string",
                            "enum": [
                                "table", "tableextension", "page", "pageextension", "codeunit",
                                "report", "reportextension", "xmlport", "query", "enum",
                                "enumextension", "permissionset", "permissionsetextension"
                            ],
                            "description": "Object kind to allocate an ID for. Omit for a summary of every kind in use."
                        },
                        "object": {
                            "type": "string",
                            "description": "Table, tableextension, enum or enumextension to allocate a field number or enum ordinal in. Wins over kind, which then only disambiguates a shared name."
                        },
                        "count": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 100,
                            "default": 1,
                            "description": "How many free numbers to return, in ascending order."
                        },
                        "includeUsed": {
                            "type": "boolean",
                            "default": false,
                            "description": "Add the full used-number list. Off by default because the answer is otherwise a few hundred bytes."
                        }
                    }),
                    &[],
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
    // MCP calls carry a default `limit`, so a list result arrives as the
    // projection envelope. `total: 0` is the empty case there, and checking
    // the envelope object itself would never see it.
    if let Some(total) = result.get("total").and_then(serde_json::Value::as_u64) {
        if result.get("items").is_some_and(serde_json::Value::is_array) {
            return total == 0;
        }
    }
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

    // A debug configuration that will not parse no longer stops the daemon, so
    // the agent has to be told why the BC-facing commands are unavailable while
    // symbol queries answer normally.
    if let Some(reason) = workspace
        .project
        .read()
        .await
        .as_ref()
        .and_then(|project| project.launch_config_error.clone())
    {
        diagnostics.push(agent_diagnostic(
            "AL_AGENT_INVALID_LAUNCH_CONFIGURATION",
            "warning",
            "The project's debug configuration file could not be read",
            reason,
            &[
                "Symbol, source, event and impact queries are unaffected; keep using them.",
                "Fix the reported field in .vscode/launch.json or .zed/debug.json before running compile, downloadSymbols, tests against live BC, or debug.",
                "Call al_call with method 'status' to see the message again after editing the file.",
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

/// Rows an MCP caller gets back when it does not say how many it wants.
///
/// The survey's five largest answers were between 89,000 and 2.4 million
/// tokens each, and the question behind every one of them had an answer of
/// twenty rows or fewer. An agent that needs more asks for it by `limit` or
/// pages with `offset`; the response says `total` and `truncated` either way.
pub(crate) const MCP_DEFAULT_LIMIT: u64 = 50;

/// The package scope an MCP caller gets when it does not say.
///
/// A developer can only change workspace code, so the workspace rows are the
/// actionable ones. `impact Item` returned 1,670 consumers of which the
/// workspace's were a handful.
pub(crate) const MCP_DEFAULT_SCOPE: &str = "workspace";

/// Add the agent-facing defaults to a forwarded tool call.
///
/// Only fills what the caller left out, so an explicit `limit`, `offset`,
/// `fields` or `scope` always wins, including `limit: 0` for a count.
fn apply_agent_defaults(method: &str, mut params: serde_json::Value) -> serde_json::Value {
    let Some(object) = params.as_object_mut() else {
        return params;
    };
    if super::daemon::list_target(method).is_some() && !object.contains_key("limit") {
        object.insert("limit".into(), serde_json::json!(MCP_DEFAULT_LIMIT));
    }
    if super::daemon::accepts_scope(method) && !object.contains_key("scope") {
        object.insert("scope".into(), serde_json::json!(MCP_DEFAULT_SCOPE));
    }
    // A search result names objects; their members are one `object` or
    // `byId` call away. With members, three hits for "Customer" were 212 KB,
    // 120 KB of them the Customer table's methods and fields.
    if method == "search" && !object.contains_key("summary") {
        object.insert("summary".into(), serde_json::json!(true));
    }
    // One line per member: Base Application's Customer was 113 KB with
    // members as objects, 110 KB of it field properties and parameter
    // objects.
    if matches!(method, "object" | "byId") && !object.contains_key("signatures") {
        object.insert("signatures".into(), serde_json::json!(true));
    }
    params
}

/// Handle one parsed MCP message. Returns the response to write, or `None`
/// for notifications (which get no response).
/// The `instructions` an MCP client shows the agent, plus the trust advisory
/// when this project asked for privileged settings it did not get.
///
/// An agent that is told what was ignored can relay it. A log line cannot
/// reach the conversation the agent is in.
fn mcp_instructions(workspace: &Arc<Workspace>) -> String {
    let base = "Use the named tools for validated common operations. Use al_call only for \
                daemon methods without a named tool.";
    match workspace.trust_advisory.get().and_then(Option::as_ref) {
        Some(advisory) => format!("{base}\n\n{advisory}"),
        None => base.to_string(),
    }
}

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
                "instructions": mcp_instructions(workspace)
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
                        "inputSchema": tool_schema(t),
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
            let daemon_params = apply_agent_defaults(daemon_method, daemon_params);
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
                        // Compact: indentation was over 40% of every answer,
                        // and an agent pays for it in tokens.
                        result.to_string(),
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
    // One frame per guard, so concurrent responses never interleave bytes.
    let mut guard = stdout.lock().await;
    guard.write_all(frame.to_string().as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await
}

/// Run the MCP server on stdio: newline-delimited JSON-RPC 2.0.
pub async fn run_mcp(project_root: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Arc::new(Workspace::new());
    let evaluated = al_project::trust::evaluate(&project_root)?;
    *workspace.config.write().await = evaluated.config;
    if let Some(advisory) = evaluated.decision.advisory() {
        tracing::warn!("mcp: {advisory}");
    }
    if let Some(advisory) = al_project::trust::enforce_dotnet_path(&project_root) {
        tracing::warn!("mcp: {advisory}");
    }
    let _ = workspace.trust_advisory.set(evaluated.decision.advisory());
    let _ = workspace.notify_sink.set(Arc::new(|msg: &str| {
        tracing::warn!("mcp: {msg}");
    }));
    super::daemon::initialize_daemon_workspace(&workspace, &project_root).await?;

    // Same warm-up as the daemon. An MCP server owns its workspace in process,
    // so without this the first event or impact question pays the whole
    // dependency source index and call-graph build inside the tool call: a
    // cold `al_trace_event` measured 599.7 s once and 12.9 s on the next call.
    // The build is single-flight, so this and a concurrent first tool call
    // join the same build rather than running two.
    let warm_workspace = Arc::clone(&workspace);
    tokio::task::spawn_blocking(move || match warm_workspace.get_or_build_call_graph() {
        Ok(_) => tracing::info!("mcp: dependency source index and call graph warm"),
        Err(error) => tracing::warn!(%error, "mcp: background index warm-up failed"),
    });

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
mod tests;
