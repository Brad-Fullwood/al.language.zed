//! Snapshot capture through the DAP, against a wiremock Business Central
//! server: attach with `"snapshot": "capture"`, setBreakpoints, the arming at
//! configurationDone, the status poll and the finish on disconnect.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use al_bc::snapshot::state::{now_secs, SessionStore};
use serde_json::{json, Value};
use tokio::sync::{watch, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::dap::framing::read_dap_body;

use super::capture::{CaptureSeams, CaptureSnappoint, CaptureState, SnappointResolver};
use super::test_support::{no_compile, TokenFut};
use super::{NativeDapState, ResolvedObject, TargetAuthorizer, VariableHandleStore};

const SOURCE: &str = "/proj/src/Poster.al";
const ZIP_BYTES: &[u8] = b"PK-synthetic-snapshot";

fn resolver() -> SnappointResolver {
    Arc::new(|path, line| {
        if path != SOURCE {
            return Err(format!("no object in {path}"));
        }
        if line == 12 {
            return Err("Line 12 holds no code that a snappoint can stop on".to_string());
        }
        Ok(CaptureSnappoint {
            object_type: 5,
            object_id: 50100,
            line0: (line - 1) as u16,
            in_package_source: false,
            package_version: None,
            alpackages_base_app: None,
        })
    })
}

struct Rig {
    server: MockServer,
    project: tempfile::TempDir,
    store: PathBuf,
    token_reads: Arc<AtomicUsize>,
    authorize_calls: Arc<AtomicUsize>,
}

#[allow(clippy::type_complexity)] // the closure types are the point of the impl Fn returns
fn build_state(
    project: &Path,
    store: &Path,
    token_reads: &Arc<AtomicUsize>,
    authorizer: TargetAuthorizer,
    poll: Duration,
) -> (
    NativeDapState<
        impl Fn(String) -> TokenFut + Send + Sync + 'static,
        impl Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync + 'static,
        impl Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
        impl Fn(PathBuf) -> super::test_support::CompileFut + Send + Sync + 'static,
        impl Fn(&Path) -> Result<Option<PathBuf>, String> + Send + Sync + 'static,
    >,
    tokio::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (cancel_tx, cancel_rx) = watch::channel(0u64);
    let (dap_event_tx, events) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let reads = token_reads.clone();
    let state = NativeDapState {
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
        project_root: project.display().to_string(),
        authorize_target: authorizer,
        acquire_token: Arc::new(move |_: String| -> TokenFut {
            reads.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok("test-token".to_string()))
        }),
        resolve_object: |_: &str, _: i64| None,
        resolve_path: |_: i32, _: i32| None,
        package_source: std::sync::Arc::new(|_, _| None),
        compile: no_compile,
        find_app: |_: &Path| Ok(None),
        replay: Default::default(),
        capture: CaptureState::new(resolver()).with_seams(CaptureSeams {
            store_dir: Some(store.to_path_buf()),
            automation_base: None,
            poll_interval: poll,
        }),
    };
    (state, events)
}

async fn rig() -> Rig {
    let project = tempfile::tempdir().unwrap();
    let store = project.path().join("store-outside-snapshots");
    Rig {
        server: MockServer::start().await,
        project,
        store,
        token_reads: Arc::new(AtomicUsize::new(0)),
        authorize_calls: Arc::new(AtomicUsize::new(0)),
    }
}

fn attach_args(rig: &Rig, extra: Value) -> Value {
    let mut args = json!({
        "request": "attach",
        "snapshot": "capture",
        "breakOnNext": "WebClient",
        "environmentType": "OnPrem",
        "authentication": "AAD",
        "tenant": "default",
        "server": "http://127.0.0.1",
        "serverInstance": "BC",
        "snapshotPort": rig.server.address().port(),
    });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    args
}

async fn mock_op(rig: &Rig, op: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("/BC/snapshotdebugger/{op}")))
        .respond_with(response)
        .mount(&rig.server)
        .await;
}

