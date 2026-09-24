//! Static-analysis router for selecting an AL test backend.
//!
//! Decides which `TestSession` backend a discovered test should run on:
//! the local Rust interpreter, the interpreter + mock record store, or
//! the live BC dev API. The router is **conservative by design**: when
//! in doubt it routes UP toward the more capable backend. False
//! positives (routing a pure-logic test to live BC) are tolerable —
//! correctness is preserved, just slower. False negatives (routing a
//! DB-touching test to the empty interpreter) are NOT tolerable — the
//! interpreter would silently miss DB operations.
//!
//! The classifier walks each test procedure's reachable call-graph
//! plus its own AST and looks for disqualifying signals:
//!
//! * Record CRUD (`Insert`/`Modify`/`Delete`/`Validate`) — needs the
//!   mock record store.
//! * `HttpClient`-typed variables, `Page.RunModal`, `TestPage`, `Report`
//!   construction, `XmlPort` use — requires mocks not currently available.
//! * `Commit` / `Rollback` — requires transaction semantics.
//! * `Codeunit.Run` / `CODEUNIT.RUN` — polymorphic dispatch; even with
//!   a literal argument, the called codeunit's body may touch DB.
//! * Event subscribers whose source body is in the workspace or embedded in a
//!   loaded `.app` are followed; genuinely source-free package procedures
//!   without a native stub force `LiveBc`.
//!
//! Anything outside the safe-list routes to `LiveBc`.

use al_syntax::IdentifierText;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use al_analysis::queries::tests::TestCodeunit;
use al_insight::graph::NodeKey;
use al_insight::index::{CallGraph, EdgeKind, NodeId};
use al_symbols::ObjectKind;
use al_workspace::Workspace;

// "Which tests must I re-run after changing these files?" is answered by
// **call-graph reachability**: a test is affected iff it transitively calls a
// procedure/event of a changed object (not merely because its own file
// changed). The implementation lives in `al_analysis::queries::tests` because
// it needs the insight call graph; it is re-exported here so the test engine is
// the single entry point for both *routing* (which backend) and *selection*
// (which tests). [`affected_tests_detailed`] reports [`AffectedMode`] so output
// stays honest about whether the graph path ran or it fell back to file-name
// matching.
pub use al_analysis::queries::tests::{
    affected_tests, affected_tests_detailed, AffectedMode, AffectedTest, AffectedTestsResult,
};

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error(transparent)]
    TestQuery(#[from] al_analysis::queries::tests::TestQueryError),
    #[error(transparent)]
    CallGraph(#[from] al_workspace::CallGraphBuildError),
    #[error(transparent)]
    SourceGraph(#[from] al_insight::calls::SourceGraphError),
    #[error("test codeunit source '{}' is absent from the workspace object index", path.display())]
    MissingObjectInfo { path: PathBuf },
    #[error("test codeunit source '{}' has unsupported object kind '{kind}'", path.display())]
    InvalidObjectKind { path: PathBuf, kind: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Pure-logic — runs on the Rust interpreter alone.
    Interp,
    /// DB-touching test — runs locally on the interpreter with an isolated
    /// in-memory record store. Only workspace-defined tables and the supported
    /// record/FlowField subset are eligible; unsupported platform semantics
    /// remain `LiveBc`.
    InterpRecord,
    /// Anything risky / not yet supported — runs against live BC.
    LiveBc,
}

impl RoutingDecision {
    /// Stable string form for wire serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            RoutingDecision::Interp => "interp",
            RoutingDecision::InterpRecord => "interpRecord",
            RoutingDecision::LiveBc => "liveBc",
        }
    }

    /// Whether a test with this decision **actually executes locally** today
    /// (pure Rust interpreter, no Business Central server contact).
    ///
    /// Both interpreter tiers run locally; `LiveBc` requires an external
    /// runtime.
    pub fn runs_locally(self) -> bool {
        matches!(
            self,
            RoutingDecision::Interp | RoutingDecision::InterpRecord
        )
    }

    /// One-line, user-facing description of where a test with this decision
    /// *actually* runs today. Surfaced by `al-explorer
    /// test-classify` so the routing surface stays honest.
    pub fn execution_note(self) -> &'static str {
        match self {
            RoutingDecision::Interp => "runs locally on the Rust interpreter",
            RoutingDecision::InterpRecord => "runs locally with the in-memory record runtime",
            RoutingDecision::LiveBc => "routes to live BC",
        }
    }
}

