//! Tests for the replay: the engine on synthetic contexts, the DAP session on
//! a synthetic archive, and parity with Microsoft's adapter on real
//! snapshots (ignored, they need `AL_SNAPSHOT_FIXTURE_DIR`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

use al_bc::snapshot::archive::{
    ExceptionContext, FormatVersion, LineContext, MethodContext, ObjectId, ObjectType,
    SnapshotArchive, SourcePosition, SourceSpan, StackFrame, Variable,
};
use serde_json::{json, Value};

use super::engine::{Engine, Pos, Step};
use super::*;
use crate::dap::native_dap::test_support::{run_request_on, test_state, TestState};

fn span(line: u16) -> SourceSpan {
    SourceSpan {
        from: SourcePosition { line, column: 8 },
        to: SourcePosition { line, column: 40 },
    }
}

fn id() -> ObjectId {
    ObjectId {
        object_type: ObjectType::CodeUnit,
        number: 50000,
    }
}

fn var(name: &str, value: &str) -> Variable {
    Variable {
        name: name.into(),
        value: value.into(),
        type_name: "Integer".into(),
        ..Variable::default()
    }
}

/// A context for `method` that ran `lines` (0-based), called from `callers`
/// (innermost first, each with its call site line). Lines in `with_vars`
/// carry a local variable.
fn ctx(
    index: usize,
    method: &str,
    callers: &[(&str, u16)],
    lines: &[u16],
    with_vars: &[u16],
) -> MethodContext {
    let mut frames = vec![StackFrame {
        method_name: method.into(),
        id: id(),
        object_name: "Probe".into(),
        statement_span: span(lines[0]),
    }];
    for (m, l) in callers {
        frames.push(StackFrame {
            method_name: (*m).into(),
            id: id(),
            object_name: "Probe".into(),
            statement_span: span(*l),
        });
    }
    MethodContext {
        index,
        id: id(),
        method_name: method.into(),
        frames,
        line_contexts: lines
            .iter()
            .enumerate()
            .map(|(i, l)| LineContext {
                ordinal: i as i32,
                statement_span: span(*l),
                variables: if with_vars.contains(l) {
                    vec![var("Counter", &l.to_string())]
                } else {
                    Vec::new()
                },
                timestamp: (index * 1000 + i * 10) as i64,
            })
            .collect(),
        ..MethodContext::default()
    }
}

/// Main (10, 11, 12) calls Sub (20, 21) from line 11. Other (30, 31) is a
/// second entry method with a variable on line 30.
fn sample() -> Vec<MethodContext> {
    let mut sub = ctx(1, "Sub", &[("Main", 11)], &[20, 21], &[]);
    sub.execution_span.start_time = 1005;
    vec![
        ctx(0, "Main", &[], &[10, 11, 12], &[]),
        sub,
        ctx(2, "Other", &[], &[30, 31], &[30]),
    ]
}

fn at(step: Step) -> (usize, usize) {
    match step {
        Step::Stopped(l) => (l.pos.ctx, l.pos.line),
        Step::Ended => panic!("replay ended"),
    }
}

#[test]
fn calls_are_linked_to_their_call_site() {
    let e = Engine::new(&sample());
    assert_eq!(
        e.stack(Pos { ctx: 1, line: 1 }),
        vec![Pos { ctx: 0, line: 1 }, Pos { ctx: 1, line: 1 }]
    );
    assert_eq!(
        e.stack(Pos { ctx: 2, line: 0 }),
        vec![Pos { ctx: 2, line: 0 }]
    );
}

#[test]
fn without_breakpoints_the_walk_starts_at_the_first_statement() {
    let mut e = Engine::new(&[ctx(0, "Main", &[], &[10, 11], &[])]);
    let Step::Stopped(l) = e.start() else {
        panic!("no stop")
    };
    assert!(l.start_over);
    assert_eq!(l.pos, Pos { ctx: 0, line: 0 });
    assert_eq!(at(e.next()), (0, 1));
    // The last statement of context 0 ends the replay.
    assert_eq!(e.next(), Step::Ended);
}

