//! Black-box smoke test for the MCP server surface (`al-lsp mcp`) — the tools
//! Zed's agent panel talks to. This is the only Rust coverage of the MCP
//! transport (the crate's `LspClient` covers LSP; this fills the gap that the
//! former Node `mcp_probe.mjs` covered).

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

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "smoke", "version": "0"}}
        }),
    )
    .await;
    let init = read_until_id(&mut reader, 1).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"], "al-lsp",
        "initialize: {init}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let listed = read_until_id(&mut reader, 2).await;

    let tools: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    for expected in ["al_build", "al_symbolsearch", "al_deadcode"] {
        assert!(
            tools.iter().any(|t| t == expected),
            "MCP tool `{expected}` missing; got {tools:?}"
        );
    }

    child.start_kill().ok();
}