/// Reasons the classifier elected a particular decision. Useful for the
/// `tests.classify` daemon endpoint and for debugging routing surprises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingReason {
    /// Human-readable phrase ("calls Insert on Customer", ...).
    pub message: String,
    /// File path where the disqualifying call appears, if any.
    pub file: Option<String>,
    /// Line number of the disqualifying call, if available.
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub codeunit_id: i32,
    pub codeunit_name: String,
    pub method_name: String,
    pub decision: RoutingDecision,
    /// All disqualifying signals encountered. Empty for `Interp`.
    pub reasons: Vec<RoutingReason>,
}

#[derive(Debug, Clone)]
struct ProcedureLocation {
    file: PathBuf,
    object: String,
    name: String,
    has_object_globals: bool,
}

type ProcedureCatalog = HashMap<(String, String), ProcedureLocation>;

#[derive(Debug, Clone, Copy, Default)]
struct LocalHandlerSupport {
    message: bool,
    confirm: bool,
    str_menu: bool,
    hyperlink: bool,
}

impl LocalHandlerSupport {
    fn supports(self, operation: &str) -> bool {
        match operation.to_ascii_lowercase().as_str() {
            "message" => self.message,
            "confirm" => self.confirm,
            "strmenu" => self.str_menu,
            "hyperlink" => self.hyperlink,
            _ => false,
        }
    }
}

const PLATFORM_TYPES: &[&str] = &[
    "httpclient",
    "httprequestmessage",
    "httpresponsemessage",
    "testpage",
    "testrequestpage",
    "recordref",
    "fieldref",
    "session",
    "notification",
];

const PLATFORM_GLOBALS: &[&str] = &[
    "commit",
    "rollback",
    "startsession",
    "starttask",
    "report",
    "xmlport",
    "page",
];

/// Classify every discovered test in the workspace using the fully-resolved
/// workspace call/event graph and syntax nodes from every reachable body.
pub fn classify_all(workspace: &Workspace) -> Result<Vec<ClassifyResult>, RoutingError> {
    let codeunits = al_analysis::queries::tests::discover_tests(workspace)?;
    classify_codeunits(workspace, &codeunits)
}

