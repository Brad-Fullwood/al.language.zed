//! Runtime validation for daemon responses consumed by manual CLI commands.
//!
//! The daemon is a separate process and JSON-RPC is an untyped boundary. Human
//! formatters must not turn a missing field into `0`, `false`, `?`, or an empty
//! list and then exit successfully. These contracts validate every field that a
//! manual formatter relies on before any output or exit-code decision is made.

use serde_json::Value;

#[derive(Clone, Copy)]
enum Kind {
    String,
    Integer,
    Unsigned,
    Number,
    Boolean,
    Array,
    Object,
}

impl Kind {
    fn matches(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Integer => value.is_i64(),
            Self::Unsigned => value.is_u64(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Array => value.is_array(),
            Self::Object => value.is_object(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Integer => "an integer",
            Self::Unsigned => "a non-negative integer",
            Self::Number => "a number",
            Self::Boolean => "a boolean",
            Self::Array => "an array",
            Self::Object => "an object",
        }
    }
}

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        "compile"
            | "package"
            | "debug"
            | "snapshot"
            | "profiling"
            | "trace"
            | "graphExport"
            | "deadCode"
            | "impact"
            | "tableImpact"
            | "traceChain"
            | "eventMap"
            | "suggestEvent"
            | "clearCache"
            | "shutdown"
            | "setup"
            | "downloadSymbols"
            | "lint"
            | "format"
            | "hover"
            | "definition"
            | "typeDefinition"
            | "declaration"
            | "implementation"
            | "references"
            | "signatureHelp"
            | "completions"
            | "documentSymbols"
            | "foldingRanges"
            | "semanticTokens"
            | "rename"
            | "permissions"
            | "parse"
            | "inlayHints"
            | "fix"
            | "authenticate"
            | "newProject"
            | "metrics"
            | "profiler.hints"
            | "sortMembers"
            | "organizeFiles"
            | "tests.snapshot_capture"
            | "tests.snapshot_validate"
            | "tests.snapshot_replay"
            | "tests.snapshot_diff"
            | "tests.mutate"
            | "search"
            | "source"
            | "location"
            | "events"
            | "subscribers"
            | "eventSource"
            | "composed"
            | "packages"
            | "generate"
            | "deps.graph"
            | "duplicates"
            | "fix.applicationArea"
            | "fix.tooltips"
            | "fix.dataClassification"
            | "tests.run"
            | "tests.run_batch"
            | "tests.run_auto"
            | "tests.last_results"
    )
}

