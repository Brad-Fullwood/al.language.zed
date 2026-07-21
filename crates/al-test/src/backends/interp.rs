//! Interpreter backend — runs AL test procedures through the pure-Rust
//! tree-walking interpreter.
//!
//! `InterpMode` implements `TestSession`. For each `TestId` it:
//!  1. Discovers the test procedure in the workspace via
//!     `queries::tests::discover_tests`.
//!  2. Finds the procedure body node in the cached parse tree.
//!  3. Builds a `CallFrame` and runs `eval_stmt` over the body.
//!  4. Maps the resulting `Eval` to a `TestMethodResult`.
//!  5. Streams events through the caller-provided `mpsc::Sender<TestEvent>`.
//!
//! The backend mirrors the LiveBcMode pattern: parallel JoinSet dispatch,
//! per-test timeout, and channel-closed detection.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::warn;
use tree_sitter::Node;

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult, TestStatus};
use crate::session::{RunOptions, TestEvent, TestId, TestSession};
use al_analysis::queries::tests::{discover_tests, TestCodeunit};
use al_runtime::interpreter::coverage::{Coverage, DynamicCoverageReport};
use al_runtime::interpreter::dispatch::{DispatchCtx, DispatchMode, TestHandlers};
use al_runtime::interpreter::eval_stmt::eval_stmt;
use al_runtime::interpreter::scope::{CallFrame, Eval, ScopeStack};
use al_workspace::Workspace;

/// Interpreter backend: runs test procedures without a live BC server.
///
/// Runs either pure-logic tests or record-backed tests, depending on the
/// [`DispatchMode`] selected at construction time. Platform-dependent tests
/// (HTTP, UI, reports, transactions, and similar) must still be routed to
/// `LiveBcMode`.
///
/// ## Dynamic coverage
///
/// Construct with [`InterpMode::with_coverage`] to additionally collect
/// *dynamic* statement/branch coverage while the tests run (the default
/// [`InterpMode::new`] leaves it off — static call-graph coverage via
/// `al-analysis` is unaffected either way). After a [`TestSession::run`] call,
/// read the aggregated, per-file report via [`InterpMode::coverage_report`].
pub struct InterpMode {
    pub workspace: Arc<Workspace>,
    /// Interpreter capability boundary for this backend instance. Keeping the
    /// mode on the session (rather than inferring it while evaluating calls)
    /// makes the router's decision enforceable at runtime.
    dispatch_mode: DispatchMode,
    /// When `true`, each test runs with an interpreter coverage collector and
    /// its hits are merged into `coverage`. Off by default (zero-cost).
    collect_coverage: bool,
    /// Aggregated dynamic coverage across every test in the run. Shared with
    /// the per-codeunit worker tasks (which may run on blocking threads).
    coverage: Arc<Mutex<Coverage>>,
}

impl InterpMode {
    /// Construct a backend without dynamic coverage. Static coverage is
    /// produced separately.
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            dispatch_mode: DispatchMode::PureLogic,
            collect_coverage: false,
            coverage: Arc::new(Mutex::new(Coverage::new())),
        }
    }

    /// Construct the executable `InterpRecord` backend. Record variables use
    /// an isolated in-memory store for each test method; no Business Central
    /// server is contacted.
    pub fn with_records(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            dispatch_mode: DispatchMode::WithRecords,
            collect_coverage: false,
            coverage: Arc::new(Mutex::new(Coverage::new())),
        }
    }

    /// Construct a backend in "dynamic coverage" mode: runs tests *and* records
    /// which source lines and branches each executes. Read the result with
    /// [`InterpMode::coverage_report`] after `run` completes.
    pub fn with_coverage(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            dispatch_mode: DispatchMode::PureLogic,
            collect_coverage: true,
            coverage: Arc::new(Mutex::new(Coverage::new())),
        }
    }

    /// Construct the record-backed interpreter with dynamic coverage enabled.
    pub fn with_records_and_coverage(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            dispatch_mode: DispatchMode::WithRecords,
            collect_coverage: true,
            coverage: Arc::new(Mutex::new(Coverage::new())),
        }
    }

    /// Capability boundary enforced by this session.
    pub fn dispatch_mode(&self) -> DispatchMode {
        self.dispatch_mode
    }

    /// True when this backend is collecting dynamic coverage.
    pub fn collects_coverage(&self) -> bool {
        self.collect_coverage
    }

    /// The aggregated per-file dynamic-coverage report gathered during the last
    /// `run`. Empty when coverage collection is disabled or `run` has not been
    /// called.
    pub fn coverage_report(&self) -> DynamicCoverageReport {
        self.coverage.lock().map(|c| c.report()).unwrap_or_default()
    }
}