async fn send<S: SendRequest>(state: &S, command: &str, arguments: Value) -> (bool, Vec<Value>) {
    state.send(command, arguments).await
}

/// Lets the helpers take the state without naming its closure types.
trait SendRequest {
    async fn send(&self, command: &str, arguments: Value) -> (bool, Vec<Value>);
}

impl<F, Fut, R, P, C, CompileFut, A> SendRequest for NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    R: Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = Result<String, String>> + Send + 'static,
    A: Fn(&Path) -> Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    async fn send(&self, command: &str, arguments: Value) -> (bool, Vec<Value>) {
        let (mut client, server) = tokio::io::duplex(256 * 1024);
        let terminate = self
            .handle_request(&mut client, command, 7, &arguments)
            .await
            .expect("handler must not error");
        use tokio::io::AsyncWriteExt;
        client.shutdown().await.unwrap();
        drop(client);
        let mut reader = tokio::io::BufReader::new(server);
        let mut frames = Vec::new();
        while let Ok(body) = read_dap_body(&mut reader).await {
            frames.push(serde_json::from_slice(&body).unwrap());
        }
        (terminate, frames)
    }
}

fn response(frames: &[Value]) -> &Value {
    frames
        .iter()
        .find(|f| f["type"] == "response")
        .expect("a response frame")
}

fn output_text(frames: &[Value]) -> String {
    frames
        .iter()
        .filter(|f| f["event"] == "output")
        .map(|f| f["body"]["output"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("")
}

fn drain(events: &mut tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(body) = events.try_recv() {
        out.push(serde_json::from_slice(&body).unwrap());
    }
    out
}

fn saved_record_texts(store: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(store).unwrap() {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    text
}

#[tokio::test]
async fn a_capture_arms_polls_and_finishes_into_the_project_snapshots_folder() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, mut events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    mock_op(
        &rig,
        "attach",
        ResponseTemplate::new(200).set_body_string("UserSession"),
    )
    .await;
    mock_op(
        &rig,
        "status",
        ResponseTemplate::new(200).set_body_string("Started"),
    )
    .await;
    mock_op(
        &rig,
        "finish",
        ResponseTemplate::new(200).set_body_bytes(ZIP_BYTES.to_vec()),
    )
    .await;

    let (_, frames) = send(&state, "initialize", json!({})).await;
    assert!(frames.iter().any(|f| f["event"] == "initialized"));
    let (_, frames) = send(&state, "attach", attach_args(&rig, json!({}))).await;
    assert_eq!(response(&frames)["success"], true, "{frames:?}");

    let (_, frames) = send(
        &state,
        "setBreakpoints",
        json!({
            "source": { "path": SOURCE },
            "breakpoints": [ { "line": 10, "condition": "Amount > 1" }, { "line": 12 } ],
        }),
    )
    .await;
    let bps = &response(&frames)["body"]["breakpoints"];
    assert_eq!(bps[0]["verified"], true);
    assert_eq!(bps[1]["verified"], false);
    assert!(bps[1]["message"].as_str().unwrap().contains("Line 12"));

    // Nothing reaches the server before configurationDone.
    assert!(rig.server.received_requests().await.unwrap().is_empty());

    let (_, frames) = send(&state, "configurationDone", json!({})).await;
    assert_eq!(response(&frames)["success"], true, "{frames:?}");
    let text = output_text(&frames);
    assert!(text.contains("armed"), "{text}");
    assert!(text.contains("al-explorer snapshot finish"), "{text}");

    let sent = rig.server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    assert!(body.contains("\"ObjectNumber\":50100"), "{body}");
    assert!(body.contains("\"Line\":9"), "{body}");
    assert!(body.contains("Amount > 1"), "{body}");
    assert!(body.contains("\"ClientType\""), "{body}");

    // The poll reports the status once, however many polls ran.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let polled = drain(&mut events);
    let status_lines = polled
        .iter()
        .filter(|f| {
            f["body"]["output"]
                .as_str()
                .unwrap_or("")
                .contains("Snapshot status: Started")
        })
        .count();
    assert_eq!(status_lines, 1, "{polled:?}");
    assert!(reads.load(Ordering::SeqCst) >= 3, "a fresh token per call");

    // Breakpoints are fixed once armed.
    let (_, frames) = send(
        &state,
        "setBreakpoints",
        json!({ "source": { "path": SOURCE }, "breakpoints": [ { "line": 20 } ] }),
    )
    .await;
    let bp = &response(&frames)["body"]["breakpoints"][0];
    assert_eq!(bp["verified"], false);
    assert!(bp["message"].as_str().unwrap().contains("fixed once"));

    // Stepping and stack requests say a capture only records.
    for command in [
        "stackTrace",
        "continue",
        "next",
        "variables",
        "scopes",
        "pause",
    ] {
        let (_, frames) = send(&state, command, json!({})).await;
        assert_eq!(response(&frames)["success"], false, "{command}");
        assert!(response(&frames)["message"]
            .as_str()
            .unwrap()
            .contains("snapshotFileName"));
    }
    let (_, frames) = send(&state, "threads", json!({})).await;
    assert_eq!(
        response(&frames)["body"]["threads"][0]["name"],
        "Snapshot capture"
    );

    // Disconnect finishes and saves.
    let (terminate, frames) = send(&state, "disconnect", json!({})).await;
    assert!(terminate);
    let text = output_text(&frames);
    let zip = rig.project.path().join(".snapshots");
    assert!(text.contains(".snapshots"), "{text}");
    assert!(frames.iter().any(|f| f["event"] == "terminated"));
    let saved: Vec<_> = std::fs::read_dir(&zip)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let zips: Vec<_> = saved
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "zip"))
        .collect();
    assert_eq!(zips.len(), 1, "{saved:?}");
    assert_eq!(std::fs::read(zips[0]).unwrap(), ZIP_BYTES);
    assert_eq!(
        std::fs::read_to_string(zip.join(".gitignore")).unwrap(),
        "*\n"
    );

    // The record carries no token and names the zip.
    let records = saved_record_texts(&rig.store);
    assert!(!records.contains("test-token"), "{records}");
    assert!(records.contains("zipPath"), "{records}");
}