#[test]
fn start_runs_to_the_first_snappoint_and_continue_ends() {
    let mut e = Engine::new(&sample());
    assert_eq!(e.snappoints().len(), 1);
    assert_eq!(at(e.start()), (2, 0));
    assert_eq!(e.cont(), Step::Ended);
}

#[test]
fn next_steps_over_calls_and_step_in_out_walk_through_them() {
    let mut e = Engine::new(&sample());
    e.start_over();
    assert_eq!(at(e.next()), (0, 1));
    assert_eq!(at(e.step_in()), (1, 0));
    assert_eq!(at(e.next()), (1, 1));
    // Leaving the callee goes to the statement after the call.
    assert_eq!(at(e.step_out()), (0, 2));
    assert_eq!(e.next(), Step::Ended);

    // next over the call site does not enter the callee.
    let mut e = Engine::new(&sample());
    e.start_over();
    e.next();
    assert_eq!(at(e.next()), (0, 2));
}

#[test]
fn next_off_the_end_of_a_later_entry_method_starts_over() {
    let mut e = Engine::new(&sample());
    assert_eq!(at(e.start()), (2, 0));
    assert_eq!(at(e.next()), (2, 1));
    let Step::Stopped(l) = e.next() else {
        panic!("no restart")
    };
    assert!(l.start_over);
    assert_eq!(l.pos, Pos { ctx: 0, line: 0 });
}

#[test]
fn step_out_of_a_callee_that_ends_its_caller_returns_further() {
    let mut e = Engine::new(&sample());
    e.start_over();
    e.next();
    e.step_in();
    e.next();
    // Callee's last statement: next returns to the caller's following statement.
    assert_eq!(at(e.next()), (0, 2));
}

#[test]
fn step_in_without_a_call_stays_on_the_statement() {
    let mut e = Engine::new(&sample());
    e.start_over();
    assert_eq!(at(e.step_in()), (0, 0));
    assert_eq!(at(e.step_in()), (0, 0));
}

#[test]
fn step_in_without_a_call_in_the_last_context_ends() {
    let mut e = Engine::new(&sample());
    e.start_over();
    // The last context has no later context to hold a callee.
    let mut e2 = Engine::new(&sample()[2..]);
    e2.start_over();
    assert!(matches!(e2.step_in(), Step::Ended));
    assert_eq!(at(e.step_in()), (0, 0));
}

#[test]
fn user_breakpoints_stop_continue_once_per_occurrence() {
    // Line 11 runs in two contexts.
    let ctxs = vec![
        ctx(0, "Main", &[], &[10, 11], &[]),
        ctx(1, "Main", &[], &[10, 11], &[]),
    ];
    let mut e = Engine::new(&ctxs);
    let r = e.set_user_breakpoints("a.al", 5, 50000, &[12, 99]);
    assert!(r[0].verified);
    assert!(!r[1].verified);
    assert_eq!(at(e.start()), (0, 1));
    assert_eq!(at(e.cont()), (1, 1));
    assert_eq!(e.cont(), Step::Ended);
}

#[test]
fn a_breakpoint_set_late_ignores_statements_already_passed() {
    let ctxs = vec![
        ctx(0, "Main", &[], &[10, 11], &[]),
        ctx(1, "Main", &[], &[10, 11], &[]),
    ];
    let mut e = Engine::new(&ctxs);
    e.start();
    e.next();
    e.set_user_breakpoints("a.al", 5, 50000, &[11]);
    // Line 10 was passed in context 0, so only context 1 is left.
    assert_eq!(at(e.cont()), (1, 0));
}