#[allow(async_fn_in_trait)]
impl TestSession for InterpMode {
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError> {
        if tests.is_empty() {
            send_event(
                &tx,
                TestEvent::SessionComplete {
                    total: 0,
                    passed: 0,
                    failed: 0,
                    skipped: 0,
                },
            )
            .await?;
            return Ok(());
        }

        let timeout_dur = Duration::from_millis(opts.timeout_ms.unwrap_or(30_000));

        // Discover all test codeunits once (async workspace reads are not needed —
        // `discover_tests` is sync and reads from the cached file_index).
        let ws_for_discover = Arc::clone(&self.workspace);
        let codeunits = tokio::task::spawn_blocking(move || discover_tests(&ws_for_discover))
            .await
            .unwrap_or_default();

        // Group TestIds by codeunit_id, deduplicating identical
        // (codeunit_id, method_name) targets. A malformed RPC call can repeat
        // the same TestId; without this guard the interpreter would run the
        // procedure once per duplicate and `from_methods` would count each
        // duplicate, inflating the SuiteComplete/SessionComplete tallies.
        let mut seen: HashSet<(i32, Option<String>)> = HashSet::new();
        let mut grouped: HashMap<i32, Vec<Option<String>>> = HashMap::new();
        for test_id in &tests {
            if !seen.insert((test_id.codeunit_id, test_id.method_name.clone())) {
                continue;
            }
            grouped
                .entry(test_id.codeunit_id)
                .or_default()
                .push(test_id.method_name.clone());
        }

        let mut all_summaries: Vec<TestCodeunitResult> = Vec::new();

        let work: Vec<(i32, String, Vec<Option<String>>)> = grouped
            .into_iter()
            .map(|(codeunit_id, methods)| {
                let name = tests
                    .iter()
                    .find(|t| t.codeunit_id == codeunit_id)
                    .map(|t| t.codeunit_name.clone())
                    .unwrap_or_else(|| format!("codeunit_{codeunit_id}"));
                (codeunit_id, name, methods)
            })
            .collect();

        if opts.parallel {
            let mut join_set: JoinSet<Vec<TestEvent>> = JoinSet::new();

            for (codeunit_id, codeunit_name, methods) in work {
                let ws = Arc::clone(&self.workspace);
                let codeunits_clone = codeunits.clone();
                let collect_coverage = self.collect_coverage;
                let dispatch_mode = self.dispatch_mode;
                let coverage = Arc::clone(&self.coverage);
                join_set.spawn_blocking(move || {
                    run_codeunit_interp(
                        &ws,
                        &codeunits_clone,
                        codeunit_id,
                        &codeunit_name,
                        &methods,
                        timeout_dur,
                        dispatch_mode,
                        collect_coverage,
                        &coverage,
                    )
                });
            }

            while let Some(join_result) = join_set.join_next().await {
                let events = match join_result {
                    Ok(inner) => inner,
                    Err(_) => continue,
                };
                for event in events {
                    if let TestEvent::SuiteComplete { ref summary, .. } = event {
                        all_summaries.push(summary.clone());
                    }
                    send_event(&tx, event).await?;
                }
            }
        } else {
            for (codeunit_id, codeunit_name, methods) in work {
                let ws = Arc::clone(&self.workspace);
                let codeunits_clone = codeunits.clone();
                let name_clone = codeunit_name.clone();
                let collect_coverage = self.collect_coverage;
                let dispatch_mode = self.dispatch_mode;
                let coverage = Arc::clone(&self.coverage);
                let events = tokio::task::spawn_blocking(move || {
                    run_codeunit_interp(
                        &ws,
                        &codeunits_clone,
                        codeunit_id,
                        &name_clone,
                        &methods,
                        timeout_dur,
                        dispatch_mode,
                        collect_coverage,
                        &coverage,
                    )
                })
                .await
                .unwrap_or_default();

                for event in events {
                    if let TestEvent::SuiteComplete { ref summary, .. } = event {
                        all_summaries.push(summary.clone());
                    }
                    send_event(&tx, event).await?;
                }
            }
        }

        let total: usize = all_summaries.iter().map(|s| s.total).sum();
        let passed: usize = all_summaries.iter().map(|s| s.passed).sum();
        let failed: usize = all_summaries.iter().map(|s| s.failed).sum();
        let skipped: usize = all_summaries.iter().map(|s| s.skipped).sum();
        send_event(
            &tx,
            TestEvent::SessionComplete {
                total,
                passed,
                failed,
                skipped,
            },
        )
        .await?;

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn run_codeunit_interp(
    workspace: &Workspace,
    codeunits: &[TestCodeunit],
    codeunit_id: i32,
    codeunit_name: &str,
    methods: &[Option<String>],
    timeout_dur: Duration,
    dispatch_mode: DispatchMode,
    collect_coverage: bool,
    coverage: &Arc<Mutex<Coverage>>,
) -> Vec<TestEvent> {
    let mut events = Vec::new();
    let mut method_results: Vec<TestMethodResult> = Vec::new();

    let cu = codeunits.iter().find(|c| c.id == codeunit_id);

    let proc_list: Vec<String> = if methods.iter().any(|m| m.is_some()) {
        methods.iter().filter_map(|m| m.clone()).collect()
    } else if let Some(cu) = cu {
        cu.tests.iter().map(|t| t.name.clone()).collect()
    } else {
        vec![]
    };

    if proc_list.is_empty() {
        events.push(TestEvent::SuiteComplete {
            codeunit_id,
            summary: TestCodeunitResult::from_methods(
                codeunit_name.to_string(),
                codeunit_id,
                vec![],
            ),
        });
        return events;
    }

    for proc_name in &proc_list {
        let id = TestId {
            codeunit_id,
            codeunit_name: codeunit_name.to_string(),
            method_name: Some(proc_name.clone()),
        };
        events.push(TestEvent::CaseStarted { id: id.clone() });

        // Do not leak stateful test-library stubs between test methods.
        al_runtime::stubs::reset_thread_local_state();

        let start = Instant::now();
        let (result, test_coverage) = run_procedure_interp(
            workspace,
            cu,
            codeunit_name,
            proc_name,
            timeout_dur,
            dispatch_mode,
            collect_coverage,
        );
        let duration_ms = start.elapsed().as_millis() as u64;

        // Merge this test's dynamic coverage into the run-wide aggregate (gap
        // Only present when coverage collection is enabled.
        if let Some(test_cov) = test_coverage {
            if let Ok(mut agg) = coverage.lock() {
                agg.merge(&test_cov);
            }
        }

        let method_result = TestMethodResult {
            name: proc_name.clone(),
            status: match &result {
                Eval::Normal(_) | Eval::Exit(_) => TestStatus::Pass,
                // A break/continue that unwound out of the whole test body
                // escaped all loops, which is a runtime error, so the test fails.
                Eval::Error(_) | Eval::Break | Eval::Continue => TestStatus::Fail,
            },
            error: match &result {
                Eval::Error(e) => Some(e.message.clone()),
                Eval::Break => Some("break statement not inside a loop".to_string()),
                Eval::Continue => Some("continue statement not inside a loop".to_string()),
                _ => None,
            },
            duration_ms: Some(duration_ms),
        };
        method_results.push(method_result.clone());
        events.push(TestEvent::CaseResult {
            id,
            result: method_result,
        });
    }

    let summary =
        TestCodeunitResult::from_methods(codeunit_name.to_string(), codeunit_id, method_results);
    events.push(TestEvent::SuiteComplete {
        codeunit_id,
        summary,
    });

    events
}

/// Run one test procedure. Returns its `Eval` result plus, when
/// `collect_coverage` is set, the dynamic statement/branch coverage it produced
/// The coverage `Option` is `None` when collection is disabled, keeping the
/// static-only path zero-cost and unchanged.
fn run_procedure_interp(
    workspace: &Workspace,
    cu: Option<&TestCodeunit>,
    codeunit_name: &str,
    proc_name: &str,
    timeout_dur: Duration,
    dispatch_mode: DispatchMode,
    collect_coverage: bool,
) -> (Eval, Option<Coverage>) {
    let cu = match cu {
        Some(c) => c,
        None => {
            return (
                Eval::Error(al_runtime::interpreter::value::ErrorInfo {
                    message: format!("codeunit '{codeunit_name}' not found in workspace"),
                    error_type: None,
                    source: None,
                }),
                None,
            );
        }
    };

    let path = std::path::Path::new(&cu.file);
    let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
        return (
            Eval::Error(al_runtime::interpreter::value::ErrorInfo {
                message: format!("could not parse file for codeunit '{codeunit_name}'"),
                error_type: None,
                source: None,
            }),
            None,
        );
    };

    let source = text.as_bytes();
    let root = tree.root_node();

    let proc_source: Arc<dyn al_types::ProcedureSource> = workspace.file_index.clone();
    // Thread the timeout down to the interpreter so a runaway
    // `while true do …` test fails with a clear "deadline exceeded" error
    // instead of pinning the spawned blocking thread until the daemon
    // shuts down.
    let mut ctx = DispatchCtx {
        source: proc_source,
        records: HashMap::new(),
        mode: dispatch_mode,
        recursion_depth: 0,
        ast_depth: 0,
        deadline: Some(std::time::Instant::now() + timeout_dur),
        cancel: None,
        // Attach a dynamic-coverage collector only when requested,
        // seeded with this codeunit's file so its statements attribute there.
        coverage: collect_coverage.then(|| {
            let mut cov = Coverage::new();
            cov.set_current_file(&cu.file);
            cov
        }),
        var_writebacks: Vec::new(),
        test_handlers: configured_handlers(cu, root, source, proc_name, codeunit_name),
    };

    // One context (especially one record store and deadline) spans the full BC
    // lifecycle for this test method. Cleanup always runs, including after a
    // failing initializer or test body.
    let mut result = Eval::Normal(al_runtime::interpreter::value::Value::Null);
    for initializer in &cu.test_initializers {
        let lifecycle =
            eval_named_procedure(root, source, codeunit_name, &initializer.name, &mut ctx);
        if lifecycle.is_error() {
            result = lifecycle;
            break;
        }
    }
    if !result.is_error() {
        result = eval_named_procedure(root, source, codeunit_name, proc_name, &mut ctx);
    }
    for cleanup in &cu.test_cleanups {
        let lifecycle = eval_named_procedure(root, source, codeunit_name, &cleanup.name, &mut ctx);
        if !result.is_error() && lifecycle.is_error() {
            result = lifecycle;
        }
    }
    (result, ctx.coverage)
}

fn configured_handlers(
    cu: &TestCodeunit,
    root: Node<'_>,
    source: &[u8],
    proc_name: &str,
    codeunit_name: &str,
) -> TestHandlers {
    let mut handlers = TestHandlers::default();
    let Some(test) = cu
        .tests
        .iter()
        .find(|test| test.name.eq_ignore_ascii_case(proc_name))
    else {
        return handlers;
    };
    for handler_name in &test.handler_functions {
        let Some(handler) = find_procedure_node(root, source, handler_name) else {
            continue;
        };
        if procedure_has_attribute(handler, source, "MessageHandler") {
            handlers.message = Some((codeunit_name.to_string(), handler_name.clone()));
        } else if procedure_has_attribute(handler, source, "ConfirmHandler") {
            handlers.confirm = Some((codeunit_name.to_string(), handler_name.clone()));
        }
    }
    handlers
}

fn procedure_has_attribute(node: Node<'_>, source: &[u8], wanted: &str) -> bool {
    let matches = |attribute: Node<'_>| {
        attribute.utf8_text(source).ok().is_some_and(|text| {
            let name = text
                .trim()
                .trim_start_matches('[')
                .split(['(', ';', ']'])
                .next()
                .unwrap_or("")
                .trim();
            name.eq_ignore_ascii_case(wanted)
        })
    };
    let mut cursor = node.walk();
    if node
        .children(&mut cursor)
        .any(|child| matches!(child.kind(), "attribute" | "attribute_list") && matches(child))
    {
        return true;
    }
    let mut sibling = node.prev_sibling();
    while let Some(child) = sibling {
        match child.kind() {
            "attribute" | "attribute_list" if matches(child) => return true,
            "attribute" | "attribute_list" | "comment" => {}
            _ => break,
        }
        sibling = child.prev_sibling();
    }
    false
}