#[tokio::test]
async fn the_eight_minute_warning_is_sent_once_for_a_started_session() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, mut events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    mock_op(
        &rig,
        "attach",
        ResponseTemplate::new(200).set_body_string("UserSession"),
    )
    .await;
    mock_op(
        &rig,
        "status",
        ResponseTemplate::new(200).set_body_string("Started"),
    )
    .await;
    mock_op(&rig, "finish", ResponseTemplate::new(200)).await;
    send(&state, "attach", attach_args(&rig, json!({}))).await;
    send(
        &state,
        "setBreakpoints",
        json!({ "source": { "path": SOURCE }, "breakpoints": [ { "line": 10 } ] }),
    )
    .await;
    send(&state, "configurationDone", json!({})).await;

    // Backdate the start the way an old session looks.
    let store = SessionStore::at(&rig.store);
    let context = {
        let slot = state.capture.slot.lock().await;
        let record = slot.as_ref().unwrap().record.clone().unwrap();
        let mut record = record.lock().await;
        record.started_at = Some(now_secs() - 500);
        store.save(&record).unwrap();
        record.debugging_context.clone()
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let polled = drain(&mut events);
    let warnings = polled
        .iter()
        .filter(|f| {
            f["body"]["output"]
                .as_str()
                .unwrap_or("")
                .contains("drops a started session")
        })
        .count();
    assert_eq!(warnings, 1, "{polled:?}");
    assert!(store.load(&context).unwrap().is_some());

    let (_, frames) = send(&state, "disconnect", json!({})).await;
    assert!(output_text(&frames).contains("Nothing recorded"));
}