#[test]
fn an_exception_statement_is_a_snappoint_and_flagged() {
    let mut c = ctx(0, "Main", &[], &[10, 11, 12], &[]);
    c.exception = Some(ExceptionContext {
        message: "Boom".into(),
        line_number: 1,
        statement_span: span(11),
    });
    let mut e = Engine::new(&[c]);
    let Step::Stopped(l) = e.start() else {
        panic!("no stop")
    };
    assert!(l.exception);
    assert_eq!(l.pos, Pos { ctx: 0, line: 1 });
}

// ---------------------------------------------------------------------------
// Session on a synthetic archive
// ---------------------------------------------------------------------------

fn archive() -> SnapshotArchive {
    let mut sources = HashMap::new();
    sources.insert((ObjectType::CodeUnit, 50000), "// source\n".to_string());
    SnapshotArchive::from_parts(
        FormatVersion {
            major: 3,
            minor: 0,
            build: 0,
            revision: 0,
        },
        sample(),
        sources,
    )
}

fn no_resolve(_: &str, _: i64) -> Option<ResolvedObject> {
    None
}

struct Probe {
    s: ReplaySession,
    seq: AtomicU64,
}

impl Probe {
    fn new() -> Self {
        let a = archive();
        let root = PathBuf::from("/cache/snapshot-sources");
        let mut files = HashMap::new();
        files.insert(
            (ObjectType::CodeUnit, 50000),
            root.join("z").join("CodeUnit%50000.al"),
        );
        Probe {
            s: ReplaySession::new(a, files, root),
            seq: AtomicU64::new(1),
        }
    }

    fn req(&mut self, command: &str, args: Value) -> Vec<Value> {
        self.s
            .handle(&self.seq, 1, command, &args, &no_resolve)
            .expect("handled")
            .messages
    }
}

fn events<'a>(msgs: &'a [Value], name: &str) -> Vec<&'a Value> {
    msgs.iter()
        .filter(|m| m["event"] == name)
        .collect::<Vec<_>>()
}

#[test]
fn configuration_done_runs_to_the_first_snappoint() {
    let mut p = Probe::new();
    let m = p.req("configurationDone", json!({}));
    assert_eq!(m[0]["success"], true);
    let stopped = events(&m, "stopped");
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0]["body"]["reason"], "step");
    let st = p.req("stackTrace", json!({"threadId": 1}));
    let f = &st[0]["body"]["stackFrames"][0];
    assert_eq!(f["name"], "Other");
    assert_eq!(f["line"], 31);
    assert_eq!(f["column"], 9);
    assert_eq!(f["source"]["name"], "Probe");
    assert_eq!(st[0]["body"]["totalFrames"], 1);
}

#[test]
fn scopes_and_variables_follow_the_oracle_shape() {
    let mut p = Probe::new();
    p.req("configurationDone", json!({}));
    let sc = p.req("scopes", json!({"frameId": 0}));
    let scopes = &sc[0]["body"]["scopes"];
    assert_eq!(scopes[0]["name"], "Globals");
    assert_eq!(scopes[0]["variablesReference"], 65536);
    assert_eq!(scopes[1]["name"], "Locals");
    assert_eq!(scopes[1]["variablesReference"], 131072);
    assert_eq!(scopes[2]["name"], "Database Statistics");
    assert_eq!(scopes[2]["variablesReference"], 196608);
    let v = p.req("variables", json!({"variablesReference": 131072}));
    assert_eq!(v[0]["body"]["variables"][0]["name"], "Counter");
    assert_eq!(v[0]["body"]["variables"][0]["value"], "30");
    // Next lands on a statement without variables.
    p.req("next", json!({}));
    let v = p.req("variables", json!({"variablesReference": 131072}));
    assert_eq!(v[0]["body"]["variables"][0]["name"], "EmptyNodeMessage");
    assert_eq!(
        v[0]["body"]["variables"][0]["value"],
        "Variable information is only available on snappoints."
    );
    let v = p.req("variables", json!({"variablesReference": 196608}));
    assert_eq!(v[0]["body"]["variables"].as_array().unwrap().len(), 0);
}

