//! Black-box transport tests for the language server.
//!
//! Unlike `lsp_integration.rs` (which calls library functions directly), these
//! drive a real `tower_lsp::Server` over an in-memory duplex stream using
//! `Content-Length`-framed JSON-RPC — the same wire an editor speaks. They
//! cover the `initialize` handshake, the advertised sync kind, incremental
//! `didChange` over multi-byte content, and `shutdown`.
//!
//! Not covered here (deliberate): request paths gated by `await_ready`
//! (hover/completion/formatting/…) need a fully initialized workspace, which in
//! a transport test means real project discovery, package loading, and
//! toolchain probing. Those handlers are covered in-process, where workspace
//! readiness can be published directly.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower_lsp::{LspService, Server};

/// Write one `Content-Length`-framed JSON-RPC message.
async fn send(writer: &mut (impl AsyncWriteExt + Unpin), message: &serde_json::Value) {
    let body = message.to_string();
    let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    writer
        .write_all(frame.as_bytes())
        .await
        .expect("write frame");
    writer.flush().await.expect("flush frame");
}

/// Read one `Content-Length`-framed JSON-RPC message.
async fn receive(reader: &mut (impl AsyncReadExt + Unpin)) -> serde_json::Value {
    let mut headers = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = reader.read(&mut byte).await.expect("read header byte");
        assert_eq!(read, 1, "server closed the transport mid-header");
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(headers).expect("headers are UTF-8");
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length: ")
                .or_else(|| line.strip_prefix("content-length: "))
        })
        .expect("Content-Length header")
        .trim()
        .parse()
        .expect("numeric Content-Length");
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.expect("read body");
    serde_json::from_slice(&body).expect("JSON body")
}

/// Read messages until one matches `predicate`, or the deadline expires.
async fn receive_matching(
    reader: &mut (impl AsyncReadExt + Unpin),
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Duration::from_secs(10);
    tokio::time::timeout(deadline, async {
        loop {
            let message = receive(reader).await;
            if predicate(&message) {
                return message;
            }
        }
    })
    .await
    .expect("expected message did not arrive")
}

struct Harness {
    to_server: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    from_server: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    _server: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn start() -> Self {
        let (client_side, server_side) = tokio::io::duplex(1024 * 1024);
        let (server_read, server_write) = tokio::io::split(server_side);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (service, socket) = LspService::new(al_lsp::server::AlServer::new);
        let server = tokio::spawn(async move {
            Server::new(server_read, server_write, socket)
                .serve(service)
                .await;
        });
        Self {
            to_server: client_write,
            from_server: client_read,
            _server: server,
        }
    }
}

fn uri() -> String {
    // A path that exists nowhere: didOpen/didChange are overlay-only.
    if cfg!(windows) {
        "file:///C:/proj/Transport.Codeunit.al".to_string()
    } else {
        "file:///tmp/al-lsp-transport/Transport.Codeunit.al".to_string()
    }
}

#[tokio::test]
async fn initialize_handshake_advertises_incremental_sync_over_the_wire() {
    let mut harness = Harness::start();
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    )
    .await;

    let response = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(1))
    })
    .await;

    let sync = &response["result"]["capabilities"]["textDocumentSync"];
    assert_eq!(
        sync["change"],
        serde_json::json!(2),
        "the server must advertise TextDocumentSyncKind::INCREMENTAL (2): {response}"
    );
    assert_eq!(sync["openClose"], serde_json::json!(true));
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        serde_json::json!("al-lsp")
    );
}

