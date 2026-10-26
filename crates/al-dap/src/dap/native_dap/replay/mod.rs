//! Replay of a Business Central snapshot zip through the native adapter.
//!
//! A `launch` whose arguments carry `snapshotFileName` starts a replay in
//! place of compile, publish and attach. The zip is read with
//! `al_bc::snapshot::archive`, its sources are written read-only to a cache
//! folder, and the recorded statements are walked by [`engine::Engine`] with
//! the rules of Microsoft's adapter (continue runs to the next snappoint or
//! user breakpoint, next/stepIn/stepOut move one statement).
//!
//! The session is plain data that turns one request into the messages to
//! send, so the glue in the shared files is a lock, a call and a write loop.
//!
//! Differences from Microsoft's adapter, on purpose:
//! - `initialized` is sent by `initialize` in this adapter, not after
//!   `launch`. Breakpoints that arrive before `launch` wait in the shared
//!   pending map and are applied when the replay starts.
//! - Microsoft's host sends one more `stopped` after `terminated` because its
//!   break request is still running. That is not copied.
//! - `stopped.reason` is the DAP value `step` (the host sends `Step`).
//! - An error stop (a context that carries an exception) is reason
//!   `exception` with the message as `text`, and Locals gets a variable named
//!   `_ALException` holding the message. ASSUMPTION: no sample zip has an
//!   exception, so the reason string and the variable name come from the
//!   decompiled host's error node and are untested against a real recording.
//! - Timestamps are not added to `output` text.

pub mod engine;
mod sources;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use al_bc::snapshot::archive::{split_scopes, ObjectType, SnapshotArchive, Variable};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::dap::Result;

use self::engine::{Engine, Landing, LineKey, Pos, Step};
use super::{make_event, make_response, write_dap, NativeDapState, ResolvedObject};

const THREAD_ID: i64 = 1;
const THREAD_NAME: &str = "Session Id";
/// First id the host hands out for snappoints. Kept so ids look alike.
const FIRST_BREAKPOINT_ID: i64 = 268_439_396;
const VARIABLE_BASE: i64 = 1 << 18;
const EMPTY_NODE_TEXT: &str = "Variable information is only available on snappoints.";
const NO_FRAMES_TEXT: &str =
    "No available frames have been found. Starting from the first recorded frame.\n";
const END_TEXT: &str = "No more frames are available.\n";

/// The replay slot and the folder sources are written to.
pub(crate) struct ReplayState {
    session: Arc<Mutex<Option<ReplaySession>>>,
    sources_dir: PathBuf,
}

impl ReplayState {
    pub fn new(sources_dir: PathBuf) -> Self {
        ReplayState {
            session: Arc::new(Mutex::new(None)),
            sources_dir,
        }
    }
}

impl Default for ReplayState {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join("al-lsp-snapshot-sources"))
    }
}

/// Requests the replay answers itself.
pub(super) struct Reply {
    pub messages: Vec<Value>,
    /// The client is done: leave the server loop.
    pub done: bool,
}

pub(super) type Resolver<'a> = &'a (dyn Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync);

pub(super) struct ReplaySession {
    archive: SnapshotArchive,
    engine: Engine,
    /// Where each object's source was written.
    files: HashMap<(ObjectType, i32), PathBuf>,
    sources_dir: PathBuf,
    stop: Option<Landing>,
    ended: bool,
    handles: HashMap<i64, Vec<Variable>>,
    next_handle: i64,
    ids: HashMap<LineKey, i64>,
    next_id: i64,
}

fn text_event(seq: &AtomicU64, text: &str) -> Value {
    make_event(
        seq,
        "output",
        Some(json!({ "category": "console", "output": text })),
    )
}