#[test]
fn nested_records_get_handles_from_262144() {
    let mut c = ctx(0, "Main", &[], &[10], &[]);
    c.line_contexts[0].variables = vec![Variable {
        name: "Rec".into(),
        value: "Order".into(),
        type_name: "Table X (1)".into(),
        has_children: true,
        children: vec![Variable {
            name: "Fields".into(),
            value: "Count = 1".into(),
            has_children: true,
            children: vec![var("No.", "1")],
            ..Variable::default()
        }],
    }];
    let a = SnapshotArchive::from_parts(
        FormatVersion {
            major: 3,
            minor: 0,
            build: 0,
            revision: 0,
        },
        vec![c],
        HashMap::new(),
    );
    let mut s = ReplaySession::new(a, HashMap::new(), PathBuf::from("/x"));
    let seq = AtomicU64::new(1);
    let mut req =
        |cmd: &str, args: Value| s.handle(&seq, 1, cmd, &args, &no_resolve).unwrap().messages;
    req("configurationDone", json!({}));
    let v = req("variables", json!({"variablesReference": 131072}));
    assert_eq!(v[0]["body"]["variables"][0]["variablesReference"], 262144);
    let v = req("variables", json!({"variablesReference": 262144}));
    assert_eq!(v[0]["body"]["variables"][0]["name"], "Fields");
    assert_eq!(v[0]["body"]["variables"][0]["variablesReference"], 262145);
    let v = req("variables", json!({"variablesReference": 262145}));
    assert_eq!(v[0]["body"]["variables"][0]["name"], "No.");
    let e = req("evaluate", json!({"expression": "rec"}));
    assert_eq!(e[0]["body"]["result"], "Order");
}

#[test]
fn the_end_sends_output_then_terminated_and_nothing_after() {
    let mut p = Probe::new();
    p.req("configurationDone", json!({}));
    let m = p.req("continue", json!({}));
    assert_eq!(m[0]["body"]["allThreadsContinued"], true);
    let out = events(&m, "output");
    assert_eq!(out[0]["body"]["output"], "No more frames are available.\n");
    assert_eq!(m.last().unwrap()["event"], "terminated");
    assert!(events(&m, "stopped").is_empty());
    let st = p.req("stackTrace", json!({}));
    assert_eq!(st[0]["body"]["stackFrames"].as_array().unwrap().len(), 0);
}

#[test]
fn snappoint_event_lines_are_zero_based() {
    let p = Probe::new();
    let e = p.s.snappoints_event(&p.seq);
    let b = &e["body"]["breakpoints"][0];
    assert_eq!(e["event"], "al/updateSnappointsEvent");
    assert_eq!(b["line"], 30);
    assert_eq!(b["source"]["name"], "CodeUnit%50000");
    assert_eq!(b["id"], 268_439_396);
}

#[test]
fn breakpoints_in_snapshot_sources_map_back_by_file_name() {
    let mut p = Probe::new();
    let path = "/cache/snapshot-sources/z/CodeUnit%50000.al";
    let m = p.req(
        "setBreakpoints",
        json!({"source": {"path": path}, "breakpoints": [{"line": 12}, {"line": 40}]}),
    );
    let b = &m[0]["body"]["breakpoints"];
    assert_eq!(b[0]["verified"], true);
    assert_eq!(b[0]["id"], 268_439_397);
    assert_eq!(b[1]["verified"], false);
    assert_eq!(b[1]["id"], 0);
}

#[test]
fn breakpoints_elsewhere_go_through_the_workspace_resolver() {
    let mut p = Probe::new();
    let resolve = |path: &str, _l: i64| {
        (path == "/ws/Probe.al").then_some(ResolvedObject {
            object_type: 5,
            object_id: 50000,
        })
    };
    let r =
        p.s.handle(
            &p.seq,
            1,
            "setBreakpoints",
            &json!({"source": {"path": "/ws/Probe.al"}, "breakpoints": [{"line": 11}]}),
            &resolve,
        )
        .unwrap();
    assert_eq!(r.messages[0]["body"]["breakpoints"][0]["verified"], true);
}

