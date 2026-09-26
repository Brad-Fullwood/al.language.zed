//! The concrete `NativeDapState` the handler tests drive, and the duplex pipe
//! they read DAP frames back through.
//!
//! No stdio loop and no BC server: a handler writes into the pipe and the test
//! parses what came out with the real framing reader.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::{watch, Mutex};

use crate::dap::framing::read_dap_body;

use super::{bc_object_type, NativeDapState, ResolvedObject, VariableHandleStore};

pub(super) type TokenFut = std::future::Ready<std::result::Result<String, String>>;
pub(super) type CompileFut = std::future::Ready<std::result::Result<String, String>>;

/// The concrete `NativeDapState` specialization used across these tests:
/// plain `fn` pointers for the token / object / path hooks.
pub(super) type TestState = NativeDapState<
    fn(String) -> TokenFut,
    fn(&str) -> Option<ResolvedObject>,
    fn(i32, i32) -> Option<PathBuf>,
    fn(PathBuf) -> CompileFut,
    fn(&Path) -> std::result::Result<Option<PathBuf>, String>,
>;

pub(super) fn no_token(_tenant: String) -> TokenFut {
    std::future::ready(Err("no auth in tests".to_string()))
}

pub(super) fn no_compile(_project_root: PathBuf) -> CompileFut {
    std::future::ready(Err("no compile in handler tests".to_string()))
}

/// An authoriser that lets every target through, for the handler tests that
/// are about something other than the credential decision.
pub(super) fn allow_every_target() -> super::TargetAuthorizer {
    Arc::new(|_| Ok(()))
}

pub(super) fn test_state() -> TestState {
    let (cancel_tx, cancel_rx) = watch::channel(0u64);
    let (dap_event_tx, _dap_event_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    // Keep the receiver alive for the state's lifetime in tests that
    // never read events — dropping it would only matter for the
    // forwarder task, which these tests don't spawn.
    std::mem::forget(_dap_event_rx);
    NativeDapState {
        seq: Arc::new(AtomicU64::new(1)),
        session: Arc::new(Mutex::new(None)),
        debug_config: Arc::new(Mutex::new(None)),
        breakpoints: Arc::new(Mutex::new(HashMap::new())),
        pending_breakpoints: Arc::new(Mutex::new(HashMap::new())),
        configured: Arc::new(Mutex::new(false)),
        variable_handles: Arc::new(Mutex::new(VariableHandleStore::default())),
        cancel_tx,
        cancel_rx,
        dap_event_tx,
        project_root: "/nonexistent/test-project".to_string(),
        authorize_target: allow_every_target(),
        acquire_token: no_token,
        resolve_object: |_| None,
        resolve_path: |_, _| None,
        compile: no_compile,
        find_app: |_| Ok(None),
    }
}

/// Run one request through `handle_request` and return (terminate, frames).
pub(super) async fn run_request(
    command: &str,
    arguments: serde_json::Value,
) -> (bool, Vec<serde_json::Value>) {
    let state = test_state();
    run_request_on(&state, command, arguments).await
}

pub(super) async fn run_request_on(
    state: &TestState,
    command: &str,
    arguments: serde_json::Value,
) -> (bool, Vec<serde_json::Value>) {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let terminate = state
        .handle_request(&mut client, command, 7, &arguments)
        .await
        .expect("handler must not error");
    use tokio::io::AsyncWriteExt;
    client.shutdown().await.expect("shutdown");
    drop(client);
    let mut reader = tokio::io::BufReader::new(server);
    let mut frames: Vec<serde_json::Value> = Vec::new();
    while let Ok(body) = read_dap_body(&mut reader).await {
        frames.push(serde_json::from_slice(&body).expect("valid JSON frame"));
    }
    (terminate, frames)
}

pub(super) fn resolve_foo_al(path: &str) -> Option<ResolvedObject> {
    if path == "/proj/src/Foo.al" {
        Some(ResolvedObject {
            object_type: bc_object_type::CODEUNIT,
            object_id: 50100,
        })
    } else {
        None
    }
}
