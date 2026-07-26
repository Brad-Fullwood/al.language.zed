//! Black-box smoke tests for the MCP server surface (`al-lsp mcp`) used by
//! Zed's agent panel.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use al_test_harness::{al_lsp_binary, test_project_dir};

async fn send(stdin: &mut ChildStdin, v: Value) {
    stdin.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_until_id(reader: &mut Lines<BufReader<ChildStdout>>, id: i64) -> Value {
    timeout(Duration::from_secs(30), async {
        loop {
            match reader.next_line().await.expect("read mcp stdout") {
                None => panic!("mcp stdout closed before response id {id}"),
                Some(line) if line.trim().is_empty() => continue,
                Some(line) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(Value::as_i64) == Some(id) {
                            return v;
                        }
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for mcp response id {id}"))
}

/// Complete the MCP initialization handshake used by every black-box session.
///
/// Keeping the request and required `notifications/initialized` notification
/// together prevents individual tool tests from accidentally exercising an
/// invalid half-initialized lifecycle.
async fn initialize_session(
    stdin: &mut ChildStdin,
    reader: &mut Lines<BufReader<ChildStdout>>,
) -> Value {
    send(
        stdin,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "smoke", "version": "0"}}
        }),
    )
    .await;
    let initialized = read_until_id(reader, 1).await;
    assert!(
        initialized.get("result").is_some(),
        "MCP initialize failed: {initialized}"
    );
    send(
        stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    initialized
}

#[tokio::test]
async fn mcp_initialize_and_list_tools() {
    let mut child = Command::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(test_project_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `al-lsp mcp`");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();

    let init = initialize_session(&mut stdin, &mut reader).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"], "al-lsp",
        "initialize: {init}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let listed = read_until_id(&mut reader, 2).await;

    let tool_objs = listed["result"]["tools"].as_array().expect("tools array");
    let tools: Vec<String> = tool_objs
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    // The official-surface staples plus this project's agent tools.
    for expected in [
        "al_call",
        "al_debug",
        "al_build",
        "al_symbolsearch",
        "al_deadcode",
        "al_suggestevent",
        "al_testclassify",
        "al_testcoverage",
        "al_depgraph",
    ] {
        assert!(
            tools.iter().any(|t| t == expected),
            "MCP tool `{expected}` missing; got {tools:?}"
        );
    }

    // Every advertised tool must carry a non-empty description and a
    // well-formed JSON Schema whose `required` fields are all declared in
    // `properties` (else an agent cannot satisfy the contract).
    for t in tool_objs {
        let name = t["name"].as_str().expect("tool name");
        assert!(
            t["description"]
                .as_str()
                .is_some_and(|d| !d.trim().is_empty()),
            "tool `{name}` has an empty description"
        );
        let schema = &t["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "tool `{name}` schema not an object"
        );
        let props = schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("tool `{name}` schema missing `properties`"));
        let required = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("tool `{name}` schema missing `required` array"));
        for field in required {
            let field = field
                .as_str()
                .unwrap_or_else(|| panic!("tool `{name}` required entry not a string"));
            assert!(
                props.contains_key(field),
                "tool `{name}` requires `{field}` but does not declare it in properties"
            );
        }
    }

    child.start_kill().ok();
}

/// The generic bridge is what makes the complete dispatcher available to MCP
/// without requiring every method to gain a second, hand-maintained registry
/// entry. Exercise a method that intentionally has no named MCP alias.
#[tokio::test]
async fn mcp_al_call_reaches_the_complete_dispatcher() {
    let mut child = Command::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(test_project_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `al-lsp mcp`");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();

    let _ = initialize_session(&mut stdin, &mut reader).await;

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "al_call",
                "arguments": {"method": "rules", "params": {}}
            }
        }),
    )
    .await;
    let resp = read_until_id(&mut reader, 3).await;
    assert_eq!(resp["result"]["isError"], false, "al_call errored: {resp}");

    child.start_kill().ok();
}

/// Prove that an MCP client can drive the stateful debug dispatcher through
/// the real stdio server. `stop` is deterministic without a live BC instance;
/// the live operations use this same tool and retained Workspace session.
#[tokio::test]
async fn mcp_al_debug_reaches_the_debug_control_plane() {
    let mut child = Command::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(test_project_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `al-lsp mcp`");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();

    let _ = initialize_session(&mut stdin, &mut reader).await;

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "al_debug", "arguments": {"cmd": "stop"}}
        }),
    )
    .await;
    let resp = read_until_id(&mut reader, 4).await;
    assert_eq!(resp["result"]["isError"], false, "al_debug errored: {resp}");
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("debug text content");
    let payload: Value = serde_json::from_str(text).expect("debug output is JSON");
    assert_eq!(payload["cmd"], "stop");
    assert_eq!(payload["status"], "no active debug session");

    child.start_kill().ok();
}

/// Mirror of `al_test::router::RoutingDecision::runs_locally` for the wire form
/// — only `interp` actually executes on the built-in interpreter today; the
/// other decisions route to live Business Central. Kept in lock-step with the
/// router (the harness crate does not depend on `al-test`).
fn decision_runs_locally(decision: &str) -> bool {
    decision == "interp"
}

/// The `al_testclassify` tool must surface, per discovered
/// test, whether it runs locally vs needs BC. The bundled fixture ships a
/// pure-logic test codeunit (`PureLogicTest.Codeunit.al`), so at least one
/// method must classify as locally runnable.
#[tokio::test]
async fn mcp_testclassify_reports_local_vs_bc_routing() {
    let mut child = Command::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(test_project_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `al-lsp mcp`");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();

    let _ = initialize_session(&mut stdin, &mut reader).await;

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "al_testclassify", "arguments": {}}
        }),
    )
    .await;
    let resp = read_until_id(&mut reader, 5).await;
    assert_eq!(resp["result"]["isError"], false, "classify errored: {resp}");

    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("classify text content");
    let payload: Value = serde_json::from_str(text).expect("classify output is JSON");
    let classifications = payload["classifications"]
        .as_array()
        .expect("classifications array");
    assert!(
        !classifications.is_empty(),
        "fixture ships a test codeunit, so classifications must be non-empty: {text}"
    );

    let known = ["interp", "interpRecord", "liveBc", "snapshot"];
    let mut saw_local = false;
    for c in classifications {
        let decision = c["decision"].as_str().expect("each test has a decision");
        assert!(
            known.contains(&decision),
            "unknown routing decision `{decision}`"
        );
        saw_local |= decision_runs_locally(decision);
    }
    assert!(
        saw_local,
        "the pure-logic fixture test must classify as locally runnable: {text}"
    );

    child.start_kill().ok();
}