#[test]
fn an_exception_stop_names_the_message_and_fills_alexception() {
    let mut c = ctx(0, "Main", &[], &[10, 11], &[]);
    c.exception = Some(ExceptionContext {
        message: "Boom".into(),
        line_number: 1,
        statement_span: span(11),
    });
    let a = SnapshotArchive::from_parts(
        FormatVersion {
            major: 3,
            minor: 0,
            build: 0,
            revision: 0,
        },
        vec![c],
        HashMap::new(),
    );
    let mut s = ReplaySession::new(a, HashMap::new(), PathBuf::from("/x"));
    let seq = AtomicU64::new(1);
    let m = s
        .handle(&seq, 1, "configurationDone", &json!({}), &no_resolve)
        .unwrap()
        .messages;
    let stopped = events(&m, "stopped");
    assert_eq!(stopped[0]["body"]["reason"], "exception");
    assert_eq!(stopped[0]["body"]["text"], "Boom");
    let v = s
        .handle(
            &seq,
            1,
            "variables",
            &json!({"variablesReference": 131072}),
            &no_resolve,
        )
        .unwrap()
        .messages;
    assert_eq!(v[0]["body"]["variables"][0]["name"], "_ALException");
    assert_eq!(v[0]["body"]["variables"][0]["value"], "Boom");
}

#[test]
fn disconnect_ends_the_replay() {
    let mut p = Probe::new();
    p.req("configurationDone", json!({}));
    let r =
        p.s.handle(&p.seq, 1, "disconnect", &json!({}), &no_resolve)
            .unwrap();
    assert!(r.done);
    assert_eq!(r.messages.last().unwrap()["event"], "terminated");
}

// ---------------------------------------------------------------------------
// Sources and launch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sources_are_written_read_only_and_a_planted_link_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    let snap = dir.path().join("snap");
    std::fs::create_dir_all(&snap).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&victim, snap.join("CodeUnit%50000.al")).unwrap();
    let files = sources::write_sources(&snap, &archive()).await.unwrap();
    let path = &files[&(ObjectType::CodeUnit, 50000)];
    assert_eq!(std::fs::read_to_string(path).unwrap(), "// source\n");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o444);
    }
    // A second replay overwrites the read-only file.
    sources::write_sources(&snap, &archive()).await.unwrap();
}

#[tokio::test]
async fn launch_with_a_missing_zip_fails_without_compiling() {
    let st = test_state();
    let (_, frames) = run_request_on(
        &st,
        "launch",
        json!({"snapshotFileName": "/nonexistent/none.zip"}),
    )
    .await;
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["success"], false);
    assert!(frames[0]["message"]
        .as_str()
        .unwrap()
        .contains("Cannot read snapshot"));
}

// ---------------------------------------------------------------------------
// Parity with Microsoft's adapter (needs AL_SNAPSHOT_FIXTURE_DIR)
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("AL_SNAPSHOT_FIXTURE_DIR").expect("AL_SNAPSHOT_FIXTURE_DIR is not set"),
    )
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

async fn send(st: &TestState, cmd: &str, args: Value) -> Vec<Value> {
    run_request_on(st, cmd, args).await.1
}

/// Variables of a reference as (name, value, type, children) down to `depth`
/// levels, in the shape of the golden files.
async fn tree(st: &TestState, reference: i64, depth: usize) -> Vec<Value> {
    let r = send(st, "variables", json!({"variablesReference": reference})).await;
    let mut out = Vec::new();
    for v in r[0]["body"]["variables"].as_array().unwrap() {
        let mut j = json!({
            "name": v["name"],
            "value": v["value"],
            "type": v.get("type").cloned().unwrap_or(json!("")),
        });
        let child = v["variablesReference"].as_i64().unwrap();
        if depth > 1 && child != 0 {
            j["children"] = Value::Array(Box::pin(tree(st, child, depth - 1)).await);
        }
        out.push(j);
    }
    out
}