#[tokio::test]
async fn incremental_edits_over_multi_byte_content_are_applied_and_rediagnosed() {
    let mut harness = Harness::start();
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    )
    .await;
    let _ = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(1))
    })
    .await;

    // The comment holds two-byte (é) and four-byte (𝄞, two UTF-16 units)
    // characters, so a byte-based column would land in the wrong place.
    let source = "codeunit 50100 Transport\n{\n    // héllo 𝄞 world\n    procedure Ok()\n    begin\n    end;\n}\n";
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri(),
                    "languageId": "al",
                    "version": 1,
                    "text": source,
                }
            }
        }),
    )
    .await;

    let opened = receive_matching(&mut harness.from_server, |message| {
        message.get("method") == Some(&serde_json::json!("textDocument/publishDiagnostics"))
    })
    .await;
    assert_eq!(opened["params"]["uri"], serde_json::json!(uri()));
    assert_eq!(
        opened["params"]["diagnostics"],
        serde_json::json!([]),
        "the opened document parses cleanly: {opened}"
    );

    // Append after the four-byte character, addressing it in UTF-16 units:
    // "    // héllo 𝄞 world" is 4 + 3 + "héllo " (6) + 𝄞 (2) = 15 units.
    let insert_at = 4 + 3 + "héllo ".encode_utf16().count() + "𝄞".encode_utf16().count();
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri(), "version": 2 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 2, "character": insert_at },
                        "end": { "line": 2, "character": insert_at },
                    },
                    "text": " — appended",
                }]
            }
        }),
    )
    .await;

    // A second (still clean) publication proves the ranged edit was accepted:
    // an out-of-bounds or mis-converted range would have been rejected with a
    // window/showMessage instead.
    let republished = receive_matching(&mut harness.from_server, |message| {
        message.get("method") == Some(&serde_json::json!("textDocument/publishDiagnostics"))
            && message["params"]["version"] == serde_json::json!(2)
    })
    .await;
    assert_eq!(
        republished["params"]["diagnostics"],
        serde_json::json!([]),
        "the incrementally edited document must still parse: {republished}"
    );

    // Now break the syntax with a second ranged edit and expect a diagnostic
    // whose range is reported in UTF-16 units on the multi-byte line.
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri(), "version": 3 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 6, "character": 0 },
                        "end": { "line": 6, "character": 1 },
                    },
                    "text": "",
                }]
            }
        }),
    )
    .await;

    let broken = receive_matching(&mut harness.from_server, |message| {
        message.get("method") == Some(&serde_json::json!("textDocument/publishDiagnostics"))
            && message["params"]["version"] == serde_json::json!(3)
    })
    .await;
    assert!(
        broken["params"]["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| !diagnostics.is_empty()),
        "removing the closing brace must produce a syntax diagnostic: {broken}"
    );
}

#[tokio::test]
async fn shutdown_is_answered_over_the_transport() {
    let mut harness = Harness::start();
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    )
    .await;
    let _ = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(1))
    })
    .await;

    send(
        &mut harness.to_server,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
    )
    .await;
    let response = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(2))
    })
    .await;
    assert!(
        response.get("error").is_none(),
        "shutdown must succeed: {response}"
    );
}

#[tokio::test]
async fn a_stale_document_version_is_rejected_without_killing_the_session() {
    let mut harness = Harness::start();
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    )
    .await;
    let _ = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(1))
    })
    .await;

    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri(),
                    "languageId": "al",
                    "version": 7,
                    "text": "codeunit 50100 Stale\n{\n}\n",
                }
            }
        }),
    )
    .await;
    let _ = receive_matching(&mut harness.from_server, |message| {
        message.get("method") == Some(&serde_json::json!("textDocument/publishDiagnostics"))
    })
    .await;

    // Version 3 is older than the opened version 7.
    send(
        &mut harness.to_server,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri(), "version": 3 },
                "contentChanges": [{ "text": "codeunit 50100 Stale\n{\n}\n" }]
            }
        }),
    )
    .await;

    let warning = receive_matching(&mut harness.from_server, |message| {
        message.get("method") == Some(&serde_json::json!("window/showMessage"))
    })
    .await;
    assert!(
        warning["params"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("rejected an edit")),
        "a stale version must be reported, not silently applied: {warning}"
    );

    // The session must still answer requests afterwards.
    send(
        &mut harness.to_server,
        &serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
    )
    .await;
    let response = receive_matching(&mut harness.from_server, |message| {
        message.get("id") == Some(&serde_json::json!(2))
    })
    .await;
    assert!(response.get("error").is_none(), "{response}");
}