impl ReplaySession {
    pub(super) fn new(
        archive: SnapshotArchive,
        files: HashMap<(ObjectType, i32), PathBuf>,
        sources_dir: PathBuf,
    ) -> Self {
        let engine = Engine::new(archive.method_contexts());
        let mut s = ReplaySession {
            archive,
            engine,
            files,
            sources_dir,
            stop: None,
            ended: false,
            handles: HashMap::new(),
            next_handle: VARIABLE_BASE,
            ids: HashMap::new(),
            next_id: FIRST_BREAKPOINT_ID,
        };
        let keys: Vec<LineKey> = s.engine.snappoints().to_vec();
        for k in keys {
            s.id_of(k);
        }
        s
    }

    fn id_of(&mut self, key: LineKey) -> i64 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.ids.insert(key, id);
        id
    }

    fn source_json(&self, object_type: ObjectType, number: i32, name: &str) -> Option<Value> {
        let path = self.files.get(&(object_type, number))?;
        Some(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "sourceReference": 0,
        }))
    }

    /// `al/updateSnappointsEvent`: the snappoint lines, 0-based as the host sends them.
    pub(super) fn snappoints_event(&self, seq: &AtomicU64) -> Value {
        let items: Vec<Value> = self
            .engine
            .snappoints()
            .iter()
            .map(|k| {
                let t = ObjectType::from_i8(k.object_type);
                let stem = sources::file_name(t, k.number);
                let stem = stem.strip_suffix(".al").unwrap_or(&stem).to_string();
                let source = self
                    .files
                    .get(&(t, k.number))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                json!({
                    "id": self.ids.get(k).copied().unwrap_or(0),
                    "verified": true,
                    "source": {
                        "name": stem,
                        "path": source,
                        "sourceReference": 0,
                        "presentationHint": 0,
                    },
                    "line": k.line,
                    "column": 0,
                    "endLine": 0,
                    "endColumn": 0,
                })
            })
            .collect();
        make_event(
            seq,
            "al/updateSnappointsEvent",
            Some(json!({ "breakpoints": items })),
        )
    }

    /// The object a client file belongs to: a snapshot source by its file
    /// name, anything else through the workspace index.
    fn object_of_path(&self, path: &str, line: i64, resolve: Resolver<'_>) -> Option<(i8, i32)> {
        let p = Path::new(path);
        if p.starts_with(&self.sources_dir) {
            let (t, n) = sources::object_of_file_name(&p.file_name()?.to_string_lossy())?;
            return Some((t.as_i8(), n));
        }
        let r = resolve(path, line)?;
        Some((i8::try_from(r.object_type).ok()?, r.object_id))
    }

    /// Apply a `setBreakpoints` request and build its response body.
    pub(super) fn set_breakpoints(
        &mut self,
        path: &str,
        lines: &[i64],
        resolve: Resolver<'_>,
    ) -> Value {
        let object = lines
            .first()
            .and_then(|l| self.object_of_path(path, *l, resolve));
        let results = match object {
            Some((t, n)) => self.engine.set_user_breakpoints(path, t, n, lines),
            None => {
                self.engine.set_user_breakpoints(path, 0, 0, &[]);
                Vec::new()
            }
        };
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let (verified, id) = match results.get(i) {
                Some(r) if r.verified => (true, self.id_of(r.key)),
                _ => (false, 0),
            };
            out.push(json!({
                "id": id,
                "verified": verified,
                "source": { "path": path, "sourceReference": 0, "presentationHint": 0 },
                "line": line,
                "column": 0,
                "endLine": 0,
                "endColumn": 0,
                "message": "",
            }));
        }
        json!({ "breakpoints": out })
    }

    /// Messages for a finished move.
    fn arrive(&mut self, seq: &AtomicU64, step: Step, out: &mut Vec<Value>) {
        self.handles.clear();
        self.next_handle = VARIABLE_BASE;
        match step {
            Step::Stopped(landing) => {
                if landing.start_over {
                    out.push(text_event(seq, NO_FRAMES_TEXT));
                }
                self.stop = Some(landing);
                let mut body = json!({
                    "reason": "step",
                    "threadId": THREAD_ID,
                    "allThreadsStopped": true,
                });
                if landing.exception {
                    body["reason"] = json!("exception");
                    body["description"] = json!("Paused on exception");
                    if let Some(msg) = self.exception_message(landing.pos) {
                        body["text"] = json!(msg);
                    }
                }
                out.push(make_event(seq, "stopped", Some(body)));
            }
            Step::Ended => {
                self.stop = None;
                self.ended = true;
                out.push(text_event(seq, END_TEXT));
                out.push(make_event(
                    seq,
                    "terminated",
                    Some(json!({ "restart": false })),
                ));
            }
        }
    }

    fn exception_message(&self, pos: Pos) -> Option<String> {
        self.archive
            .method_contexts()
            .get(pos.ctx)?
            .exception
            .as_ref()
            .map(|e| e.message.clone())
    }

    fn run(&mut self, command: &str) -> Option<Step> {
        if self.ended {
            return None;
        }
        Some(match command {
            "configurationDone" => self.engine.start(),
            "continue" => self.engine.cont(),
            "next" => self.engine.next(),
            "stepIn" => self.engine.step_in(),
            _ => self.engine.step_out(),
        })
    }

    /// Handle one request. `None` means this request is not the replay's.
    pub(super) fn handle(
        &mut self,
        seq: &AtomicU64,
        request_seq: i64,
        command: &str,
        args: &Value,
        resolve: Resolver<'_>,
    ) -> Option<Reply> {
        let ok = |body: Option<Value>| make_response(seq, request_seq, command, true, body, None);
        let mut messages = Vec::new();
        match command {
            "configurationDone" | "continue" | "next" | "stepIn" | "stepOut" => {
                let body = (command == "continue").then(|| json!({ "allThreadsContinued": true }));
                messages.push(ok(body));
                if let Some(step) = self.run(command) {
                    self.arrive(seq, step, &mut messages);
                }
            }
            "threads" => messages.push(ok(Some(
                json!({ "threads": [{ "id": THREAD_ID, "name": THREAD_NAME }] }),
            ))),
            "stackTrace" => {
                let body = self.stack_trace(args);
                messages.push(ok(Some(body)));
            }
            "scopes" => {
                let frame = args.get("frameId").and_then(Value::as_i64).unwrap_or(0);
                messages.push(ok(Some(scopes(frame))));
            }
            "variables" => {
                let r = args
                    .get("variablesReference")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let list = self.variables_for(r);
                let body = json!({ "variables": self.render(&list) });
                messages.push(ok(Some(body)));
            }
            "evaluate" => {
                let expr = args.get("expression").and_then(Value::as_str).unwrap_or("");
                match self.evaluate(expr.trim()) {
                    Some(v) => messages.push(ok(Some(v))),
                    None => messages.push(make_response(
                        seq,
                        request_seq,
                        command,
                        false,
                        None,
                        Some(format!("Cannot evaluate '{expr}' in a snapshot replay")),
                    )),
                }
            }
            "setBreakpoints" => {
                let path = args
                    .pointer("/source/path")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let lines: Vec<i64> = match args.get("breakpoints").and_then(Value::as_array) {
                    Some(b) => b
                        .iter()
                        .filter_map(|b| b.get("line").and_then(Value::as_i64))
                        .collect(),
                    None => args
                        .get("lines")
                        .and_then(Value::as_array)
                        .map(|l| l.iter().filter_map(Value::as_i64).collect())
                        .unwrap_or_default(),
                };
                let body = self.set_breakpoints(path, &lines, resolve);
                messages.push(ok(Some(body)));
            }
            "pause" => messages.push(ok(None)),
            "disconnect" | "terminate" => {
                messages.push(ok(None));
                if !self.ended {
                    messages.push(make_event(seq, "terminated", None));
                }
                return Some(Reply {
                    messages,
                    done: true,
                });
            }
            _ => return None,
        }
        Some(Reply {
            messages,
            done: false,
        })
    }

    fn stack_trace(&self, args: &Value) -> Value {
        let Some(stop) = self.stop else {
            return json!({ "stackFrames": [], "totalFrames": 0 });
        };
        let ctx = &self.archive.method_contexts()[stop.pos.ctx];
        let lc = &ctx.line_contexts[stop.pos.line];
        let mut frames = Vec::new();
        for (i, f) in ctx.frames.iter().enumerate() {
            let (span, id, name, object_name) = if i == 0 {
                (
                    lc.statement_span,
                    f.id,
                    if f.method_name.is_empty() {
                        ctx.method_name.as_str()
                    } else {
                        f.method_name.as_str()
                    },
                    f.object_name.as_str(),
                )
            } else {
                (
                    f.statement_span,
                    f.id,
                    f.method_name.as_str(),
                    f.object_name.as_str(),
                )
            };
            let mut frame = json!({
                "id": i,
                "name": name,
                "line": u32::from(span.from.line) + 1,
                "column": u32::from(span.from.column) + 1,
                "endLine": u32::from(span.to.line) + 1,
                "endColumn": u32::from(span.to.column) + 1,
            });
            if let Some(src) = self.source_json(id.object_type, id.number, object_name) {
                frame["source"] = src;
            }
            frames.push(frame);
        }
        if frames.is_empty() {
            // A context without recorded frames: show the context itself.
            let span = lc.statement_span;
            let mut frame = json!({
                "id": 0,
                "name": ctx.method_name,
                "line": u32::from(span.from.line) + 1,
                "column": u32::from(span.from.column) + 1,
                "endLine": u32::from(span.to.line) + 1,
                "endColumn": u32::from(span.to.column) + 1,
            });
            if let Some(src) = self.source_json(ctx.id.object_type, ctx.id.number, "") {
                frame["source"] = src;
            }
            frames.push(frame);
        }
        let total = frames.len();
        let start = args
            .get("startFrame")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(total as u64) as usize;
        let levels = args.get("levels").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = if levels == 0 {
            total
        } else {
            (start + levels).min(total)
        };
        json!({ "stackFrames": frames[start..end], "totalFrames": total })
    }

    /// Variables of a scope reference or of a child handle.
    fn variables_for(&mut self, reference: i64) -> Vec<Variable> {
        if reference >= VARIABLE_BASE {
            return self.handles.get(&reference).cloned().unwrap_or_default();
        }
        let group = reference >> 16;
        let frame = reference & 0xffff;
        let empty = || {
            vec![Variable {
                name: "EmptyNodeMessage".to_string(),
                value: EMPTY_NODE_TEXT.to_string(),
                type_name: "String".to_string(),
                ..Variable::default()
            }]
        };
        if group == 3 {
            return Vec::new();
        }
        let Some(stop) = self.stop.filter(|_| frame == 0) else {
            return empty();
        };
        let ctx = &self.archive.method_contexts()[stop.pos.ctx];
        let lc = &ctx.line_contexts[stop.pos.line];
        let message = stop
            .exception
            .then(|| ctx.exception.as_ref().map(|e| e.message.clone()))
            .flatten();
        if lc.variables.is_empty() && message.is_none() {
            return empty();
        }
        let (mut locals, globals) = split_scopes(&lc.variables);
        if let Some(m) = message {
            locals.insert(
                0,
                Variable {
                    name: "_ALException".to_string(),
                    value: m,
                    ..Variable::default()
                },
            );
        }
        if group == 1 {
            globals
        } else {
            locals
        }
    }

    /// Turn variables into DAP variables, giving the ones with children a handle.
    fn render(&mut self, list: &[Variable]) -> Vec<Value> {
        list.iter()
            .map(|v| {
                let reference = self.handle_for(v);
                let mut j = json!({
                    "name": v.name,
                    "value": v.value,
                    "variablesReference": reference,
                });
                if !v.type_name.is_empty() {
                    j["type"] = json!(v.type_name);
                }
                j
            })
            .collect()
    }

    fn handle_for(&mut self, v: &Variable) -> i64 {
        if v.children.is_empty() {
            return 0;
        }
        let r = self.next_handle;
        self.next_handle += 1;
        self.handles.insert(r, v.children.clone());
        r
    }

    fn evaluate(&mut self, expr: &str) -> Option<Value> {
        if expr.is_empty() {
            return None;
        }
        let locals = self.variables_for(2 << 16);
        let globals = self.variables_for(1 << 16);
        let v = locals
            .iter()
            .chain(globals.iter())
            .filter(|v| v.name != "EmptyNodeMessage")
            .find(|v| v.name.eq_ignore_ascii_case(expr))?
            .clone();
        let reference = self.handle_for(&v);
        let mut j = json!({ "result": v.value, "variablesReference": reference });
        if !v.type_name.is_empty() {
            j["type"] = json!(v.type_name);
        }
        Some(j)
    }
}