/// Differences between ours and the golden tree, to `depth` levels.
fn diff_tree(path: &str, ours: &[Value], golden: &[Value], depth: usize, out: &mut Vec<String>) {
    if ours.len() != golden.len() {
        out.push(format!(
            "{path}: {} children vs golden {}",
            ours.len(),
            golden.len()
        ));
        return;
    }
    for (o, g) in ours.iter().zip(golden) {
        let here = format!("{path}/{}", g["name"].as_str().unwrap_or("?"));
        for k in ["name", "value", "type"] {
            let gv = g.get(k).cloned().unwrap_or(json!(""));
            if o[k] != gv {
                out.push(format!("{here} {k}: ours {} vs golden {}", o[k], gv));
            }
        }
        if depth > 1 {
            let oc = o
                .get("children")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let gc = g
                .get("children")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            diff_tree(&here, &oc, &gc, depth - 1, out);
        }
    }
}

/// Run one oracle trace from the fixture directory against the full server.
async fn parity(zip8: &str, mode: &str) -> Vec<String> {
    let dir = fixture_dir();
    let oracle = read_json(&dir.join(format!("oracle/oracle-{zip8}-{mode}.json")));
    let zip = dir.join(oracle["zip"].as_str().unwrap());
    parity_run(&zip, &oracle, &dir).await
}