fn eval_named_procedure(
    root: Node<'_>,
    source: &[u8],
    codeunit_name: &str,
    proc_name: &str,
    ctx: &mut DispatchCtx,
) -> Eval {
    let Some(proc_node) = find_procedure_node(root, source, proc_name) else {
        return Eval::Error(al_runtime::interpreter::value::ErrorInfo {
            message: format!("procedure '{proc_name}' not found in '{codeunit_name}'"),
            error_type: None,
            source: None,
        });
    };
    let Some(body) = procedure_body(proc_node) else {
        return Eval::Error(al_runtime::interpreter::value::ErrorInfo {
            message: format!("procedure '{proc_name}' in '{codeunit_name}' has no body"),
            error_type: None,
            source: None,
        });
    };
    let mut stack = ScopeStack::new();
    let mut frame = CallFrame::new(codeunit_name, proc_name);
    al_runtime::interpreter::dispatch::bind_procedure_locals(proc_node, source, &mut frame);
    stack.push(frame);
    eval_stmt(body, source, &mut stack, ctx)
}

/// Locate the `begin_end_block` (body) of a named procedure.
///
/// Uses an iterative walk (no recursion). Returns the first matching
/// `begin_end_block` whose enclosing `procedure_declaration` has the given name.
/// Find the `procedure_declaration` node for `proc_name`. Returns the whole
/// declaration (not just its body) so the caller can both bind the procedure's
/// locals and locate its `begin_end_block` via [`procedure_body`].
fn find_procedure_node<'a>(root: Node<'a>, source: &[u8], proc_name: &str) -> Option<Node<'a>> {
    let mut stack_nodes = vec![root];
    while let Some(current) = stack_nodes.pop() {
        if current.kind() == "procedure_declaration" {
            if let Some(name_node) = current.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("").trim_matches('"');
                if name.eq_ignore_ascii_case(proc_name) {
                    return Some(current);
                }
            }
            let mut cursor = current.walk();
            for child in current.named_children(&mut cursor) {
                if matches!(child.kind(), "identifier" | "quoted_identifier") {
                    let name = child.utf8_text(source).unwrap_or("").trim_matches('"');
                    if name.eq_ignore_ascii_case(proc_name) {
                        return Some(current);
                    }
                }
                if !matches!(
                    child.kind(),
                    "identifier" | "quoted_identifier" | "attribute" | "attribute_list" | "comment"
                ) {
                    stack_nodes.push(child);
                }
            }
            continue;
        }
        let mut cursor = current.walk();
        stack_nodes.extend(current.named_children(&mut cursor));
    }
    None
}