pub(super) fn validate(method: &str, params: Option<&Value>, result: &Value) -> Result<(), String> {
    let validation = match method {
        "compile" | "package" => validate_build(result),
        "debug" => validate_debug(params, result),
        "snapshot" => validate_snapshot(params, result),
        "profiling" => validate_profiling(params, result),
        "trace" => array_objects(
            result,
            "trace",
            &[
                ("depth", Kind::Unsigned),
                ("edgeType", Kind::String),
                ("nodeType", Kind::String),
                ("name", Kind::String),
                ("object", Kind::String),
            ],
        ),
        "graphExport" => validate_graph_export(params, result),
        "deadCode" => array_objects(
            result,
            "deadCode",
            &[
                ("k", Kind::String),
                ("n", Kind::String),
                ("obj", Kind::String),
                ("f", Kind::String),
                ("l", Kind::Unsigned),
                ("confidence", Kind::String),
            ],
        ),
        "impact" => {
            fields(
                result,
                "impact",
                &[("symbol", Kind::String), ("impacted", Kind::Array)],
            )?;
            named_array_objects(
                result,
                "impacted",
                &[
                    ("k", Kind::String),
                    ("id", Kind::Integer),
                    ("n", Kind::String),
                    ("type", Kind::String),
                ],
            )?;
            for (index, entry) in result["impacted"]
                .as_array()
                .ok_or_else(|| "impact.impacted must be an array".to_string())?
                .iter()
                .enumerate()
            {
                optional_kind(entry, "field", Kind::String, false)?;
                optional_kind(entry, "proc", Kind::String, false)?;
                optional_kind(entry, "package", Kind::String, false)
                    .map_err(|reason| format!("impact.impacted[{index}]: {reason}"))?;
            }
            Ok(())
        }
        "tableImpact" => validate_table_impact(result),
        "traceChain" => validate_trace_chain(result),
        "eventMap" => validate_event_map(result),
        "suggestEvent" => validate_suggest_event(result),
        "clearCache" => {
            fields(
                result,
                "clearCache",
                &[
                    ("deleted", Kind::Boolean),
                    ("existed", Kind::Boolean),
                    ("path", Kind::String),
                ],
            )?;
            optional_kind(result, "error", Kind::String, true)
        }
        "shutdown" => fields(result, "shutdown", &[("shutdownRequested", Kind::Boolean)]),
        "setup" => validate_setup(result),
        "downloadSymbols" => validate_download_symbols(result),
        "lint" => validate_lint(params, result),
        "format" => validate_format(params, result),
        "hover" => {
            if result.is_null() {
                Ok(())
            } else {
                fields(result, "hover", &[("contents", Kind::String)])
            }
        }
        "definition" | "typeDefinition" | "declaration" | "implementation" => {
            validate_locations_or_null(result, method)
        }
        "references" => {
            if result.is_null() {
                Ok(())
            } else {
                validate_location_array(result, "references")
            }
        }
        "signatureHelp" => {
            if result.is_null() {
                Ok(())
            } else {
                fields(result, "signatureHelp", &[("signatures", Kind::Array)])
            }
        }
        "completions" => array_objects(result, "completions", &[("label", Kind::String)]),
        "documentSymbols" => {
            if result.is_null() {
                Ok(())
            } else {
                array_objects(
                    result,
                    "documentSymbols",
                    &[
                        ("name", Kind::String),
                        ("kind", Kind::String),
                        ("range", Kind::Object),
                    ],
                )
            }
        }
        "foldingRanges" => {
            if result.is_null() {
                Ok(())
            } else {
                array_objects(
                    result,
                    "foldingRanges",
                    &[("start_line", Kind::Unsigned), ("end_line", Kind::Unsigned)],
                )
            }
        }
        "semanticTokens" => {
            if result.is_null() {
                Ok(())
            } else {
                array_objects(
                    result,
                    "semanticTokens",
                    &[
                        ("deltaLine", Kind::Unsigned),
                        ("deltaStart", Kind::Unsigned),
                        ("length", Kind::Unsigned),
                        ("tokenType", Kind::Unsigned),
                        ("tokenModifiers", Kind::Unsigned),
                    ],
                )
            }
        }
        "rename" => validate_workspace_edit_or_null(result),
        "permissions" => fields(
            result,
            "permissions",
            &[
                ("format", Kind::String),
                ("content", Kind::String),
                ("objectCount", Kind::Unsigned),
            ],
        ),
        "parse" => {
            fields(
                result,
                "parse",
                &[
                    ("errors", Kind::Unsigned),
                    ("nodeCount", Kind::Unsigned),
                    ("parseTimeMs", Kind::Number),
                    ("parseErrors", Kind::Array),
                ],
            )?;
            named_array_objects(
                result,
                "parseErrors",
                &[
                    ("line", Kind::Unsigned),
                    ("column", Kind::Unsigned),
                    ("message", Kind::String),
                ],
            )
        }
        "inlayHints" => array_objects(result, "inlayHints", &[("position", Kind::Object)]),
        "fix" => fields(
            result,
            "fix",
            &[
                ("diagnostics", Kind::Unsigned),
                ("fixes", Kind::Unsigned),
                ("dryRun", Kind::Boolean),
            ],
        ),
        "authenticate" => validate_authenticate(params, result),
        "newProject" => {
            fields(
                result,
                "newProject",
                &[("projectDir", Kind::String), ("filesCreated", Kind::Array)],
            )?;
            let files_created = result
                .get("filesCreated")
                .ok_or_else(|| "newProject is missing required field 'filesCreated'".to_string())?;
            string_array(files_created, "newProject.filesCreated")
        }
        "metrics" => validate_metrics(params, result),
        "profiler.hints" => array_objects(
            result,
            "profiler.hints",
            &[
                ("procedure", Kind::String),
                ("object", Kind::String),
                ("selfTimeMs", Kind::Number),
                ("hitCount", Kind::Unsigned),
            ],
        ),
        "sortMembers" => fields(
            result,
            "sortMembers",
            &[("changed", Kind::Boolean), ("dryRun", Kind::Boolean)],
        ),
        "organizeFiles" => {
            fields(
                result,
                "organizeFiles",
                &[("files", Kind::Array), ("dryRun", Kind::Boolean)],
            )?;
            named_array_objects(
                result,
                "files",
                &[
                    ("from", Kind::String),
                    ("to", Kind::String),
                    ("renamed", Kind::Boolean),
                ],
            )
        }
        "tests.snapshot_capture" => fields(
            result,
            "tests.snapshot_capture",
            &[
                ("captured", Kind::Boolean),
                ("snapshotPath", Kind::String),
                ("sampleCount", Kind::Unsigned),
                ("codeunitId", Kind::Integer),
                ("methodName", Kind::String),
                ("bcVersion", Kind::String),
                ("testResult", Kind::Object),
            ],
        ),
        "tests.snapshot_validate" => fields(
            result,
            "tests.snapshot_validate",
            &[
                ("valid", Kind::Boolean),
                ("sampleCount", Kind::Unsigned),
                ("codeunitId", Kind::Integer),
                ("methodName", Kind::String),
                ("bcVersion", Kind::String),
            ],
        ),
        "tests.snapshot_replay" => {
            fields(
                result,
                "tests.snapshot_replay",
                &[
                    ("replayed", Kind::Boolean),
                    ("matched", Kind::Boolean),
                    ("divergences", Kind::Array),
                ],
            )?;
            validate_divergences(result)
        }
        "tests.snapshot_diff" => {
            fields(
                result,
                "tests.snapshot_diff",
                &[("divergences", Kind::Array)],
            )?;
            validate_divergences(result)
        }
        "tests.mutate" => validate_mutation(result),
        "search" => array_objects(
            result,
            "search",
            &[
                ("kind", Kind::String),
                ("id", Kind::Integer),
                ("name", Kind::String),
                ("package", Kind::String),
                ("source_availability", Kind::String),
            ],
        ),
        "source" => fields(
            result,
            "source",
            &[
                ("source_availability", Kind::String),
                ("code", Kind::String),
            ],
        ),
        "location" => validate_source_location(result),
        "events" => validate_events(result),
        "subscribers" => validate_subscribers(result),
        "eventSource" => validate_event_source(result),
        "composed" => fields(
            result,
            "composed",
            &[
                ("base", Kind::Object),
                ("extensions", Kind::Array),
                ("all_fields", Kind::Array),
                ("all_methods", Kind::Array),
            ],
        ),
        "packages" => array_objects(
            result,
            "packages",
            &[
                ("name", Kind::String),
                ("publisher", Kind::String),
                ("version", Kind::String),
                ("object_count", Kind::Unsigned),
                ("source_availability", Kind::Object),
            ],
        ),
        "generate" => fields(
            result,
            "generate",
            &[("code", Kind::String), ("kind", Kind::String)],
        ),
        "deps.graph" => validate_dependency_graph(params, result),
        "duplicates" => validate_duplicates(result),
        "fix.applicationArea" | "fix.tooltips" | "fix.dataClassification" => {
            validate_bulk_fix(result)
        }
        "tests.run" => validate_test_run(result),
        "tests.run_batch" | "tests.run_auto" => validate_test_run_batch(method, result),
        "tests.last_results" => validate_test_last_results(params, result),
        _ => {
            return Err(format!(
                "no manual response contract is registered for '{method}'"
            ));
        }
    };
    validation
        .map_err(|reason| format!("daemon returned an invalid response for '{method}': {reason}"))
}