/// Compare our replay of `zip` with the host oracle trace `oracle`. The host's
/// variable dump `golden-<zip8>*.json` is looked up in `golden_dir`.
async fn parity_run(
    zip: &std::path::Path,
    oracle: &Value,
    golden_dir: &std::path::Path,
) -> Vec<String> {
    let zip_name = oracle["zip"].as_str().unwrap().to_string();
    let zip8 = &zip_name[..8];
    let golden = std::fs::read_dir(golden_dir)
        .unwrap()
        .flatten()
        .find(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&format!("golden-{zip8}")) && n.ends_with(".json")
        })
        .map(|e| read_json(&e.path()))
        .expect("golden file");
    let sources = tempfile::tempdir().unwrap();
    let mut st = test_state();
    st.replay = ReplayState::new(sources.path().to_path_buf());

    send(&st, "initialize", json!({"adapterID": "al"})).await;
    let launch = send(
        &st,
        "launch",
        json!({"snapshotFileName": zip, "request": "launch"}),
    )
    .await;
    let mut diffs = Vec::new();
    assert_eq!(launch[0]["success"], true, "launch: {launch:?}");
    // The oracle's snappoint event.
    let ours: Vec<i64> = launch[1]["body"]["breakpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["line"].as_i64().unwrap())
        .collect();
    let theirs: Vec<i64> = oracle["snappointEvents"][0]["breakpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["line"].as_i64().unwrap())
        .collect();
    if ours != theirs {
        diffs.push(format!("snappoint lines: ours {ours:?} vs host {theirs:?}"));
    }
    if let Some(bp) = oracle["bpSetup"].as_object() {
        let base = bp["path"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        let path = sources
            .path()
            .join(zip_name.trim_end_matches(".zip"))
            .join(&base);
        let line = bp["line"].as_i64().unwrap();
        let r = send(
            &st,
            "setBreakpoints",
            json!({"source": {"path": path}, "breakpoints": [{"line": line}]}),
        )
        .await;
        let want = bp["response"]["body"]["breakpoints"][0]["verified"].clone();
        if r[0]["body"]["breakpoints"][0]["verified"] != want {
            diffs.push(format!(
                "breakpoint verified: ours {} vs host {want}",
                r[0]["body"]["breakpoints"][0]["verified"]
            ));
        }
    }

    let host_stops = oracle["stops"].as_array().unwrap();
    let mut ops: Vec<String> = vec!["configurationDone".into()];
    ops.extend(
        oracle["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_str().unwrap().to_string()),
    );
    let mut snap_seen = 0usize;
    let mut n = 0usize;
    let mut ended = false;
    for op in &ops {
        let m = send(&st, op, json!({"threadId": 1})).await;
        let stopped = m.iter().any(|e| e["event"] == "stopped");
        if m.iter().any(|e| e["event"] == "terminated") {
            ended = true;
            break;
        }
        if !stopped {
            diffs.push(format!("stop {}: no stopped event after {op}", n + 1));
            break;
        }
        n += 1;
        let Some(host) = host_stops.get(n - 1) else {
            diffs.push(format!(
                "stop {n}: ours has more stops than host ({})",
                host_stops.len()
            ));
            break;
        };
        let reason = m.iter().find(|e| e["event"] == "stopped").unwrap()["body"]["reason"]
            .as_str()
            .unwrap()
            .to_string();
        if !reason.eq_ignore_ascii_case(host["reason"].as_str().unwrap()) {
            diffs.push(format!(
                "stop {n} reason: ours {reason} vs host {}",
                host["reason"]
            ));
        }
        let st_r = send(&st, "stackTrace", json!({"threadId": 1})).await;
        let ours = st_r[0]["body"]["stackFrames"].as_array().unwrap().clone();
        let theirs = host["frames"].as_array().unwrap();
        if ours.len() != theirs.len() || st_r[0]["body"]["totalFrames"] != host["totalFrames"] {
            diffs.push(format!(
                "stop {n} frame count: ours {} vs host {}",
                ours.len(),
                theirs.len()
            ));
        }
        for (i, (o, h)) in ours.iter().zip(theirs).enumerate() {
            // The host shows objects of other apps from a decompiled
            // `al-preview://` copy (sourceOrigin External) whose end column is
            // one past the recorded span. We point at the recorded source in
            // the zip instead, so file and end column differ by design there.
            let external = h["sourceOrigin"] == "External";
            for k in ["name", "line", "column", "endLine", "endColumn"] {
                if o[k] != h[k] && !(external && k == "endColumn") {
                    diffs.push(format!(
                        "stop {n} frame {i} {k}: ours {} vs host {}",
                        o[k], h[k]
                    ));
                }
            }
            if o["source"]["name"] != h["sourceName"] {
                diffs.push(format!(
                    "stop {n} frame {i} source name: ours {} vs host {}",
                    o["source"]["name"], h["sourceName"]
                ));
            }
            let stem = h["sourcePath"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string();
            if !external && !o["source"]["path"].as_str().unwrap_or("").ends_with(&stem) {
                diffs.push(format!(
                    "stop {n} frame {i} source file: ours {} vs host {stem}",
                    o["source"]["path"]
                ));
            }
        }
        // Variables at snappoints.
        let sc = send(&st, "scopes", json!({"frameId": 0})).await;
        let scopes = sc[0]["body"]["scopes"].as_array().unwrap().clone();
        let globals = tree(&st, scopes[0]["variablesReference"].as_i64().unwrap(), 3).await;
        let locals = tree(&st, scopes[1]["variablesReference"].as_i64().unwrap(), 3).await;
        let is_empty_node = locals.len() == 1 && locals[0]["name"] == "EmptyNodeMessage";
        let gstops = golden["stops"].as_array().unwrap();
        let g = if is_empty_node {
            gstops
                .iter()
                .find(|g| g["scopes"][1]["variables"][0]["name"] == "EmptyNodeMessage")
        } else {
            snap_seen += 1;
            gstops.get(snap_seen - 1)
        };
        match g {
            Some(g) => {
                let mut d = Vec::new();
                diff_tree(
                    &format!("stop {n} Globals"),
                    &globals,
                    g["scopes"][0]["variables"].as_array().unwrap(),
                    3,
                    &mut d,
                );
                diff_tree(
                    &format!("stop {n} Locals"),
                    &locals,
                    g["scopes"][1]["variables"].as_array().unwrap(),
                    3,
                    &mut d,
                );
                diffs.extend(d);
            }
            None => {
                if !is_empty_node {
                    diffs.push(format!(
                        "stop {n}: snappoint stop {snap_seen} has no golden entry"
                    ));
                }
            }
        }
    }
    let host_end = oracle["end"]["event"].as_str().unwrap();
    if host_end == "terminated" && !ended {
        diffs.push(format!(
            "end: host terminated after {} stops, ours did not",
            host_stops.len()
        ));
    }
    if host_end == "terminated" && n != host_stops.len() {
        diffs.push(format!("stop count: ours {n} vs host {}", host_stops.len()));
    }
    if host_end != "terminated" && n != host_stops.len() {
        diffs.push(format!(
            "stop count at the cap: ours {n} vs host {}",
            host_stops.len()
        ));
    }
    diffs
}

macro_rules! parity_test {
    ($name:ident, $zip:expr, $mode:expr) => {
        #[tokio::test]
        #[ignore = "needs AL_SNAPSHOT_FIXTURE_DIR"]
        async fn $name() {
            let diffs = parity($zip, $mode).await;
            assert!(
                diffs.is_empty(),
                "{} differences:\n{}",
                diffs.len(),
                diffs.join("\n")
            );
        }
    };
}

parity_test!(replay_parity_83a73940_continue, "83a73940", "continue");
parity_test!(replay_parity_83a73940_next, "83a73940", "next");
parity_test!(replay_parity_83a73940_stepin, "83a73940", "stepin");
parity_test!(replay_parity_83a73940_breakpoint, "83a73940", "breakpoint");
parity_test!(replay_parity_a0d62542_continue, "a0d62542", "continue");
parity_test!(replay_parity_a0d62542_next, "a0d62542", "next");
parity_test!(replay_parity_a0d62542_stepin, "a0d62542", "stepin");
parity_test!(replay_parity_a0d62542_breakpoint, "a0d62542", "breakpoint");

/// Parity for any zip: `AL_SNAPSHOT_PARITY_ZIP` is the snapshot zip and
/// `AL_SNAPSHOT_PARITY_ORACLE_DIR` holds `oracle-<zip8>-<mode>.json` for the
/// four modes and the host's `golden-<zip8>*.json` variable dump. Skips when
/// either variable is unset.
#[tokio::test]
#[ignore = "needs AL_SNAPSHOT_PARITY_ZIP and AL_SNAPSHOT_PARITY_ORACLE_DIR"]
async fn replay_parity_env_zip() {
    let (Ok(zip), Ok(dir)) = (
        std::env::var("AL_SNAPSHOT_PARITY_ZIP"),
        std::env::var("AL_SNAPSHOT_PARITY_ORACLE_DIR"),
    ) else {
        eprintln!("skipped: AL_SNAPSHOT_PARITY_ZIP or AL_SNAPSHOT_PARITY_ORACLE_DIR is unset");
        return;
    };
    let zip = PathBuf::from(zip);
    let dir = PathBuf::from(dir);
    let zip8: String = zip.file_name().unwrap().to_string_lossy()[..8].to_string();
    let mut failed = Vec::new();
    for mode in ["continue", "next", "stepin", "breakpoint"] {
        let oracle = read_json(&dir.join(format!("oracle-{zip8}-{mode}.json")));
        let diffs = parity_run(&zip, &oracle, &dir).await;
        eprintln!("mode {mode}: {} differences", diffs.len());
        for d in &diffs {
            eprintln!("  {mode}: {d}");
        }
        if !diffs.is_empty() {
            failed.push(mode);
        }
    }
    assert!(failed.is_empty(), "modes with differences: {failed:?}");
}