/// Classify a known set of test codeunits (so callers that already
/// computed `discover_tests` don't pay for it twice).
pub fn classify_codeunits(
    workspace: &Workspace,
    codeunits: &[TestCodeunit],
) -> Result<Vec<ClassifyResult>, RoutingError> {
    let (insight, cached_graph) = workspace.get_or_build_call_graph()?;
    drop(cached_graph);
    let mut graph = CallGraph::build_from_insight(&insight);
    al_insight::calls::resolve_all_workspace_call_edges(
        &workspace.file_index,
        &workspace.symbols,
        &insight,
        &mut graph,
    )?;
    let catalog = build_procedure_catalog(workspace);
    let mut out = Vec::new();
    for cu in codeunits {
        let codeunit_start = out.len();
        let path = std::path::Path::new(&cu.file);
        let info = workspace.file_index.object_info.get(path).ok_or_else(|| {
            RoutingError::MissingObjectInfo {
                path: path.to_path_buf(),
            }
        })?;
        let kind =
            info.kind
                .parse::<ObjectKind>()
                .map_err(|_| RoutingError::InvalidObjectKind {
                    path: path.to_path_buf(),
                    kind: info.kind.clone(),
                })?;
        for proc in &cu.tests {
            let (handler_support, handler_reasons) = local_handler_support(workspace, cu, proc);
            let key = NodeKey::Procedure(
                kind,
                cu.name.to_ascii_lowercase(),
                proc.name.to_ascii_lowercase(),
            );
            let Some(root) = CallGraph::node_id_for(&insight, &key) else {
                out.push(conservative_result(
                    cu,
                    proc,
                    "test procedure is absent from the resolved call graph",
                ));
                continue;
            };
            let (mut decision, mut reasons) =
                classify_reachable(workspace, &graph, &catalog, root, handler_support);
            for reason in handler_reasons {
                decision = RoutingDecision::LiveBc;
                push_reason(&mut reasons, reason);
            }
            for lifecycle in cu.test_initializers.iter().chain(&cu.test_cleanups) {
                let lifecycle_key = NodeKey::Procedure(
                    kind,
                    cu.name.to_ascii_lowercase(),
                    lifecycle.name.to_ascii_lowercase(),
                );
                let Some(lifecycle_root) = CallGraph::node_id_for(&insight, &lifecycle_key) else {
                    decision = RoutingDecision::LiveBc;
                    push_reason(
                        &mut reasons,
                        RoutingReason {
                            message: format!(
                                "lifecycle procedure '{}' is absent from the resolved call graph",
                                lifecycle.name
                            ),
                            file: Some(cu.file.clone()),
                            line: Some(lifecycle.line),
                        },
                    );
                    continue;
                };
                let (lifecycle_decision, lifecycle_reasons) = classify_reachable(
                    workspace,
                    &graph,
                    &catalog,
                    lifecycle_root,
                    handler_support,
                );
                decision = decision.max(lifecycle_decision);
                for reason in lifecycle_reasons {
                    push_reason(&mut reasons, reason);
                }
            }
            for handler in &proc.handler_functions {
                let handler_key = NodeKey::Procedure(
                    kind,
                    cu.name.to_ascii_lowercase(),
                    handler.to_ascii_lowercase(),
                );
                let Some(handler_root) = CallGraph::node_id_for(&insight, &handler_key) else {
                    decision = RoutingDecision::LiveBc;
                    push_reason(
                        &mut reasons,
                        RoutingReason {
                            message: format!(
                                "configured handler procedure '{handler}' is absent from the resolved call graph"
                            ),
                            file: Some(cu.file.clone()),
                            line: Some(proc.line),
                        },
                    );
                    continue;
                };
                let (handler_decision, handler_reasons) =
                    classify_reachable(workspace, &graph, &catalog, handler_root, handler_support);
                decision = decision.max(handler_decision);
                for reason in handler_reasons {
                    push_reason(&mut reasons, reason);
                }
            }
            out.push(ClassifyResult {
                codeunit_id: cu.id,
                codeunit_name: cu.name.clone(),
                method_name: proc.name.clone(),
                decision,
                reasons,
            });
        }
        let codeunit_decision = out[codeunit_start..]
            .iter()
            .fold(RoutingDecision::Interp, |decision, result| {
                decision.max(result.decision)
            });
        for result in &mut out[codeunit_start..] {
            if result.decision != codeunit_decision {
                result.decision = codeunit_decision;
                push_reason(
                    &mut result.reasons,
                    RoutingReason {
                        message: format!(
                            "another test in codeunit '{}' requires {}; shared globals and codeunit lifecycle keep every method on one backend",
                            cu.name,
                            codeunit_decision.as_str()
                        ),
                        file: Some(cu.file.clone()),
                        line: cu
                            .tests
                            .iter()
                            .find(|test| test.name.eq_ignore_ascii_case(&result.method_name))
                            .map(|test| test.line),
                    },
                );
            }
        }
    }
    Ok(out)
}

fn conservative_result(
    cu: &TestCodeunit,
    proc: &al_analysis::queries::tests::TestProcedure,
    message: &str,
) -> ClassifyResult {
    ClassifyResult {
        codeunit_id: cu.id,
        codeunit_name: cu.name.clone(),
        method_name: proc.name.clone(),
        decision: RoutingDecision::LiveBc,
        reasons: vec![RoutingReason {
            message: message.to_string(),
            file: Some(cu.file.clone()),
            line: Some(proc.line),
        }],
    }
}