fn fields(value: &Value, label: &str, expected: &[(&str, Kind)]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))?;
    for (name, kind) in expected {
        let field = object
            .get(*name)
            .ok_or_else(|| format!("{label} is missing required field '{name}'"))?;
        if !kind.matches(field) {
            return Err(format!(
                "{label}.{name} must be {}, got {}",
                kind.label(),
                type_name(field)
            ));
        }
    }
    Ok(())
}

fn array_objects(value: &Value, label: &str, expected: &[(&str, Kind)]) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    for (index, item) in items.iter().enumerate() {
        fields(item, &format!("{label}[{index}]"), expected)?;
    }
    Ok(())
}

fn named_array_objects(
    value: &Value,
    field: &str,
    expected: &[(&str, Kind)],
) -> Result<(), String> {
    let items = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} must be an array"))?;
    for (index, item) in items.iter().enumerate() {
        fields(item, &format!("{field}[{index}]"), expected)?;
    }
    Ok(())
}

fn string_array(value: &Value, label: &str) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    for (index, item) in items.iter().enumerate() {
        if !item.is_string() {
            return Err(format!("{label}[{index}] must be a string"));
        }
    }
    Ok(())
}

fn optional_kind(value: &Value, field: &str, kind: Kind, allow_null: bool) -> Result<(), String> {
    let Some(field_value) = value.get(field) else {
        return Ok(());
    };
    if allow_null && field_value.is_null() {
        return Ok(());
    }
    if kind.matches(field_value) {
        Ok(())
    } else {
        Err(format!("{field} must be {}", kind.label()))
    }
}