/// The `begin_end_block` body of a `procedure_declaration`, if present.
fn procedure_body<'a>(proc_node: Node<'a>) -> Option<Node<'a>> {
    for i in 0..proc_node.named_child_count() {
        if let Some(child) = proc_node.named_child(i) {
            if child.kind() == "begin_end_block" {
                return Some(child);
            }
        }
    }
    None
}

async fn send_event(tx: &mpsc::Sender<TestEvent>, event: TestEvent) -> Result<(), TestRunnerError> {
    tx.send(event).await.map_err(|_| {
        warn!("interpreter test event channel closed; aborting run");
        TestRunnerError::ChannelClosed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::RunOptions;
    use tokio::sync::mpsc;

    fn make_session() -> InterpMode {
        InterpMode::new(Arc::new(Workspace::new()))
    }

    async fn collect_events(
        session: &InterpMode,
        tests: Vec<TestId>,
        opts: RunOptions,
    ) -> Vec<TestEvent> {
        let (tx, mut rx) = mpsc::channel(256);
        session
            .run(tests, opts, tx)
            .await
            .expect("run must succeed");
        let mut events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            events.push(evt);
        }
        events
    }

    /// 1-based line number of the first source line containing `needle`.
    fn line_of(src: &str, needle: &str) -> u32 {
        src.lines()
            .position(|l| l.contains(needle))
            .map(|i| i as u32 + 1)
            .unwrap_or_else(|| panic!("needle {needle:?} not found in source"))
    }

    #[tokio::test]
    async fn dynamic_mode_reports_executed_lines_and_branches() {
        let source = r#"codeunit 50110 "Cov Tests"
{
    Subtype = Test;

    [Test]
    procedure TestBranch()
    var
        x: Integer;
    begin
        x := 1;
        if x = 1 then
            x := 100
        else
            x := 200;
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/CovTests.al");
        workspace
            .file_index
            .add_file(path.clone(), source.to_string());

        let session = InterpMode::with_coverage(Arc::new(workspace));
        assert!(session.collects_coverage());

        let tests = vec![TestId {
            codeunit_id: 50110,
            codeunit_name: "Cov Tests".to_string(),
            method_name: Some("TestBranch".to_string()),
        }];

        let events = collect_events(&session, tests, RunOptions::default()).await;

        let passed = events.iter().any(|e| {
            matches!(e, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Pass)
        });
        assert!(passed, "TestBranch should pass; events: {events:?}");

        let report = session.coverage_report();
        assert_eq!(report.files.len(), 1, "exactly one file should be reported");
        let file = &report.files[0];

        let taken = line_of(source, "x := 100");
        let untaken = line_of(source, "x := 200");
        let if_line = line_of(source, "if x = 1");

        assert!(
            file.executed_lines.contains(&taken),
            "taken branch (x := 100) must be in the report; got {:?}",
            file.executed_lines
        );
        assert!(
            !file.executed_lines.contains(&untaken),
            "not-taken branch (x := 200) must NOT be in the report; got {:?}",
            file.executed_lines
        );

        let branch = file
            .branches
            .iter()
            .find(|b| b.line == if_line)
            .expect("if branch decision recorded");
        assert_eq!(branch.then_taken, 1);
        assert_eq!(branch.else_taken, 0);
    }

    #[tokio::test]
    async fn record_mode_executes_workspace_table_operations_and_pure_mode_rejects_them() {
        let table_source = r#"table 50130 "Native Entry"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Integer) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}
"#;
        let test_source = r#"codeunit 50131 "Record Runtime Tests"
{
    Subtype = Test;

    [Test]
    procedure InsertAndRead()
    var
        Entry: Record "Native Entry";
    begin
        Entry.Init();
        Entry."No." := 'A';
        Entry.Amount := 42;
        Entry.Insert();
        Entry.Reset();
        if Entry.Count() <> 1 then
            Error('expected one record');
        if not Entry.Get('A') then
            Error('record not found');
        if Entry.Amount <> 42 then
            Error('amount was %1', Entry.Amount);
    end;
}
"#;
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/NativeEntry.Table.al"),
            table_source.to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/RecordRuntimeTests.Codeunit.al"),
            test_source.to_string(),
        );
        let workspace = Arc::new(workspace);
        let target = TestId {
            codeunit_id: 50131,
            codeunit_name: "Record Runtime Tests".to_string(),
            method_name: Some("InsertAndRead".to_string()),
        };

        let record_events = collect_events(
            &InterpMode::with_records(Arc::clone(&workspace)),
            vec![target.clone()],
            RunOptions::default(),
        )
        .await;
        assert!(record_events.iter().any(|event| {
            matches!(event, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Pass)
        }), "record-backed interpreter should pass: {record_events:?}");

        let pure_events = collect_events(
            &InterpMode::new(workspace),
            vec![target],
            RunOptions::default(),
        )
        .await;
        assert!(
            pure_events.iter().any(|event| {
                matches!(event, TestEvent::CaseResult { result, .. }
                if result.status == TestStatus::Fail
                    && result.error.as_deref().is_some_and(|e| e.contains("pure-logic")))
            }),
            "pure interpreter must enforce its record boundary: {pure_events:?}"
        );
    }

    #[tokio::test]
    async fn uninitialized_local_uses_default_value() {
        let source = r#"codeunit 50120 "Uninit Tests"
{
    Subtype = Test;

    [Test]
    procedure ReadsUnassignedLocal()
    var
        t: Text;
        u: Text;
    begin
        u := 'x' + t + 'y';
        if u <> 'xy' then
            Error('expected xy');
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/UninitTests.al");
        workspace.file_index.add_file(path, source.to_string());

        let session = InterpMode::new(Arc::new(workspace));
        let tests = vec![TestId {
            codeunit_id: 50120,
            codeunit_name: "Uninit Tests".to_string(),
            method_name: Some("ReadsUnassignedLocal".to_string()),
        }];

        let events = collect_events(&session, tests, RunOptions::default()).await;
        let passed = events.iter().any(|e| {
            matches!(e, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Pass)
        });
        assert!(
            passed,
            "reading a zero-initialized local must pass, not error; events: {events:?}"
        );
    }

    #[tokio::test]
    async fn static_mode_collects_no_dynamic_coverage() {
        let source = r#"codeunit 50111 "Plain Tests"
{
    Subtype = Test;

    [Test]
    procedure TestPlain()
    var
        x: Integer;
    begin
        x := 1;
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/PlainTests.al");
        workspace
            .file_index
            .add_file(path.clone(), source.to_string());

        let session = make_session(); // InterpMode::new — static mode
        assert!(!session.collects_coverage());

        let tests = vec![TestId {
            codeunit_id: 50111,
            codeunit_name: "Plain Tests".to_string(),
            method_name: Some("TestPlain".to_string()),
        }];

        let _events = collect_events(&session, tests, RunOptions::default()).await;

        assert!(
            session.coverage_report().is_empty(),
            "static mode must not produce dynamic coverage"
        );
    }

    #[tokio::test]
    async fn empty_tests_emits_only_session_complete() {
        let session = make_session();
        let events = collect_events(&session, vec![], RunOptions::default()).await;
        assert_eq!(
            events.len(),
            1,
            "expected exactly 1 event (SessionComplete), got {}",
            events.len()
        );
        match &events[0] {
            TestEvent::SessionComplete {
                total,
                passed,
                failed,
                skipped,
            } => {
                assert_eq!(*total, 0);
                assert_eq!(*passed, 0);
                assert_eq!(*failed, 0);
                assert_eq!(*skipped, 0);
            }
            other => panic!("expected SessionComplete, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unknown_codeunit_emits_fail_and_session_complete() {
        let session = make_session();
        let tests = vec![TestId {
            codeunit_id: 99999,
            codeunit_name: "NonExistentCU".to_string(),
            method_name: Some("TestProc".to_string()),
        }];
        let events = collect_events(&session, tests, RunOptions::default()).await;
        let last = events.last().expect("must have at least one event");
        assert!(
            matches!(last, TestEvent::SessionComplete { .. }),
            "last event must be SessionComplete; got {:?}",
            last
        );
        let fail_result = events.iter().find(|e| matches!(e, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Fail));
        assert!(
            fail_result.is_some(),
            "expected a Fail CaseResult for unknown codeunit"
        );
    }

    #[tokio::test]
    async fn library_assert_fixture_passes_in_interpreter() {
        let source = r#"codeunit 50101 "My Assert Tests"
{
    Subtype = Test;

    [Test]
    procedure TestAssertEqual()
    begin
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/TestAssertFixture.al");
        workspace
            .file_index
            .add_file(path.clone(), source.to_string());

        let session = InterpMode::new(Arc::new(workspace));
        let tests = vec![TestId {
            codeunit_id: 50101,
            codeunit_name: "My Assert Tests".to_string(),
            method_name: Some("TestAssertEqual".to_string()),
        }];

        let (tx, mut rx) = mpsc::channel(256);
        session
            .run(tests, RunOptions::default(), tx)
            .await
            .expect("run must succeed");

        let mut events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            events.push(evt);
        }

        let case_result = events
            .iter()
            .find(|e| matches!(e, TestEvent::CaseResult { .. }));

        match case_result {
            Some(TestEvent::CaseResult { result, .. }) => {
                assert_eq!(
                    result.status,
                    TestStatus::Pass,
                    "empty test body should pass; error: {:?}",
                    result.error
                );
            }
            None => {
                let session_complete = events
                    .iter()
                    .find(|e| matches!(e, TestEvent::SessionComplete { .. }));
                assert!(session_complete.is_some(), "must emit SessionComplete");
            }
            Some(other) => panic!("unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn duplicate_test_ids_deduplicated() {
        let source = r#"codeunit 50103 "Dup Tests"
{
    Subtype = Test;

    [Test]
    procedure TestPasses()
    begin
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/DupTests.al");
        workspace
            .file_index
            .add_file(path.clone(), source.to_string());

        let session = InterpMode::new(Arc::new(workspace));
        let one = TestId {
            codeunit_id: 50103,
            codeunit_name: "Dup Tests".to_string(),
            method_name: Some("TestPasses".to_string()),
        };
        let tests = vec![one.clone(), one];

        let events = collect_events(&session, tests, RunOptions::default()).await;

        let case_results = events
            .iter()
            .filter(|e| matches!(e, TestEvent::CaseResult { .. }))
            .count();
        assert_eq!(
            case_results, 1,
            "duplicate TestId must yield one CaseResult, got {events:?}"
        );

        match events.last() {
            Some(TestEvent::SessionComplete { total, .. }) => {
                assert_eq!(*total, 1, "total must not be inflated by the duplicate");
            }
            other => panic!("expected SessionComplete last, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failing_assert_are_equal_emits_fail_with_expected_in_error() {
        let source = r#"codeunit 50102 "Fail Tests"
{
    Subtype = Test;

    [Test]
    procedure TestExpectedFail()
    begin
        error('expected failure');
    end;
}
"#;
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/FailTests.al");
        workspace
            .file_index
            .add_file(path.clone(), source.to_string());

        let session = InterpMode::new(Arc::new(workspace));
        let tests = vec![TestId {
            codeunit_id: 50102,
            codeunit_name: "Fail Tests".to_string(),
            method_name: Some("TestExpectedFail".to_string()),
        }];

        let (tx, mut rx) = mpsc::channel(256);
        session
            .run(tests, RunOptions::default(), tx)
            .await
            .expect("run must succeed");

        let mut events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            events.push(evt);
        }

        let case_result = events
            .iter()
            .find(|e| matches!(e, TestEvent::CaseResult { .. }));

        match case_result {
            Some(TestEvent::CaseResult { result, .. }) => {
                assert_eq!(
                    result.status,
                    TestStatus::Fail,
                    "expected Fail status; got {:?}",
                    result
                );
            }
            None => {
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e, TestEvent::SessionComplete { .. })),
                    "must emit SessionComplete even if no tests found"
                );
            }
            Some(other) => panic!("unexpected CaseResult shape: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_initialize_shares_record_context_with_test_body() {
        let table = r#"table 50140 "Lifecycle Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}"#;
        let tests_source = r#"codeunit 50141 "Lifecycle Tests"
{
    Subtype = Test;

    [TestInitialize]
    procedure SetUp()
    var Entry: Record "Lifecycle Entry";
    begin
        Entry."No." := 'INIT';
        Entry.Insert();
    end;

    [Test]
    procedure SeesInitializedRecord()
    var Entry: Record "Lifecycle Entry";
    begin
        if not Entry.Get('INIT') then
            Error('initializer record was not visible');
    end;
}"#;
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/LifecycleEntry.Table.al"),
            table.to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/LifecycleTests.Codeunit.al"),
            tests_source.to_string(),
        );
        let session = InterpMode::with_records(Arc::new(workspace));
        let events = collect_events(
            &session,
            vec![TestId {
                codeunit_id: 50141,
                codeunit_name: "Lifecycle Tests".to_string(),
                method_name: Some("SeesInitializedRecord".to_string()),
            }],
            RunOptions::default(),
        )
        .await;
        assert!(events.iter().any(|event| {
            matches!(event, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Pass)
        }), "initializer and test must share the record store: {events:?}");
    }

    #[tokio::test]
    async fn test_cleanup_runs_and_its_failure_is_reported() {
        let source = r#"codeunit 50142 "Cleanup Tests"
{
    Subtype = Test;
    [Test]
    procedure BodyPasses() begin end;

    [TestCleanup]
    procedure TearDown()
    begin
        Error('cleanup executed');
    end;
}"#;
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/CleanupTests.Codeunit.al"),
            source.to_string(),
        );
        let events = collect_events(
            &InterpMode::new(Arc::new(workspace)),
            vec![TestId {
                codeunit_id: 50142,
                codeunit_name: "Cleanup Tests".to_string(),
                method_name: Some("BodyPasses".to_string()),
            }],
            RunOptions::default(),
        )
        .await;
        assert!(events.iter().any(|event| {
            matches!(event, TestEvent::CaseResult { result, .. }
                if result.status == TestStatus::Fail
                    && result.error.as_deref().is_some_and(|error| error.contains("cleanup executed")))
        }), "cleanup failure must fail the test: {events:?}");
    }

    #[tokio::test]
    async fn message_and_confirm_handlers_execute_locally() {
        let source = r#"codeunit 50143 "Handler Tests"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('HandleMessage,HandleConfirm')]
    procedure Dialogs()
    begin
        Message('Hello %1', 'world');
        if not Confirm('Proceed?') then
            Error('confirm handler did not reply true');
    end;

    [MessageHandler]
    procedure HandleMessage(MessageText: Text[1024])
    begin
        if MessageText <> 'Hello world' then
            Error('wrong message: %1', MessageText);
    end;

    [ConfirmHandler]
    procedure HandleConfirm(Question: Text[1024]; var Reply: Boolean)
    begin
        if Question <> 'Proceed?' then
            Error('wrong question: %1', Question);
        Reply := true;
    end;
}"#;
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/HandlerTests.Codeunit.al"),
            source.to_string(),
        );
        let events = collect_events(
            &InterpMode::new(Arc::new(workspace)),
            vec![TestId {
                codeunit_id: 50143,
                codeunit_name: "Handler Tests".to_string(),
                method_name: Some("Dialogs".to_string()),
            }],
            RunOptions::default(),
        )
        .await;
        assert!(events.iter().any(|event| {
            matches!(event, TestEvent::CaseResult { result, .. } if result.status == TestStatus::Pass)
        }), "dialog handlers should execute and validate calls: {events:?}");
    }
}