fn build_procedure_catalog(workspace: &Workspace) -> ProcedureCatalog {
    let mut catalog = HashMap::new();
    for entry in workspace.file_index.object_info.iter() {
        let path = entry.key().clone();
        let object = entry.value().name.to_ascii_lowercase();
        let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };
        let bytes = text.as_bytes();
        let has_object_globals = has_object_global_declarations(tree.root_node());
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "procedure_declaration" | "trigger_declaration" | "event_declaration"
            ) {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(bytes).ok())
                {
                    let clean = name.unquote_identifier().into_owned();
                    catalog.insert(
                        (object.clone(), clean.to_ascii_lowercase()),
                        ProcedureLocation {
                            file: path.clone(),
                            object: entry.value().name.clone(),
                            name: clean,
                            has_object_globals,
                        },
                    );
                }
                continue;
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
    }
    catalog
}

fn classify_reachable(
    workspace: &Workspace,
    graph: &CallGraph,
    catalog: &ProcedureCatalog,
    root: NodeId,
    handler_support: LocalHandlerSupport,
) -> (RoutingDecision, Vec<RoutingReason>) {
    let mut decision = RoutingDecision::Interp;
    let mut reasons = Vec::new();
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let root_object = graph
        .node_info(root)
        .map(|info| info.object.to_ascii_lowercase());
    while let Some(node) = queue.pop_front() {
        let Some(info) = graph.node_info(node) else {
            decision = RoutingDecision::LiveBc;
            push_reason(
                &mut reasons,
                RoutingReason {
                    message: format!(
                        "reachable call-graph node {} has no metadata; routing conservatively",
                        node.0
                    ),
                    file: None,
                    line: None,
                },
            );
            continue;
        };
        let key = (
            info.object.to_ascii_lowercase(),
            info.name.to_ascii_lowercase(),
        );
        if let Some(location) = catalog.get(&key) {
            if location.has_object_globals
                && !root_object
                    .as_deref()
                    .is_some_and(|root| root.eq_ignore_ascii_case(&location.object))
            {
                decision = RoutingDecision::LiveBc;
                push_reason(
                    &mut reasons,
                    RoutingReason {
                        message: format!(
                            "reachable helper codeunit '{}' has object-level state that requires live BC execution",
                            location.object
                        ),
                        file: Some(location.file.to_string_lossy().into_owned()),
                        line: None,
                    },
                );
            }
            classify_procedure_ast(
                workspace,
                catalog,
                location,
                &mut decision,
                &mut reasons,
                node != root,
                handler_support,
            );
        } else if !matches!(info.node_type.as_str(), "event" | "object")
            && !al_runtime::stubs::is_supported(&info.object, &info.name)
        {
            decision = RoutingDecision::LiveBc;
            push_reason(
                &mut reasons,
                RoutingReason {
                    message: format!(
                        "reachable dependency procedure '{}::{}' has no executable workspace body or native stub",
                        info.object, info.name
                    ),
                    file: None,
                    line: None,
                },
            );
        }
        for edge in graph
            .callees_of(node)
            .iter()
            .filter(|edge| edge.kind != EdgeKind::EventSubscription)
        {
            if visited.insert(edge.to) {
                queue.push_back(edge.to);
            }
        }
    }
    (decision, reasons)
}

