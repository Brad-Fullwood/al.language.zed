use super::*;

#[test]
fn a_search_from_an_agent_returns_summaries_unless_it_asks_for_members() {
    let defaulted = apply_agent_defaults("search", serde_json::json!({ "query": "Customer" }));
    assert_eq!(defaulted["summary"], true);
    let explicit = apply_agent_defaults(
        "search",
        serde_json::json!({ "query": "Customer", "summary": false }),
    );
    assert_eq!(explicit["summary"], false);
    let other = apply_agent_defaults("object", serde_json::json!({ "name": "Customer" }));
    assert!(other.get("summary").is_none());
}

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
        launch_config_error: None,
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
    // A list tool advertises the projection envelope, not a bare array:
    // MCP calls carry a default `limit`, so `total` and `truncated` are
    // always part of the answer.
    let search_result = &search["outputSchema"]["properties"]["result"];
    assert_eq!(search_result["type"], "object");
    assert_eq!(search_result["properties"]["items"]["type"], "array");
    for counter in ["total", "returned", "offset"] {
        assert_eq!(
            search_result["properties"][counter]["type"], "integer",
            "a list tool must advertise {counter}: {search_result}"
        );
    }
    assert_eq!(search_result["properties"]["truncated"]["type"], "boolean");
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
    // MCP calls carry a default `limit`, so a list arrives as the
    // projection envelope rather than a bare array.
    assert!(
        resp["result"]["structuredContent"]["result"]["items"].is_array(),
        "structured result must preserve the daemon JSON: {resp}"
    );
    assert_eq!(resp["result"]["structuredContent"]["result"]["total"], 1);
    assert_eq!(
        resp["result"]["structuredContent"]["result"]["truncated"],
        false
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

/// The allocator's whole point is one call that an agent can read inline,
/// so the tool call is pinned against the fixture project end to end.
#[tokio::test(flavor = "multi_thread")]
async fn tools_call_freeids_returns_the_next_free_id_for_a_kind() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../al-test-harness/data/test_al_project");
    let workspace = ws();
    al_workspace::initialize_core_workspace(&workspace, &root)
        .await
        .expect("fixture project must initialize");

    let resp = handle_mcp_message(
        &workspace,
        &Notify::new(),
        serde_json::json!({
            "jsonrpc":"2.0","id":31,"method":"tools/call",
            "params": {"name": "al_freeids", "arguments": {"kind": "table"}}
        }),
    )
    .await
    .expect("response");
    assert_eq!(resp["result"]["isError"], false, "resp: {resp}");
    let result = &resp["result"]["structuredContent"]["result"];
    assert_eq!(result["nextFree"].as_i64(), Some(50101), "{result}");
    validate_schema_value(result, &result_schema("al_freeids"), "result")
        .expect("free-ids output must match its advertised result schema");

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(
        text.len() < 400,
        "an agent must be able to read this inline: {} bytes, {text}",
        text.len()
    );
}

#[tokio::test]
async fn tools_call_freeids_rejects_an_unknown_kind_at_the_schema() {
    let resp = handle_mcp_message(
        &ws(),
        &Notify::new(),
        serde_json::json!({
            "jsonrpc":"2.0","id":32,"method":"tools/call",
            "params": {"name": "al_freeids", "arguments": {"kind": "tabel"}}
        }),
    )
    .await
    .expect("response");
    let message = resp["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a misspelt kind must be refused: {resp}"));
    assert!(message.contains("arguments.kind"), "{message}");
    assert!(message.contains("tableextension"), "{message}");
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

/// A named tool must accept exactly what the method behind it accepts.
/// `al_getdiagnostics` rejected `text` at its schema while `al_call` with
/// method `lint` took it, so an agent holding an unsaved buffer could lint
/// it only through the generic entry point. The catalog drives the check,
/// so a tool added over a document-reading method is covered on arrival.
#[test]
fn every_document_tool_accepts_the_document_arguments_its_method_takes() {
    for tool in tools() {
        let schema = tool_schema(tool);
        let properties = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{}: schema must declare `properties`", tool.name));
        if !crate::server::daemon::reads_document(tool.method) {
            assert!(
                !properties.contains_key("text"),
                "{}: `text` is only for a method that reads a document through the \
                 shared document parameters",
                tool.name
            );
            continue;
        }
        for argument in ["uri", "file", "text"] {
            assert!(
                properties.contains_key(argument),
                "{}: `{argument}` reaches method `{}`, so the schema must advertise it",
                tool.name,
                tool.method
            );
        }
        // `text` stands for a file the daemon may not open, and it needs
        // the path it stands for, so the pair has to pass validation.
        validate_tool_arguments(
            tool,
            &serde_json::json!({"file": "/outside/Unsaved.al", "text": "codeunit 1 X {}"}),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{}: supplying unsaved text was rejected: {error}",
                tool.name
            )
        });
        // Either spelling of the path identifies the document on its own.
        validate_tool_arguments(tool, &serde_json::json!({"uri": "file:///tmp/X.al"}))
            .unwrap_or_else(|error| panic!("{}: `uri` was rejected: {error}", tool.name));
    }
}

#[test]
fn mcp_reference_lists_exactly_the_named_tool_registry() {
    let registered = tools()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let reference = include_str!("../../../../../Docs/reference/mcp-tools.md");
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
        "al_freeids",
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
