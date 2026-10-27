//! Journey test, stage 10: sends one request to al-lsp over MCP, LSP or the
//! project daemon and prints the response.
//!
//! Usage: `journey-rpc --mode mcp|lsp|daemon --project DIR --method NAME
//!         [--params JSON] [--out FILE]`
//!
//! - `mcp`: starts `al-lsp mcp --project DIR`, runs the initialize handshake, then
//!   `tools/call` with `name = NAME` and `arguments = JSON` (an object). The method
//!   `tools/list` lists the tools instead.
//! - `lsp`: starts al-lsp on DIR through `LspClient`. A NAME with a `/` is sent as that
//!   LSP request with JSON as its params. Any other NAME is a `workspace/executeCommand`
//!   command and JSON is its `arguments` array. When the first argument has a `uri` inside
//!   DIR, that file is opened first.
//! - `daemon`: connects to the project daemon with `DaemonClient` (starting one when none
//!   runs) and sends NAME with JSON as params. Reached through the same protocol client
//!   that al-explorer uses.
//!
//! The response is printed to stdout and written to `--out`. Exit 0 for a response
//! without an error, 1 for an error response or a tool result with `isError`, 2 for bad
//! usage, 3 when the request timed out. The al-lsp binary is read from `AL_LSP_BIN`.

use std::process::Stdio;
use std::time::Duration;

use al_protocol::DaemonClient;
use al_test_harness::{al_lsp_binary, LspClient};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const REQUEST_SECS: u64 = 50;

struct Args {
    mode: String,
    project: String,
    method: String,
    params: Value,
    out: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: journey-rpc --mode mcp|lsp|daemon --project DIR --method NAME [--params JSON] [--out FILE]"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| -> Option<String> {
        argv.iter()
            .position(|a| a == name)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };
    let (Some(mode), Some(project), Some(method)) =
        (get("--mode"), get("--project"), get("--method"))
    else {
        usage();
    };
    let raw = get("--params").unwrap_or_else(|| "{}".to_string());
    let params: Value = serde_json::from_str(&raw).unwrap_or_else(|error| {
        eprintln!("--params is not JSON: {error}");
        std::process::exit(2);
    });
    Args {
        mode,
        project,
        method,
        params,
        out: get("--out"),
    }
}

/// Outcome of one request: the JSON to print and whether it counts as a success.
struct Outcome {
    value: Value,
    ok: bool,
}

async fn run_mcp(args: &Args) -> Result<Outcome, String> {
    let mut child = Command::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(&args.project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn al-lsp mcp: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();

    async fn send(stdin: &mut tokio::process::ChildStdin, v: Value) -> Result<(), String> {
        stdin
            .write_all(format!("{v}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }
    async fn read_id(
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        id: i64,
    ) -> Result<Value, String> {
        loop {
            match lines.next_line().await.map_err(|e| e.to_string())? {
                None => return Err(format!("mcp stdout closed before response {id}")),
                Some(line) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(Value::as_i64) == Some(id) {
                            return Ok(v);
                        }
                    }
                }
            }
        }
    }

    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                          "clientInfo": {"name": "journey-rpc", "version": "0"}}}),
    )
    .await?;
    let init = read_id(&mut lines, 1).await?;
    if init.get("result").is_none() {
        return Ok(Outcome {
            value: init,
            ok: false,
        });
    }
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await?;
    let request = if args.method == "tools/list" {
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    } else {
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": args.method, "arguments": args.params}})
    };
    send(&mut stdin, request).await?;
    let response = read_id(&mut lines, 2).await?;
    let ok =
        response.get("error").is_none() && response["result"]["isError"].as_bool() != Some(true);
    Ok(Outcome {
        value: response,
        ok,
    })
}

async fn run_lsp(args: &Args) -> Result<Outcome, String> {
    let mut client = LspClient::spawn(&args.project)
        .await
        .map_err(|e| format!("start al-lsp: {e}"))?;
    let result = if args.method.contains('/') {
        client.raw_request(&args.method, args.params.clone()).await
    } else {
        let arguments = if args.params.is_array() {
            args.params.clone()
        } else {
            json!([args.params])
        };
        // The lens commands read the document the editor has open, so a call
        // whose first argument names a project file opens that file first.
        let opened = arguments
            .get(0)
            .and_then(|arg| arg.get("uri"))
            .and_then(|uri| uri.as_str())
            .and_then(|uri| uri.strip_prefix("file://"))
            .and_then(|path| std::path::Path::new(path).strip_prefix(&args.project).ok())
            .map(|relative| relative.to_string_lossy().into_owned());
        if let Some(relative) = opened {
            if let Ok(text) =
                std::fs::read_to_string(std::path::Path::new(&args.project).join(&relative))
            {
                client
                    .open_file_within(&relative, &text, Duration::from_secs(30))
                    .await;
            }
        }
        client
            .execute_command_answering_requests(&args.method, arguments)
            .await
    };
    client.shutdown().await;
    Ok(match result {
        Ok(value) => Outcome { value, ok: true },
        Err(error) => Outcome {
            value: json!({"error": error.to_string()}),
            ok: false,
        },
    })
}

fn run_daemon(args: &Args) -> Result<Outcome, String> {
    let mut client = DaemonClient::connect(std::path::Path::new(&args.project))?;
    client.set_request_timeout(Duration::from_secs(REQUEST_SECS));
    let params = if args.params.is_null() {
        None
    } else {
        Some(args.params.clone())
    };
    Ok(match client.request(&args.method, params) {
        Ok(value) => Outcome { value, ok: true },
        Err(error) => Outcome {
            value: json!({"error": error}),
            ok: false,
        },
    })
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let work = async {
        match args.mode.as_str() {
            "mcp" => run_mcp(&args).await,
            "lsp" => run_lsp(&args).await,
            "daemon" => {
                let owned = Args {
                    mode: args.mode.clone(),
                    project: args.project.clone(),
                    method: args.method.clone(),
                    params: args.params.clone(),
                    out: None,
                };
                tokio::task::spawn_blocking(move || run_daemon(&owned))
                    .await
                    .map_err(|e| e.to_string())?
            }
            _ => usage(),
        }
    };
    let outcome = match timeout(Duration::from_secs(REQUEST_SECS + 5), work).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            eprintln!("journey-rpc: {error}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("journey-rpc: timed out after {REQUEST_SECS}s");
            std::process::exit(3);
        }
    };
    let text = serde_json::to_string_pretty(&outcome.value).unwrap_or_default();
    println!("{text}");
    if let Some(path) = &args.out {
        let _ = std::fs::write(path, &text);
    }
    std::process::exit(if outcome.ok { 0 } else { 1 });
}