fn local_handler_support(
    workspace: &Workspace,
    codeunit: &TestCodeunit,
    procedure: &al_analysis::queries::tests::TestProcedure,
) -> (LocalHandlerSupport, Vec<RoutingReason>) {
    let mut support = LocalHandlerSupport::default();
    let mut reasons = Vec::new();
    if procedure.handler_functions.is_empty() {
        return (support, reasons);
    }

    let path = std::path::Path::new(&codeunit.file);
    let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
        reasons.push(RoutingReason {
            message:
                "cannot inspect configured test handlers because the codeunit parse is unavailable"
                    .to_string(),
            file: Some(codeunit.file.clone()),
            line: Some(procedure.line),
        });
        return (support, reasons);
    };
    let source = text.as_bytes();
    for handler_name in &procedure.handler_functions {
        let Some(handler) = find_callable_node(tree.root_node(), source, handler_name) else {
            reasons.push(RoutingReason {
                message: format!(
                    "configured handler procedure '{handler_name}' is missing from the test codeunit"
                ),
                file: Some(codeunit.file.clone()),
                line: Some(procedure.line),
            });
            continue;
        };
        let matches = [
            (
                "MessageHandler",
                callable_has_attribute(handler, source, "MessageHandler"),
            ),
            (
                "ConfirmHandler",
                callable_has_attribute(handler, source, "ConfirmHandler"),
            ),
            (
                "StrMenuHandler",
                callable_has_attribute(handler, source, "StrMenuHandler"),
            ),
            (
                "HyperlinkHandler",
                callable_has_attribute(handler, source, "HyperlinkHandler"),
            ),
        ];
        let matched: Vec<_> = matches
            .into_iter()
            .filter_map(|(kind, present)| present.then_some(kind))
            .collect();
        let [kind] = matched.as_slice() else {
            reasons.push(RoutingReason {
                message: format!(
                    "configured handler '{handler_name}' must declare exactly one supported local handler attribute; found {}",
                    matched.len()
                ),
                file: Some(codeunit.file.clone()),
                line: Some(handler.start_position().row as u32 + 1),
            });
            continue;
        };
        let slot = match *kind {
            "MessageHandler" => &mut support.message,
            "ConfirmHandler" => &mut support.confirm,
            "StrMenuHandler" => &mut support.str_menu,
            "HyperlinkHandler" => &mut support.hyperlink,
            _ => unreachable!("matched from the fixed supported handler set"),
        };
        if *slot {
            reasons.push(RoutingReason {
                message: format!(
                    "test configures more than one {kind}; the local runtime cannot choose one deterministically"
                ),
                file: Some(codeunit.file.clone()),
                line: Some(handler.start_position().row as u32 + 1),
            });
        } else {
            *slot = true;
        }
    }
    (support, reasons)
}

fn callable_has_attribute(callable: tree_sitter::Node<'_>, source: &[u8], wanted: &str) -> bool {
    let matches = |attribute: tree_sitter::Node<'_>| {
        attribute.utf8_text(source).ok().is_some_and(|text| {
            text.trim()
                .trim_start_matches('[')
                .split(['(', ';', ']'])
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case(wanted))
        })
    };
    let mut cursor = callable.walk();
    if callable
        .children(&mut cursor)
        .any(|child| matches!(child.kind(), "attribute" | "attribute_list") && matches(child))
    {
        return true;
    }
    let mut sibling = callable.prev_sibling();
    while let Some(node) = sibling {
        match node.kind() {
            "attribute" | "attribute_list" if matches(node) => return true,
            "attribute" | "attribute_list" | "comment" => {}
            _ => break,
        }
        sibling = node.prev_sibling();
    }
    false
}