fn scopes(frame: i64) -> Value {
    let frame = frame & 0xffff;
    json!({ "scopes": [
        { "name": "Globals", "variablesReference": (1 << 16) | frame, "expensive": true },
        { "name": "Locals", "variablesReference": (2 << 16) | frame, "expensive": true },
        { "name": "Database Statistics", "variablesReference": (3 << 16) | frame, "expensive": true },
    ] })
}

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    R: Fn(&str, i64) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send + 'static,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    /// Route a request to the active replay. `Some(done)` when the replay
    /// answered, `None` when no replay is active or the request is not its own.
    pub(super) async fn dispatch_replay<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        command: &str,
        request_seq: i64,
        arguments: &Value,
    ) -> Result<Option<bool>> {
        let mut guard = self.replay.session.lock().await;
        let Some(session) = guard.as_mut() else {
            return Ok(None);
        };
        let Some(reply) = session.handle(
            &self.seq,
            request_seq,
            command,
            arguments,
            &self.resolve_object,
        ) else {
            return Ok(None);
        };
        for m in &reply.messages {
            write_dap(out, m).await?;
        }
        if reply.done {
            *guard = None;
        }
        Ok(Some(reply.done))
    }

    /// `launch` with `snapshotFileName`: read the zip, write its sources and
    /// start a replay. No compile, publish, token or attach.
    pub(super) async fn handle_replay_launch<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &Value,
    ) -> Result<()> {
        let zip = PathBuf::from(
            arguments
                .get("snapshotFileName")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        let archive = {
            let zip = zip.clone();
            tokio::task::spawn_blocking(move || SnapshotArchive::open(&zip)).await
        };
        let archive = match archive {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => {
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Cannot read snapshot {}: {e}", zip.display()),
                    )
                    .await;
            }
            Err(e) => {
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Cannot read snapshot: {e}"),
                    )
                    .await;
            }
        };
        let dir = self.replay.sources_dir.join(sources::folder_name(&zip));
        let files = match sources::write_sources(&dir, &archive).await {
            Ok(f) => f,
            Err(e) => {
                return self
                    .reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Cannot write snapshot sources to {}: {e}", dir.display()),
                    )
                    .await;
            }
        };
        let mut session = ReplaySession::new(archive, files, dir);
        // Breakpoints sent before the launch wait in the shared pending map.
        let pending = std::mem::take(&mut *self.pending_breakpoints.lock().await);
        for (path, requests) in pending {
            let lines: Vec<i64> = requests.iter().map(|(l, _)| *l).collect();
            session.set_breakpoints(&path, &lines, &self.resolve_object);
        }
        let event = session.snappoints_event(&self.seq);
        *self.replay.session.lock().await = Some(session);
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, None, None),
        )
        .await?;
        write_dap(out, &event).await?;
        Ok(())
    }
}