fn requested_command<'a>(params: Option<&'a Value>, method: &str) -> Result<&'a str, String> {
    params
        .and_then(|value| value.get("cmd"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{method} request has no string 'cmd' for response validation"))
}

fn validate_build(result: &Value) -> Result<(), String> {
    fields(
        result,
        "build",
        &[
            ("success", Kind::Boolean),
            ("diagnostics", Kind::Array),
            ("output", Kind::String),
            ("backend", Kind::String),
            ("validated", Kind::Boolean),
        ],
    )?;
    optional_kind(result, "appPath", Kind::String, true)?;
    optional_kind(result, "verificationLevel", Kind::String, true)?;
    named_array_objects(
        result,
        "diagnostics",
        &[
            ("file", Kind::String),
            ("line", Kind::Unsigned),
            ("column", Kind::Unsigned),
            ("severity", Kind::String),
            ("code", Kind::String),
            ("message", Kind::String),
        ],
    )
}

fn validate_debug(params: Option<&Value>, result: &Value) -> Result<(), String> {
    match requested_command(params, "debug")? {
        "start" => fields(
            result,
            "debug.start",
            &[("session", Kind::String), ("status", Kind::String)],
        ),
        "breakpoint" => {
            fields(result, "debug.breakpoint", &[("breakpoints", Kind::Array)])?;
            named_array_objects(
                result,
                "breakpoints",
                &[("line", Kind::Unsigned), ("verified", Kind::Boolean)],
            )
        }
        "state" => {
            fields(
                result,
                "debug.state",
                &[("sessionId", Kind::String), ("status", Kind::String)],
            )?;
            optional_kind(result, "location", Kind::Object, true)?;
            optional_kind(result, "variables", Kind::Array, false)
        }
        "eval" => fields(
            result,
            "debug.eval",
            &[("result", Kind::String), ("typeName", Kind::String)],
        ),
        "continue" | "step" => {
            fields(
                result,
                "debug state transition",
                &[("status", Kind::String)],
            )?;
            optional_kind(result, "location", Kind::Object, true)
        }
        "history" => {
            fields(result, "debug.history", &[("hits", Kind::Array)])?;
            named_array_objects(
                result,
                "hits",
                &[("seq", Kind::Unsigned), ("timestamp", Kind::String)],
            )
        }
        "stop" => fields(result, "debug.stop", &[("status", Kind::String)]),
        other => Err(format!("unsupported debug response command '{other}'")),
    }
}

fn validate_snapshot(params: Option<&Value>, result: &Value) -> Result<(), String> {
    match requested_command(params, "snapshot")? {
        "start" => fields(
            result,
            "snapshot.start",
            &[("snapshotId", Kind::String), ("status", Kind::String)],
        ),
        "list" => {
            fields(result, "snapshot.list", &[("snapshots", Kind::Array)])?;
            named_array_objects(
                result,
                "snapshots",
                &[
                    ("id", Kind::String),
                    ("createdAt", Kind::String),
                    ("sizeBytes", Kind::Unsigned),
                ],
            )
        }
        "download" => fields(
            result,
            "snapshot.download",
            &[("path", Kind::String), ("status", Kind::String)],
        ),
        other => Err(format!("unsupported snapshot response command '{other}'")),
    }
}

fn validate_profiling(params: Option<&Value>, result: &Value) -> Result<(), String> {
    match requested_command(params, "profiling")? {
        "start" => fields(
            result,
            "profiling.start",
            &[("sessionId", Kind::String), ("status", Kind::String)],
        ),
        "stop" => fields(
            result,
            "profiling.stop",
            &[("path", Kind::String), ("status", Kind::String)],
        ),
        "analyze" => {
            fields(
                result,
                "profiling.analyze",
                &[("durationMs", Kind::Number), ("hotspots", Kind::Array)],
            )?;
            named_array_objects(
                result,
                "hotspots",
                &[
                    ("procedure", Kind::String),
                    ("selfTimeMs", Kind::Number),
                    ("totalTimeMs", Kind::Number),
                    ("hitCount", Kind::Unsigned),
                ],
            )
        }
        other => Err(format!("unsupported profiling response command '{other}'")),
    }
}

fn validate_graph_export(params: Option<&Value>, result: &Value) -> Result<(), String> {
    let format = params
        .and_then(|value| value.get("format"))
        .and_then(Value::as_str)
        .ok_or_else(|| "graphExport request has no string format".to_string())?;
    match format {
        "dot" => fields(
            result,
            "graphExport.dot",
            &[("format", Kind::String), ("content", Kind::String)],
        ),
        "json" => fields(
            result,
            "graphExport.json",
            &[("nodes", Kind::Array), ("edges", Kind::Array)],
        ),
        other => Err(format!("unsupported graphExport format '{other}'")),
    }
}

fn validate_table_impact(result: &Value) -> Result<(), String> {
    fields(
        result,
        "tableImpact",
        &[
            ("tableName", Kind::String),
            ("objects", Kind::Array),
            ("totalImpacts", Kind::Unsigned),
        ],
    )?;
    let objects = result
        .get("objects")
        .ok_or_else(|| "tableImpact is missing required field 'objects'".to_string())?
        .as_array()
        .ok_or_else(|| "tableImpact.objects must be an array".to_string())?;
    for (index, object) in objects.iter().enumerate() {
        fields(
            object,
            &format!("tableImpact.objects[{index}]"),
            &[
                ("objectKind", Kind::String),
                ("objectName", Kind::String),
                ("impacts", Kind::Array),
            ],
        )?;
        named_array_objects(
            object,
            "impacts",
            &[("operation", Kind::String), ("locationHint", Kind::String)],
        )?;
    }
    Ok(())
}

fn validate_trace_chain(result: &Value) -> Result<(), String> {
    fields(
        result,
        "traceChain",
        &[
            ("publisherObject", Kind::String),
            ("chains", Kind::Array),
            ("nodesVisited", Kind::Unsigned),
        ],
    )?;
    let chains = result
        .get("chains")
        .ok_or_else(|| "traceChain is missing required field 'chains'".to_string())?
        .as_array()
        .ok_or_else(|| "traceChain.chains must be an array".to_string())?;
    for (index, node) in chains.iter().enumerate() {
        validate_chain_node(node, &format!("traceChain.chains[{index}]"))?;
    }
    Ok(())
}

fn validate_chain_node(node: &Value, label: &str) -> Result<(), String> {
    fields(
        node,
        label,
        &[
            ("edgeKind", Kind::String),
            ("nodeType", Kind::String),
            ("name", Kind::String),
            ("object", Kind::String),
            ("cycle", Kind::Boolean),
            ("children", Kind::Array),
        ],
    )?;
    let children = node
        .get("children")
        .ok_or_else(|| format!("{label} is missing required field 'children'"))?
        .as_array()
        .ok_or_else(|| format!("{label}.children must be an array"))?;
    for (index, child) in children.iter().enumerate() {
        validate_chain_node(child, &format!("{label}.children[{index}]"))?;
    }
    Ok(())
}

fn validate_event_map(result: &Value) -> Result<(), String> {
    fields(
        result,
        "eventMap",
        &[
            ("events", Kind::Array),
            ("orphanSubscribers", Kind::Array),
            ("totalEvents", Kind::Unsigned),
        ],
    )?;
    named_array_objects(
        result,
        "events",
        &[
            ("eventName", Kind::String),
            ("publisher", Kind::Object),
            ("subscriberCount", Kind::Unsigned),
            ("subscribers", Kind::Array),
        ],
    )?;
    named_array_objects(
        result,
        "orphanSubscribers",
        &[
            ("objectName", Kind::String),
            ("methodName", Kind::String),
            ("targetObject", Kind::String),
            ("targetEvent", Kind::String),
        ],
    )
}

fn validate_suggest_event(result: &Value) -> Result<(), String> {
    fields(
        result,
        "suggestEvent",
        &[
            ("integrationPoints", Kind::Array),
            ("partial", Kind::Boolean),
        ],
    )?;
    named_array_objects(
        result,
        "integrationPoints",
        &[
            ("event", Kind::String),
            ("object", Kind::String),
            ("eventType", Kind::String),
            ("example", Kind::String),
            ("params", Kind::Array),
        ],
    )
}

fn validate_setup(result: &Value) -> Result<(), String> {
    fields(
        result,
        "setup",
        &[
            ("altoolInstalled", Kind::Boolean),
            ("indexedSymbols", Kind::Unsigned),
            ("workspaceFiles", Kind::Unsigned),
        ],
    )?;
    optional_kind(result, "dotnetVersion", Kind::String, true)?;
    optional_kind(result, "toolchain", Kind::Object, true)?;
    optional_kind(result, "project", Kind::Object, true)
}

fn validate_download_symbols(result: &Value) -> Result<(), String> {
    fields(
        result,
        "downloadSymbols",
        &[
            ("source", Kind::String),
            ("downloaded", Kind::Unsigned),
            ("skipped", Kind::Unsigned),
            ("failed", Kind::Unsigned),
            ("results", Kind::Array),
        ],
    )?;
    named_array_objects(
        result,
        "results",
        &[("name", Kind::String), ("status", Kind::String)],
    )
}

fn validate_lint(params: Option<&Value>, result: &Value) -> Result<(), String> {
    let all = params
        .and_then(|value| value.get("all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if all {
        array_objects(
            result,
            "lint --all",
            &[("file", Kind::String), ("diagnostics", Kind::Array)],
        )?;
        let files = result
            .as_array()
            .ok_or_else(|| "lint --all must be an array".to_string())?;
        for (index, file) in files.iter().enumerate() {
            let diagnostics = file
                .get("diagnostics")
                .ok_or_else(|| format!("lint[{index}] is missing required field 'diagnostics'"))?;
            validate_diagnostics(diagnostics, &format!("lint[{index}].diagnostics"))?;
        }
        Ok(())
    } else {
        validate_diagnostics(result, "lint")
    }
}

fn validate_diagnostics(value: &Value, label: &str) -> Result<(), String> {
    array_objects(
        value,
        label,
        &[
            ("code", Kind::String),
            ("message", Kind::String),
            ("severity", Kind::String),
            ("line", Kind::Unsigned),
            ("column", Kind::Unsigned),
        ],
    )
}

fn validate_format(params: Option<&Value>, result: &Value) -> Result<(), String> {
    fields(result, "format", &[("changed", Kind::Boolean)])?;
    let check = params
        .and_then(|value| value.get("check"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if check {
        Ok(())
    } else {
        fields(result, "format", &[("formatted", Kind::String)])
    }
}

fn validate_locations_or_null(result: &Value, label: &str) -> Result<(), String> {
    if result.is_null() {
        return Ok(());
    }
    if result.is_array() {
        validate_location_array(result, label)
    } else {
        validate_location(result, label)
    }
}

fn validate_location_array(result: &Value, label: &str) -> Result<(), String> {
    let locations = result
        .as_array()
        .ok_or_else(|| format!("{label} must be an array of locations"))?;
    for (index, location) in locations.iter().enumerate() {
        validate_location(location, &format!("{label}[{index}]"))?;
    }
    Ok(())
}

fn validate_location(value: &Value, label: &str) -> Result<(), String> {
    fields(
        value,
        label,
        &[("uri", Kind::String), ("range", Kind::Object)],
    )?;
    validate_range(&value["range"], &format!("{label}.range"))
}

fn validate_range(value: &Value, label: &str) -> Result<(), String> {
    fields(
        value,
        label,
        &[("start", Kind::Object), ("end", Kind::Object)],
    )?;
    for point in ["start", "end"] {
        fields(
            &value[point],
            &format!("{label}.{point}"),
            &[("line", Kind::Unsigned), ("character", Kind::Unsigned)],
        )?;
    }
    Ok(())
}

fn validate_workspace_edit_or_null(result: &Value) -> Result<(), String> {
    if result.is_null() {
        return Ok(());
    }
    fields(result, "rename", &[("changes", Kind::Object)])?;
    let changes = result
        .get("changes")
        .ok_or_else(|| "rename is missing required field 'changes'".to_string())?
        .as_object()
        .ok_or_else(|| "rename.changes must be an object".to_string())?;
    for (uri, edits) in changes {
        let edits = edits
            .as_array()
            .ok_or_else(|| format!("rename.changes[{uri:?}] must be an array"))?;
        for (index, edit) in edits.iter().enumerate() {
            fields(
                edit,
                &format!("rename.changes[{uri:?}][{index}]"),
                &[("range", Kind::Object), ("newText", Kind::String)],
            )?;
            validate_range(
                &edit["range"],
                &format!("rename.changes[{uri:?}][{index}].range"),
            )?;
        }
    }
    Ok(())
}

fn validate_authenticate(params: Option<&Value>, result: &Value) -> Result<(), String> {
    match requested_command(params, "authenticate")? {
        "status" => {
            fields(result, "authenticate.status", &[("tenants", Kind::Array)])?;
            named_array_objects(
                result,
                "tenants",
                &[("tenant", Kind::String), ("authenticated", Kind::Boolean)],
            )
        }
        "clear" => fields(result, "authenticate.clear", &[("cleared", Kind::Unsigned)]),
        "login" => fields(
            result,
            "authenticate.login",
            &[
                ("status", Kind::String),
                ("tenant", Kind::String),
                ("messages", Kind::Array),
            ],
        ),
        other => Err(format!(
            "unsupported authenticate response command '{other}'"
        )),
    }
}

fn validate_metrics(params: Option<&Value>, result: &Value) -> Result<(), String> {
    let all = params
        .and_then(|value| value.get("all"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if all {
        array_objects(
            result,
            "metrics --all",
            &[("file", Kind::String), ("hotspots", Kind::Array)],
        )
    } else {
        fields(
            result,
            "metrics",
            &[
                ("procedures", Kind::Array),
                ("hotspots", Kind::Array),
                ("thresholdCyclomatic", Kind::Unsigned),
                ("thresholdCognitive", Kind::Unsigned),
            ],
        )
    }
}

fn validate_divergences(result: &Value) -> Result<(), String> {
    named_array_objects(
        result,
        "divergences",
        &[
            ("breakpoint_id", Kind::Unsigned),
            ("iteration", Kind::Unsigned),
            ("field_path", Kind::String),
        ],
    )
}

fn validate_mutation(result: &Value) -> Result<(), String> {
    fields(
        result,
        "tests.mutate",
        &[
            ("killed", Kind::Unsigned),
            ("survived", Kind::Unsigned),
            ("errored", Kind::Unsigned),
            ("variants", Kind::Array),
            ("unscored", Kind::Unsigned),
        ],
    )?;
    optional_kind(result, "mutationScore", Kind::Number, true)
}

fn validate_events(result: &Value) -> Result<(), String> {
    array_objects(
        result,
        "events",
        &[
            ("objectKind", Kind::String),
            ("objectName", Kind::String),
            ("methodName", Kind::String),
            ("eventType", Kind::String),
            ("parameters", Kind::Array),
        ],
    )
}

fn validate_source_location(result: &Value) -> Result<(), String> {
    fields(
        result,
        "location",
        &[
            ("path", Kind::String),
            ("line", Kind::Unsigned),
            ("source_availability", Kind::String),
        ],
    )?;
    optional_kind(result, "virtual", Kind::Boolean, false)
}

fn validate_subscribers(result: &Value) -> Result<(), String> {
    array_objects(
        result,
        "subscribers",
        &[
            ("objectName", Kind::String),
            ("methodName", Kind::String),
            ("targetObjectType", Kind::String),
            ("targetObjectName", Kind::String),
            ("targetEventName", Kind::String),
        ],
    )
}

fn validate_event_source(result: &Value) -> Result<(), String> {
    fields(
        result,
        "eventSource",
        &[
            ("targetKind", Kind::String),
            ("targetObject", Kind::String),
            ("targetEvent", Kind::String),
            ("sourceAvailability", Kind::String),
        ],
    )?;
    optional_kind(result, "path", Kind::String, true)?;
    optional_kind(result, "line", Kind::Unsigned, true)?;
    optional_kind(result, "signature", Kind::String, true)?;
    optional_kind(result, "note", Kind::String, true)
}

fn validate_dependency_graph(params: Option<&Value>, result: &Value) -> Result<(), String> {
    let format = params
        .and_then(|value| value.get("format"))
        .and_then(Value::as_str)
        .ok_or_else(|| "deps.graph request has no string format".to_string())?;
    match format {
        "dot" => {
            fields(result, "deps.graph dot", &[("format", Kind::String)])?;
            if result.get("content").is_some_and(Value::is_string)
                || result.get("dot").is_some_and(Value::is_string)
            {
                Ok(())
            } else {
                Err("deps.graph dot response requires string 'content' or 'dot'".to_string())
            }
        }
        "json" => fields(
            result,
            "deps.graph json",
            &[("nodes", Kind::Array), ("edges", Kind::Array)],
        ),
        other => Err(format!("unsupported deps.graph format '{other}'")),
    }
}

fn validate_duplicates(result: &Value) -> Result<(), String> {
    array_objects(
        result,
        "duplicates",
        &[
            ("similarity", Kind::Number),
            ("first", Kind::Object),
            ("second", Kind::Object),
        ],
    )
}

fn validate_bulk_fix(result: &Value) -> Result<(), String> {
    fields(
        result,
        "bulk fix",
        &[
            ("modifiedFiles", Kind::Array),
            ("changesCount", Kind::Unsigned),
            ("dryRun", Kind::Boolean),
        ],
    )?;
    let modified_files = result
        .get("modifiedFiles")
        .ok_or_else(|| "bulk fix is missing required field 'modifiedFiles'".to_string())?;
    string_array(modified_files, "modifiedFiles")
}

fn validate_test_run(result: &Value) -> Result<(), String> {
    fields(result, "tests.run", &[("result", Kind::Object)])?;
    let run = &result["result"];
    fields(
        run,
        "tests.run.result",
        &[
            ("name", Kind::String),
            ("total", Kind::Unsigned),
            ("passed", Kind::Unsigned),
            ("failed", Kind::Unsigned),
            ("skipped", Kind::Unsigned),
            ("methods", Kind::Array),
        ],
    )?;
    validate_test_method_results(run, "tests.run.result")
}

fn validate_test_run_batch(method: &str, result: &Value) -> Result<(), String> {
    fields(
        result,
        method,
        &[
            ("summaries", Kind::Array),
            ("routing", Kind::Array),
            ("totals", Kind::Object),
        ],
    )?;
    named_array_objects(
        result,
        "summaries",
        &[
            ("name", Kind::String),
            ("id", Kind::Integer),
            ("methods", Kind::Array),
            ("total", Kind::Unsigned),
            ("passed", Kind::Unsigned),
            ("failed", Kind::Unsigned),
            ("skipped", Kind::Unsigned),
        ],
    )?;
    for (index, summary) in result["summaries"]
        .as_array()
        .ok_or_else(|| format!("{method}.summaries must be an array"))?
        .iter()
        .enumerate()
    {
        validate_test_method_results(summary, &format!("{method}.summaries[{index}]"))?;
    }
    validate_test_routing(result, method)?;
    fields(
        &result["totals"],
        &format!("{method}.totals"),
        &[
            ("total", Kind::Unsigned),
            ("passed", Kind::Unsigned),
            ("failed", Kind::Unsigned),
            ("skipped", Kind::Unsigned),
        ],
    )
}

fn validate_test_method_results(container: &Value, label: &str) -> Result<(), String> {
    let methods = container
        .get("methods")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}.methods must be an array"))?;
    for (index, method) in methods.iter().enumerate() {
        let item_label = format!("{label}.methods[{index}]");
        fields(
            method,
            &item_label,
            &[("name", Kind::String), ("status", Kind::String)],
        )?;
        validate_test_status(&method["status"], &format!("{item_label}.status"))?;
        optional_kind(method, "durationMs", Kind::Unsigned, true)?;
        optional_kind(method, "error", Kind::String, true)?;
    }
    Ok(())
}

fn validate_test_routing(result: &Value, method: &str) -> Result<(), String> {
    let routing = result
        .get("routing")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{method}.routing must be an array"))?;
    for (index, route) in routing.iter().enumerate() {
        let label = format!("{method}.routing[{index}]");
        fields(
            route,
            &label,
            &[
                ("codeunitId", Kind::Integer),
                ("codeunitName", Kind::String),
                ("decision", Kind::String),
                ("runsLocally", Kind::Boolean),
                ("execution", Kind::String),
                ("reasons", Kind::Array),
            ],
        )?;
        let method_name = route
            .get("methodName")
            .ok_or_else(|| format!("{label} is missing required field 'methodName'"))?;
        if !method_name.is_null() && !method_name.is_string() {
            return Err(format!("{label}.methodName must be a string or null"));
        }
        optional_kind(route, "classifiedDecision", Kind::String, true)?;
        let reasons = route["reasons"]
            .as_array()
            .ok_or_else(|| format!("{label}.reasons must be an array"))?;
        for (reason_index, reason) in reasons.iter().enumerate() {
            let reason_label = format!("{label}.reasons[{reason_index}]");
            fields(reason, &reason_label, &[("message", Kind::String)])?;
            optional_kind(reason, "file", Kind::String, true)?;
            optional_kind(reason, "line", Kind::Unsigned, true)?;
        }
    }
    Ok(())
}

fn validate_test_last_results(params: Option<&Value>, result: &Value) -> Result<(), String> {
    let specific = params.and_then(|value| value.get("methodName")).is_some();
    if specific {
        let last = result.get("lastResult").ok_or_else(|| {
            "tests.last_results is missing required field 'lastResult'".to_string()
        })?;
        if last.is_null() {
            return Ok(());
        }
        return validate_test_history_record(last, "tests.last_results.lastResult");
    }

    fields(result, "tests.last_results", &[("results", Kind::Array)])?;
    for (index, record) in result["results"]
        .as_array()
        .ok_or_else(|| "tests.last_results.results must be an array".to_string())?
        .iter()
        .enumerate()
    {
        validate_test_history_record(record, &format!("tests.last_results.results[{index}]"))?;
    }
    Ok(())
}

fn validate_test_history_record(record: &Value, label: &str) -> Result<(), String> {
    fields(
        record,
        label,
        &[
            ("timestamp", Kind::Unsigned),
            ("codeunitId", Kind::Integer),
            ("codeunitName", Kind::String),
            ("methodName", Kind::String),
            ("status", Kind::String),
        ],
    )?;
    validate_test_status(&record["status"], &format!("{label}.status"))?;
    optional_kind(record, "durationMs", Kind::Unsigned, false)?;
    optional_kind(record, "error", Kind::String, false)
}

fn validate_test_status(value: &Value, label: &str) -> Result<(), String> {
    match value.as_str() {
        Some("pass" | "fail" | "skip") => Ok(()),
        Some(status) => Err(format!(
            "{label} must be one of 'pass', 'fail', or 'skip', got {status:?}"
        )),
        None => Err(format!("{label} must be a string")),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn clean_looking_defaults_are_not_valid_manual_responses() {
        for (method, params, value) in [
            (
                "format",
                serde_json::json!({"check": true}),
                serde_json::json!({}),
            ),
            ("parse", serde_json::json!({}), serde_json::json!({})),
            (
                "tests.run",
                serde_json::json!({}),
                serde_json::json!({"result": null}),
            ),
            (
                "debug",
                serde_json::json!({"cmd": "state"}),
                serde_json::json!({"status": "stopped"}),
            ),
        ] {
            assert!(
                validate(method, Some(&params), &value).is_err(),
                "{method} unexpectedly accepted {value}"
            );
        }
    }

    #[test]
    fn daemon_folding_and_semantic_token_shapes_are_accepted_exactly() {
        let folding = serde_json::json!([{
            "start_line": 1,
            "start_character": null,
            "end_line": 4,
            "end_character": null,
            "kind": "Region"
        }]);
        assert!(validate("foldingRanges", None, &folding).is_ok());
        assert!(
            validate(
                "foldingRanges",
                None,
                &serde_json::json!([{"startLine": 1, "endLine": 4}])
            )
            .is_err(),
            "the daemon's transport-agnostic folding shape must not drift to an unannounced LSP shape"
        );

        let tokens = serde_json::json!([{
            "deltaLine": 0,
            "deltaStart": 0,
            "length": 8,
            "tokenType": 15,
            "tokenModifiers": 0
        }]);
        assert!(validate("semanticTokens", None, &tokens).is_ok());
        assert!(
            validate("semanticTokens", None, &serde_json::json!({"data": []})).is_err(),
            "semantic token validation must match the daemon's readable token array"
        );
    }

    #[test]
    fn daemon_shutdown_acknowledgement_is_typed_and_not_a_legacy_scalar() {
        assert!(
            validate(
                "shutdown",
                None,
                &serde_json::json!({"shutdownRequested": true})
            )
            .is_ok()
        );
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!("ok"),
            serde_json::json!({}),
            serde_json::json!({"shutdownRequested": "true"}),
        ] {
            assert!(
                validate("shutdown", None, &invalid).is_err(),
                "shutdown unexpectedly accepted {invalid}"
            );
        }
    }
}