fn has_object_global_declarations(root: tree_sitter::Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_var_section" {
            let mut cursor = node.walk();
            return node.named_children(&mut cursor).next().is_some();
        }
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_declaration"
        ) {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

mod ast;
#[cfg(test)]
mod tests;

use ast::*;

#[cfg(test)]
fn classify_body(body: &str) -> (RoutingDecision, Vec<RoutingReason>) {
    let wrapped = format!("codeunit 50100 X {{ {body} }}");
    let parsed = al_syntax::AlParser::parse_quick(&wrapped);
    let bytes = wrapped.as_bytes();
    let mut decision = RoutingDecision::Interp;
    let mut reasons = Vec::new();
    let mut stack = vec![parsed.tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(type_node) = (node.kind() == "regular_variable_declaration")
            .then(|| node.child_by_field_name("type"))
            .flatten()
        {
            let ty = type_node.utf8_text(bytes).unwrap_or("");
            if PLATFORM_TYPES.iter().any(|candidate| {
                ty.split_whitespace()
                    .next()
                    .is_some_and(|part| part.eq_ignore_ascii_case(candidate))
            }) {
                decision = RoutingDecision::LiveBc;
            }
        }
        if node.kind() == "postfix_expression" {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            if children.len() == 1 {
                let name = children[0].utf8_text(bytes).unwrap_or("").trim();
                if PLATFORM_GLOBALS
                    .iter()
                    .any(|candidate| name.eq_ignore_ascii_case(candidate))
                {
                    decision = RoutingDecision::LiveBc;
                    reasons.push(RoutingReason {
                        message: format!("calls {name}"),
                        file: None,
                        line: None,
                    });
                }
            }
            if let Some(last) = children.last() {
                let method = match last.kind() {
                    "member_call_suffix" | "scope_call_suffix" => last
                        .child_by_field_name("member")
                        .and_then(|member| member.utf8_text(bytes).ok()),
                    "call_suffix" => children.first().and_then(|name| name.utf8_text(bytes).ok()),
                    _ => None,
                };
                if let Some(method) = method.map(str::trim) {
                    if al_runtime::interpreter::records::supports_record_method(method) {
                        decision = decision.max(RoutingDecision::InterpRecord);
                        reasons.push(RoutingReason {
                            message: format!("calls {method}"),
                            file: None,
                            line: None,
                        });
                    }
                    if PLATFORM_GLOBALS
                        .iter()
                        .any(|candidate| method.eq_ignore_ascii_case(candidate))
                        || matches!(method.to_ascii_lowercase().as_str(), "run" | "runmodal")
                    {
                        decision = RoutingDecision::LiveBc;
                        reasons.push(RoutingReason {
                            message: format!("calls {method}"),
                            file: None,
                            line: None,
                        });
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    (decision, reasons)
}

#[cfg(test)]
fn record_subtypes(body: &str) -> Vec<String> {
    let wrapped = format!("codeunit 50100 X {{ {body} }}");
    let parsed = al_syntax::AlParser::parse_quick(&wrapped);
    let bytes = wrapped.as_bytes();
    let mut out = Vec::new();
    let mut stack = vec![parsed.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "regular_variable_declaration" {
            if let Some(text) = node
                .child_by_field_name("type")
                .and_then(|ty| ty.utf8_text(bytes).ok())
                .filter(|ty| {
                    ty.split_whitespace()
                        .next()
                        .is_some_and(|part| part.eq_ignore_ascii_case("record"))
                })
            {
                let subtype = text["record".len()..].unquote_identifier().into_owned();
                if !subtype.is_empty()
                    && !out
                        .iter()
                        .any(|seen: &String| seen.eq_ignore_ascii_case(&subtype))
                {
                    out.push(subtype);
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out.sort_by_key(|name| name.to_ascii_lowercase());
    out
}

/// `RoutingDecision::max` — pick the more conservative (capability-rich)
/// option. Implemented manually so we don't need to derive `Ord`.
impl RoutingDecision {
    fn max(self, other: RoutingDecision) -> RoutingDecision {
        fn rank(d: RoutingDecision) -> u8 {
            match d {
                RoutingDecision::Interp => 0,
                RoutingDecision::InterpRecord => 1,
                RoutingDecision::LiveBc => 2,
            }
        }
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

/// End-to-end through the test-engine entry point: changing a helper's file
/// must select the test that calls it via call-graph reachability, and report
/// `CallGraph` mode. (Exhaustive cases live in `al_analysis::queries::tests`.)
#[cfg(test)]
mod affected_smoke {
    use super::{affected_tests_detailed, AffectedMode};
    use al_workspace::Workspace;
    use std::path::PathBuf;

    #[test]
    fn affected_selection_uses_call_graph_reachability() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/ws/helper.al"),
            "codeunit 50100 Helper\n{\n    procedure DoWork()\n    begin\n    end;\n}\n"
                .to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/ws/tests.al"),
            concat!(
                "codeunit 50101 MyTests\n{\n    Subtype = Test;\n\n",
                "    [Test]\n    procedure TestCallsHelper()\n",
                "    var\n        H: Codeunit Helper;\n",
                "    begin\n        H.DoWork();\n    end;\n}\n"
            )
            .to_string(),
        );

        let changed = vec!["/ws/helper.al".to_string()];
        let result = affected_tests_detailed(&ws, &changed).unwrap();

        assert_eq!(result.mode, AffectedMode::CallGraph);
        assert_eq!(result.tests.len(), 1, "got {:?}", result.tests);
        assert_eq!(result.tests[0].method_name, "TestCallsHelper");
    }
}