#[tokio::test]
async fn production_is_refused_before_any_token_is_read() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    let args = attach_args(
        &rig,
        json!({ "environmentType": "Production", "environmentName": "Live" }),
    );
    let (_, frames) = send(&state, "attach", args).await;
    assert_eq!(response(&frames)["success"], false, "{frames:?}");
    assert!(
        response(&frames)["message"]
            .as_str()
            .unwrap()
            .contains("Production"),
        "{frames:?}"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(rig.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_authorizer_refusal_comes_before_any_token_is_read() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let calls = rig.authorize_calls.clone();
    let authorizer: TargetAuthorizer = Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err("Refusing to send a cached Business Central token: not trusted".to_string())
    });
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        authorizer,
        Duration::from_millis(20),
    );
    let (_, frames) = send(&state, "attach", attach_args(&rig, json!({}))).await;
    assert_eq!(response(&frames)["success"], false);
    assert!(response(&frames)["message"]
        .as_str()
        .unwrap()
        .contains("not trusted"));
    assert_eq!(rig.authorize_calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(rig.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn breakpoints_sent_before_the_attach_are_resolved_when_it_arrives() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    mock_op(
        &rig,
        "attach",
        ResponseTemplate::new(200).set_body_string("UserSession"),
    )
    .await;
    send(
        &state,
        "setBreakpoints",
        json!({ "source": { "path": SOURCE }, "breakpoints": [ { "line": 10 }, { "line": 12 } ] }),
    )
    .await;
    let (_, frames) = send(&state, "attach", attach_args(&rig, json!({}))).await;
    let changed: Vec<_> = frames
        .iter()
        .filter(|f| f["event"] == "breakpoint")
        .collect();
    assert_eq!(changed.len(), 2, "{frames:?}");
    send(&state, "configurationDone", json!({})).await;
    let sent = rig.server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    assert!(body.contains("\"Line\":9"), "{body}");
    assert!(!body.contains("\"Line\":11"), "{body}");
}

#[tokio::test]
async fn no_snappoints_is_sent_as_an_empty_list() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_secs(60),
    );
    mock_op(
        &rig,
        "attach",
        ResponseTemplate::new(200).set_body_string("UserSession"),
    )
    .await;
    mock_op(&rig, "finish", ResponseTemplate::new(200)).await;
    send(&state, "attach", attach_args(&rig, json!({}))).await;
    let (_, frames) = send(&state, "configurationDone", json!({})).await;
    assert_eq!(response(&frames)["success"], true, "{frames:?}");
    let sent = rig.server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    assert!(body.contains("\"SourceBreakpointLocations\":[]"), "{body}");
    assert!(
        !body.contains("SnapshotVerbosity"),
        "SnapPoint is the default: {body}"
    );
    send(&state, "disconnect", json!({})).await;
}

#[tokio::test]
async fn an_output_path_outside_the_project_is_refused_at_attach() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    let outside = tempfile::tempdir().unwrap();
    let args = attach_args(
        &rig,
        json!({ "snapshotOutputPath": outside.path().to_str().unwrap() }),
    );
    let (_, frames) = send(&state, "attach", args).await;
    assert_eq!(response(&frames)["success"], false);
    assert!(response(&frames)["message"]
        .as_str()
        .unwrap()
        .contains("outside the project"));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn launch_with_the_capture_mode_and_unknown_modes_are_refused() {
    let rig = rig().await;
    let reads = rig.token_reads.clone();
    let (state, _events) = build_state(
        rig.project.path(),
        &rig.store,
        &reads,
        super::test_support::allow_every_target(),
        Duration::from_millis(20),
    );
    let (_, frames) = send(&state, "launch", attach_args(&rig, json!({}))).await;
    assert!(response(&frames)["message"]
        .as_str()
        .unwrap()
        .contains("attach"));
    let (_, frames) = send(
        &state,
        "attach",
        attach_args(&rig, json!({ "snapshot": "replay" })),
    )
    .await;
    assert!(response(&frames)["message"]
        .as_str()
        .unwrap()
        .contains("capture"));
}
