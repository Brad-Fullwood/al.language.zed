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
                    let clean = name.trim().trim_matches('"').to_string();
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
                location,
                &mut decision,
                &mut reasons,
                node != root,
                handler_support,
            );
        } else if !matches!(info.node_type.as_str(), "event" | "object")
            && al_runtime::stubs::resolve(&info.object, &info.name).is_none()
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

fn classify_procedure_ast(
    workspace: &Workspace,
    location: &ProcedureLocation,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    reachable: bool,
    handler_support: LocalHandlerSupport,
) {
    let Some((text, tree)) = workspace.file_index.get_cached_parse(&location.file) else {
        *decision = RoutingDecision::LiveBc;
        push_reason(
            reasons,
            RoutingReason {
                message: format!(
                    "no cached parse for reachable procedure '{}'; routing conservatively",
                    location.name
                ),
                file: Some(location.file.to_string_lossy().into_owned()),
                line: None,
            },
        );
        return;
    };
    let bytes = text.as_bytes();
    let Some(procedure) = find_callable_node(tree.root_node(), bytes, &location.name) else {
        *decision = RoutingDecision::LiveBc;
        push_reason(
            reasons,
            RoutingReason {
                message: format!(
                    "cannot locate syntax node for reachable procedure '{}'; routing conservatively",
                    location.name
                ),
                file: Some(location.file.to_string_lossy().into_owned()),
                line: None,
            },
        );
        return;
    };
    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    let mut stack = vec![procedure];
    while let Some(node) = stack.pop() {
        if node != procedure
            && matches!(
                node.kind(),
                "procedure_declaration" | "trigger_declaration" | "event_declaration"
            )
        {
            continue;
        }
        if node.kind() == "regular_variable_declaration" || node.kind() == "parameter" {
            if let Some(type_node) = node.child_by_field_name("type") {
                classify_type_reference(
                    workspace,
                    type_node,
                    bytes,
                    &location.file,
                    decision,
                    reasons,
                    reachable,
                );
            }
        } else if node.kind() == "postfix_expression" {
            classify_call(
                workspace,
                &resolver,
                node,
                bytes,
                &location.file,
                (decision, reasons),
                CallRoutingContext {
                    reachable,
                    handler_support,
                },
            );
        } else if node.kind() == "attribute" || node.kind() == "attribute_list" {
            let attr = node.utf8_text(bytes).unwrap_or("");
            let attr_lower = attr.to_ascii_lowercase();
            if attr_lower.contains("testpermissions") {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    "uses TestPermissions (requires BC authorization semantics)",
                    &location.file,
                    node,
                    reachable,
                );
            } else if attr_lower.contains("handler")
                && !attr_lower.contains("handlerfunctions")
                && !attr_lower.contains("messagehandler")
                && !attr_lower.contains("confirmhandler")
                && !attr_lower.contains("strmenuhandler")
                && !attr_lower.contains("hyperlinkhandler")
            {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    &format!("uses unsupported test handler attribute {attr}"),
                    &location.file,
                    node,
                    reachable,
                );
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

fn classify_type_reference(
    workspace: &Workspace,
    type_node: tree_sitter::Node<'_>,
    source: &[u8],
    file: &std::path::Path,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    reachable: bool,
) {
    let raw = type_node.utf8_text(source).unwrap_or("").trim();
    let (kind, subtype) = split_type_reference(raw);
    let kind_lower = kind.to_ascii_lowercase();

    if PLATFORM_TYPES
        .iter()
        .any(|platform| kind_lower.eq_ignore_ascii_case(platform))
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("uses platform type '{kind}'"),
            file,
            type_node,
            reachable,
        );
        return;
    }

    if kind_lower == "record" {
        let Some(table) = subtype.filter(|name| !name.is_empty()) else {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                "uses an untyped Record without a locally verifiable schema",
                file,
                type_node,
                reachable,
            );
            return;
        };
        let local_path = workspace.file_index.object_path_of_kind(&table, &["table"]);
        let (floor, message) = if let Some(path) = local_path {
            if let Some(capability) = table_platform_capability(workspace, &path) {
                (
                    RoutingDecision::LiveBc,
                    format!("record table '{table}' {capability}"),
                )
            } else {
                (
                    RoutingDecision::InterpRecord,
                    format!("uses workspace record table '{table}'"),
                )
            }
        } else {
            (
                RoutingDecision::LiveBc,
                format!("uses record table '{table}' without a workspace table definition"),
            )
        };
        promote(
            decision, reasons, floor, &message, file, type_node, reachable,
        );
    } else if kind_lower == "enum" {
        let local = subtype.as_deref().is_some_and(|name| {
            workspace
                .file_index
                .object_path_of_kind(name, &["enum"])
                .is_some()
        });
        if !local {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "uses enum '{}' without a workspace declaration for ordinal resolution",
                    subtype.unwrap_or_default()
                ),
                file,
                type_node,
                reachable,
            );
        }
    } else if matches!(kind_lower.as_str(), "page" | "report" | "xmlport" | "query") {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("uses platform object type '{kind}'"),
            file,
            type_node,
            reachable,
        );
    }
}

fn table_platform_capability(
    workspace: &Workspace,
    path: &std::path::Path,
) -> Option<&'static str> {
    let (text, tree) = workspace.file_index.get_cached_parse(path)?;
    let bytes = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "trigger_declaration" {
            return Some("declares triggers that require BC execution");
        }
        if matches!(node.kind(), "property" | "property_assignment") {
            let property = node.utf8_text(bytes).unwrap_or("").to_ascii_lowercase();
            if property.contains("fieldclass") && property.contains("flowfilter") {
                return Some("declares FlowFilter fields that require BC execution");
            }
            if property.contains("calcformula") && property.contains("linked(") {
                return Some("uses a Linked CalcFormula that requires BC execution");
            }
            if property.trim_start().starts_with("permissions") {
                return Some("declares permission behavior that requires BC execution");
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct CallRoutingContext {
    reachable: bool,
    handler_support: LocalHandlerSupport,
}

fn classify_call(
    workspace: &Workspace,
    resolver: &al_syntax::TypeResolver<'_>,
    node: tree_sitter::Node<'_>,
    source: &[u8],
    file: &std::path::Path,
    outcome: (&mut RoutingDecision, &mut Vec<RoutingReason>),
    context: CallRoutingContext,
) {
    let (decision, reasons) = outcome;
    let CallRoutingContext {
        reachable,
        handler_support,
    } = context;
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    let (Some(primary), Some(suffix)) = (children.first().copied(), children.last().copied())
    else {
        return;
    };
    let receiver = primary
        .utf8_text(source)
        .unwrap_or("")
        .trim()
        .trim_matches('"');

    // AL permits parameterless built-ins as statements without parentheses
    // (`Commit;`). In that shape the postfix expression has no call suffix.
    if children.len() == 1
        && PLATFORM_GLOBALS
            .iter()
            .any(|global| receiver.eq_ignore_ascii_case(global))
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("calls platform operation {receiver}"),
            file,
            primary,
            reachable,
        );
        return;
    }

    if suffix.kind() == "call_suffix" {
        if matches!(
            receiver.to_ascii_lowercase().as_str(),
            "message" | "confirm" | "strmenu" | "hyperlink"
        ) {
            if !handler_support.supports(receiver) {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    &format!("calls {receiver} without its required configured local test handler"),
                    file,
                    primary,
                    reachable,
                );
            }
            return;
        }
        if PLATFORM_GLOBALS
            .iter()
            .any(|global| receiver.eq_ignore_ascii_case(global))
        {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls platform operation {receiver}"),
                file,
                primary,
                reachable,
            );
            return;
        }
        // A bare global call is interpreter-safe only when the interpreter
        // actually implements it: a builtin from the shared catalog
        // (`supports_global_builtin` is the single source of truth), a
        // procedure of the same object (followed through the call graph), or
        // a receiver-less native stub. Everything else has no local
        // implementation and must route to LiveBc.
        let is_builtin = al_runtime::interpreter::dispatch::supports_global_builtin(receiver);
        let is_same_object_procedure =
            is_builtin || find_callable_node(resolver_root(node), source, receiver).is_some();
        let is_stub = is_same_object_procedure
            || al_runtime::stubs::CATALOGS
                .iter()
                .any(|catalog| (catalog.resolve)(receiver).is_some());
        if !is_builtin && !is_same_object_procedure && !is_stub {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls global '{receiver}' that the local interpreter does not implement"),
                file,
                primary,
                reachable,
            );
        }
        return;
    }

    if suffix.kind() == "scope_suffix" {
        let enum_type = if receiver.eq_ignore_ascii_case("enum") {
            children
                .get(1)
                .and_then(|scope| scope.child_by_field_name("member"))
                .and_then(|member| member.utf8_text(source).ok())
                .unwrap_or("")
                .trim_matches('"')
        } else {
            receiver
        };
        if workspace
            .file_index
            .object_path_of_kind(enum_type, &["enum"])
            .is_none()
        {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "uses enum '{enum_type}' without a workspace declaration for ordinal resolution"
                ),
                file,
                primary,
                reachable,
            );
        }
        return;
    }

    if !matches!(suffix.kind(), "member_call_suffix" | "scope_call_suffix") {
        return;
    }
    let Some(member_node) = suffix.child_by_field_name("member") else {
        return;
    };
    let method = member_node
        .utf8_text(source)
        .unwrap_or("")
        .trim()
        .trim_matches('"');

    if matches!(
        receiver.to_ascii_lowercase().as_str(),
        "codeunit" | "page" | "report" | "xmlport"
    ) {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("calls {receiver}.{method} (requires platform dispatch)"),
            file,
            member_node,
            reachable,
        );
        return;
    }

    let point = primary.start_position();
    let line = std::str::from_utf8(source)
        .ok()
        .and_then(|text| text.lines().nth(point.row))
        .unwrap_or("");
    let position = al_syntax::SyntaxPosition {
        line: point.row as u32,
        character: al_syntax::byte_col_to_utf16_col(line, point.column),
    };
    let Some(decl) = resolver.resolve_type(receiver, position) else {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "cannot resolve receiver '{receiver}' for call '{method}'; routing conservatively"
            ),
            file,
            member_node,
            reachable,
        );
        return;
    };
    let type_name = decl.type_name.to_ascii_lowercase();
    if type_name == "record" {
        let local = al_runtime::interpreter::records::supports_record_method(method);
        let (floor, message) = if local {
            (
                RoutingDecision::InterpRecord,
                format!("calls supported Record.{method}"),
            )
        } else {
            (
                RoutingDecision::LiveBc,
                format!("calls unsupported Record.{method} (requires BC semantics)"),
            )
        };
        promote(
            decision,
            reasons,
            floor,
            &message,
            file,
            member_node,
            reachable,
        );
    } else if type_name == "list" {
        if !al_runtime::interpreter::records::supports_list_method(method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported List.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name == "codeunit" {
        let subtype = decl.type_subtype.as_deref().unwrap_or("").trim();
        let has_local_body = (!subtype.is_empty())
            .then(|| {
                workspace
                    .file_index
                    .object_path_of_kind(subtype, &["codeunit"])
            })
            .flatten()
            .and_then(|path| workspace.file_index.get_cached_parse(&path))
            .is_some_and(|(text, tree)| {
                find_callable_node(tree.root_node(), text.as_bytes(), method).is_some()
            });
        let has_stub = !subtype.is_empty() && al_runtime::stubs::resolve(subtype, method).is_some();
        if !has_local_body && !has_stub {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "calls Codeunit '{}'.{method} without an executable workspace body or native stub",
                    if subtype.is_empty() {
                        "<unspecified>"
                    } else {
                        subtype
                    }
                ),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name.starts_with("text") || type_name.starts_with("code") {
        if !al_runtime::interpreter::records::supports_text_method(method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported Text.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name.starts_with("dictionary") {
        if !al_runtime::interpreter::records::supports_dict_method(method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported Dictionary.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if PLATFORM_TYPES
        .iter()
        .any(|platform| type_name.eq_ignore_ascii_case(platform))
        || matches!(type_name.as_str(), "page" | "report" | "xmlport" | "query")
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "calls {receiver}.{method} on platform type {}",
                decl.type_name
            ),
            file,
            member_node,
            reachable,
        );
    } else {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "calls {receiver}.{method} on type '{}' outside the verified local runtime capability set",
                decl.type_name
            ),
            file,
            member_node,
            reachable,
        );
    }
}

fn split_type_reference(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim().trim_end_matches(';').trim();
    let split = raw.find(char::is_whitespace).unwrap_or(raw.len());
    let kind = raw[..split].trim_matches('"').to_string();
    let mut subtype = raw[split..].trim().trim_matches('"').trim().to_string();
    if subtype.to_ascii_lowercase().ends_with(" temporary") {
        subtype.truncate(subtype.len() - " temporary".len());
        subtype = subtype.trim_end().trim_matches('"').to_string();
    }
    (kind, (!subtype.is_empty()).then_some(subtype))
}

fn promote(
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    floor: RoutingDecision,
    message: &str,
    file: &std::path::Path,
    node: tree_sitter::Node<'_>,
    reachable: bool,
) {
    *decision = (*decision).max(floor);
    let message = if reachable {
        format!("reachable procedure {message}")
    } else {
        message.to_string()
    };
    push_reason(
        reasons,
        RoutingReason {
            message,
            file: Some(file.to_string_lossy().into_owned()),
            line: Some(node.start_position().row as u32 + 1),
        },
    );
}

fn push_reason(reasons: &mut Vec<RoutingReason>, reason: RoutingReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

/// The root node of the tree containing `node` (walks up the parent chain).
fn resolver_root(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        current = parent;
    }
    current
}

fn find_callable_node<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_declaration"
        ) {
            if node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .is_some_and(|n| n.trim().trim_matches('"').eq_ignore_ascii_case(name))
            {
                return Some(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

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
                let subtype = text["record".len()..].trim().trim_matches('"').to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pure_logic_body() -> &'static str {
        "procedure TestSomething()
        begin
            Assert.AreEqual(1, 1);
        end;"
    }

    #[test]
    fn pure_logic_is_interp() {
        let (decision, reasons) = classify_body(pure_logic_body());
        assert_eq!(decision, RoutingDecision::Interp);
        assert!(reasons.is_empty(), "expected no disqualifying reasons");
    }

    #[test]
    fn record_insert_promotes_to_interp_record() {
        let body = "procedure T() begin Customer.Insert(true); end;";
        let (decision, reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
        assert!(reasons.iter().any(|r| r.message.contains("Insert")));
    }

    #[test]
    fn http_client_promotes_to_live_bc() {
        let body = "procedure T() var Client: HttpClient; begin end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
    }

    #[test]
    fn commit_forces_live_bc() {
        let body = "procedure T() begin Customer.Insert(true); Commit; end;";
        let (decision, reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
        assert!(reasons.len() >= 2);
    }

    #[test]
    fn codeunit_run_forces_live_bc_even_with_literal_id() {
        let body = "procedure T() begin Codeunit.Run(50100); end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
    }

    #[test]
    fn report_and_xmlport_force_live_bc() {
        for body in [
            "procedure T() begin Report.Run(50100); end;",
            "procedure T() begin XmlPort.Run(50100); end;",
            "procedure T() begin Page.Run(50100); end;",
        ] {
            let (decision, _reasons) = classify_body(body);
            assert_eq!(decision, RoutingDecision::LiveBc, "body: {body}");
        }
    }

    #[test]
    fn flowfield_calc_promotes_to_interp_record() {
        let body = "procedure T() begin Customer.CalcFields(Balance); end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
    }

    #[test]
    fn pure_arithmetic_stays_interp() {
        let body = "procedure T() var x: Integer; begin x := 1 + 2; end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::Interp);
    }

    #[test]
    fn case_insensitive_matching() {
        let body = "procedure T() begin CUSTOMER.INSERT(TRUE); end;";
        let (decision, _) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
    }

    #[test]
    fn rank_ordering_is_transitive() {
        assert_eq!(
            RoutingDecision::Interp.max(RoutingDecision::InterpRecord),
            RoutingDecision::InterpRecord
        );
        assert_eq!(
            RoutingDecision::InterpRecord.max(RoutingDecision::LiveBc),
            RoutingDecision::LiveBc
        );
        assert_eq!(
            RoutingDecision::Interp.max(RoutingDecision::LiveBc),
            RoutingDecision::LiveBc
        );
    }

    #[test]
    fn as_str_is_stable() {
        assert_eq!(RoutingDecision::Interp.as_str(), "interp");
        assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
        assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
    }

    #[test]
    fn both_interpreter_tiers_run_locally() {
        assert!(RoutingDecision::Interp.runs_locally());
        assert!(RoutingDecision::InterpRecord.runs_locally());
        assert!(!RoutingDecision::LiveBc.runs_locally());
    }

    #[test]
    fn interp_record_execution_note_says_local_record_runtime() {
        let note = RoutingDecision::InterpRecord.execution_note();
        assert!(
            note.contains("locally"),
            "InterpRecord note must mention local execution, got: {note:?}"
        );
        assert!(
            note.contains("record"),
            "InterpRecord note must identify the record runtime, got: {note:?}"
        );
        assert!(RoutingDecision::Interp.execution_note().contains("locally"));
        assert!(RoutingDecision::LiveBc.execution_note().contains("live BC"));
    }

    #[test]
    fn extracts_quoted_and_bare_record_subtypes() {
        let body = r#"
            procedure T()
            var
                Customer: Record Customer;
                Entry: Record "Native Entry";
            begin
            end;
        "#;
        assert_eq!(
            record_subtypes(body),
            vec!["Customer".to_string(), "Native Entry".to_string()]
        );
    }

    #[test]
    fn temporary_record_type_keeps_only_the_table_subtype() {
        assert_eq!(
            split_type_reference(r#"Record "Native Entry" temporary"#),
            ("Record".to_string(), Some("Native Entry".to_string()))
        );
    }

    #[test]
    fn record_word_in_error_text_is_not_a_table_declaration() {
        let body = r#"procedure T()
        begin
            Error('record not found');
        end;"#;
        assert!(record_subtypes(body).is_empty());
        assert_eq!(classify_body(body).0, RoutingDecision::Interp);
    }

    #[test]
    fn package_record_promotes_the_whole_shared_codeunit_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/NativeEntry.Table.al"),
            r#"table 50130 "Native Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}
"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/RecordTests.Codeunit.al"),
            r#"codeunit 50131 "Record Tests"
{
    Subtype = Test;
    [Test]
    procedure WorkspaceRecord()
    var
        Entry: Record "Native Entry";
    begin
        Entry.Insert();
    end;

    [Test]
    procedure PackageRecord()
    var
        Customer: Record Customer;
    begin
        Customer.Insert();
    end;
}
"#
            .to_string(),
        );

        let classified = classify_all(&workspace).unwrap();
        let local = classified
            .iter()
            .find(|result| result.method_name == "WorkspaceRecord")
            .expect("workspace record classification");
        assert_eq!(
            local.decision,
            RoutingDecision::LiveBc,
            "shared codeunit state must not split workspace-record and live methods"
        );
        assert!(
            local
                .reasons
                .iter()
                .any(|reason| reason.message.contains("shared globals")),
            "local-capable method must explain codeunit promotion: {:?}",
            local.reasons
        );

        let package = classified
            .iter()
            .find(|result| result.method_name == "PackageRecord")
            .expect("package record classification");
        assert_eq!(package.decision, RoutingDecision::LiveBc);
        assert!(
            package
                .reasons
                .iter()
                .any(|reason| reason.message.contains("without a workspace table")),
            "unexpected reasons: {:?}",
            package.reasons
        );
    }

    #[test]
    fn routing_follows_transitive_workspace_helpers() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/DeepEntry.Table.al"),
            r#"table 50150 "Deep Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/DeepHelper.Codeunit.al"),
            r#"codeunit 50151 "Deep Helper"
{
    procedure TouchRecord()
    var Entry: Record "Deep Entry";
    begin
        Entry.Insert();
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/MiddleHelper.Codeunit.al"),
            r#"codeunit 50152 "Middle Helper"
{
    procedure Run()
    var Helper: Codeunit "Deep Helper";
    begin
        Helper.TouchRecord();
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/GraphTests.Codeunit.al"),
            r#"codeunit 50153 "Graph Tests"
{
    Subtype = Test;
    [Test]
    procedure CallsTwoHelpers()
    var Helper: Codeunit "Middle Helper";
    begin
        Helper.Run();
    end;
}"#
            .to_string(),
        );

        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::InterpRecord);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.message.contains("reachable procedure")),
            "expected a transitive reason: {:?}",
            result.reasons
        );
        assert!(
            result
                .reasons
                .iter()
                .filter_map(|reason| reason.line)
                .all(|line| line >= 1),
            "routing reasons must use one-based source lines: {:?}",
            result.reasons
        );
    }

    #[test]
    fn list_count_is_not_misclassified_as_record_count() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/ListTests.Codeunit.al"),
            r#"codeunit 50154 "List Tests"
{
    Subtype = Test;
    [Test]
    procedure CountsAList()
    var Values: List of [Integer]; N: Integer;
    begin
        Values.Add(1);
        N := Values.Count();
    end;
}"#
            .to_string(),
        );
        assert_eq!(
            classify_all(&workspace).unwrap()[0].decision,
            RoutingDecision::Interp
        );
    }

    #[test]
    fn unsupported_list_method_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/ListRoutingTests.Codeunit.al"),
            r#"codeunit 50166 "List Routing Tests"
{
    Subtype = Test;
    [Test]
    procedure UsesUnsupportedListMethod()
    var Values: List of [Integer];
    begin
        Values.Reverse();
    end;
}"#
            .to_string(),
        );
        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.message.contains("unsupported List.Reverse")),
            "unexpected reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn unsupported_structured_type_method_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/JsonRoutingTests.Codeunit.al"),
            r#"codeunit 50167 "JSON Routing Tests"
{
    Subtype = Test;
    [Test]
    procedure ReadsJson()
    var Payload: JsonObject;
    begin
        Payload.ReadFrom('{}');
    end;
}"#
            .to_string(),
        );
        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result.reasons.iter().any(|reason| reason
                .message
                .contains("outside the verified local runtime")),
            "unexpected reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn unresolved_member_receiver_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/UnresolvedTests.Codeunit.al"),
            r#"codeunit 50161 "Unresolved Tests"
{
    Subtype = Test;
    [Test]
    procedure CallsUnknownReceiver()
    begin
        Mystery.DoSomething();
    end;
}"#
            .to_string(),
        );

        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.message.contains("cannot resolve receiver")),
            "unexpected routing reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn table_triggers_are_routed_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/TriggeredEntry.Table.al"),
            r#"table 50155 "Triggered Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
    trigger OnInsert() begin end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/TriggerTests.Codeunit.al"),
            r#"codeunit 50156 "Trigger Tests"
{
    Subtype = Test;
    [Test]
    procedure Inserts()
    var Entry: Record "Triggered Entry";
    begin
        Entry.Insert();
    end;
}"#
            .to_string(),
        );
        let results = classify_all(&workspace).unwrap();
        let result = &results[0];
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("triggers")));
    }

    #[test]
    fn shared_codeunit_state_promotes_every_method_to_one_backend() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/SharedEntry.Table.al"),
            r#"table 50157 "Shared Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/SharedTests.Codeunit.al"),
            r#"codeunit 50158 "Shared Tests"
{
    Subtype = Test;
    var SharedFlag: Boolean;

    [Test]
    procedure PureMethod()
    begin
        SharedFlag := true;
    end;

    [Test]
    procedure RecordMethod()
    var Entry: Record "Shared Entry";
    begin
        Entry.Insert();
    end;
}"#
            .to_string(),
        );

        let results = classify_all(&workspace).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.decision == RoutingDecision::InterpRecord));
        let pure = results
            .iter()
            .find(|result| result.method_name == "PureMethod")
            .expect("pure method");
        assert!(
            pure.reasons
                .iter()
                .any(|reason| reason.message.contains("shared globals")),
            "promotion must explain the shared-state reason: {:?}",
            pure.reasons
        );
    }

    #[test]
    fn platform_handler_promotes_other_methods_through_shared_codeunit_state() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/PlatformHandlerTests.Codeunit.al"),
            r#"codeunit 50159 "Platform Handler Tests"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('HandleNotification')]
    procedure PlatformMethod()
    begin
    end;

    [Test]
    procedure OtherwisePure()
    begin
    end;

    [SendNotificationHandler]
    procedure HandleNotification(var Notification: Notification): Boolean
    begin
        exit(true);
    end;
}"#
            .to_string(),
        );

        let results = classify_all(&workspace).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.decision == RoutingDecision::LiveBc));
        let pure = results
            .iter()
            .find(|result| result.method_name == "OtherwisePure")
            .expect("pure method");
        assert!(
            pure.reasons
                .iter()
                .any(|reason| reason.message.contains("shared globals")),
            "platform handler must promote the other method: {:?}",
            pure.reasons
        );
    }

    #[test]
    fn deterministic_handler_attributes_remain_local() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/LocalHandlerRouting.Codeunit.al"),
            r#"codeunit 50160 "Local Handler Routing"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('HandleMenu,HandleLink')]
    procedure Dialogs()
    begin
        StrMenu('First,Second', 1, 'Pick');
        Hyperlink('https://example.test');
    end;

    [StrMenuHandler]
    procedure HandleMenu(MenuOptions: Text[1024]; var Choice: Integer; Instruction: Text[1024])
    begin
        Choice := 2;
    end;

    [HyperlinkHandler]
    procedure HandleLink(Link: Text[1024])
    begin
    end;
}"#
            .to_string(),
        );
        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(
            result.decision,
            RoutingDecision::Interp,
            "deterministic handler attributes must not create a false live fallback: {:?}",
            result.reasons
        );
    }

    #[test]
    fn dialog_without_matching_handler_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/UnhandledRouting.Codeunit.al"),
            r#"codeunit 50162 "Unhandled Routing"
{
    Subtype = Test;

    [Test]
    procedure OpensMessage()
    begin
        Message('must not disappear');
    end;
}"#
            .to_string(),
        );

        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result.reasons.iter().any(|reason| reason
                .message
                .contains("required configured local test handler")),
            "unexpected reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn handler_function_without_supported_attribute_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/InvalidHandlerRouting.Codeunit.al"),
            r#"codeunit 50163 "Invalid Handler Routing"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('NotAHandler')]
    procedure OpensMessage()
    begin
        Message('must not disappear');
    end;

    procedure NotAHandler(MessageText: Text[1024])
    begin
    end;
}"#
            .to_string(),
        );

        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result.reasons.iter().any(|reason| reason
                .message
                .contains("exactly one supported local handler")),
            "unexpected reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn unimplemented_bare_global_routes_to_live_bc() {
        // `Evaluate` (and any other global the interpreter does not
        // implement) has no local body: routing it to Interp would fail at
        // runtime with "procedure not found" instead of falling back to BC.
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/GlobalRouting.Codeunit.al"),
            r#"codeunit 50170 "Global Routing"
{
    Subtype = Test;

    [Test]
    procedure UsesEvaluate()
    var
        t: Integer;
    begin
        Evaluate(t, '42');
    end;
}"#
            .to_string(),
        );
        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result.reasons.iter().any(|reason| reason
                .message
                .contains("global 'Evaluate' that the local interpreter does not implement")),
            "unexpected reasons: {:?}",
            result.reasons
        );
    }

    #[test]
    fn implemented_builtin_and_same_object_bare_calls_stay_interp() {
        // Bare calls to interpreter builtins (shared safe-list) and to the
        // codeunit's own procedures must not be pushed to LiveBc.
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/BuiltinRouting.Codeunit.al"),
            r#"codeunit 50171 "Builtin Routing"
{
    Subtype = Test;

    [Test]
    procedure UsesBuiltins()
    var
        n: Integer;
        s: Text;
    begin
        n := Abs(-5);
        n := StrPos('abc', 'b');
        s := IncStr('INV-001');
        Helper();
    end;

    procedure Helper()
    begin
    end;
}"#
            .to_string(),
        );
        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(
            result.decision,
            RoutingDecision::Interp,
            "builtin and same-object calls must stay local: {:?}",
            result.reasons
        );
    }

    #[test]
    fn supported_text_methods_stay_local_and_unsupported_route_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/TextRouting.Codeunit.al"),
            r#"codeunit 50172 "Text Routing"
{
    Subtype = Test;

    [Test]
    procedure SupportedTextMethod()
    var
        s: Text;
        found: Boolean;
    begin
        s := 'abc';
        found := s.Contains('b');
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/TextRouting2.Codeunit.al"),
            r#"codeunit 50173 "Text Routing 2"
{
    Subtype = Test;

    [Test]
    procedure UnsupportedTextMethod()
    var
        s: Text;
    begin
        s := 'abc';
        s := s.PadLeft(10);
    end;
}"#
            .to_string(),
        );
        let results = classify_all(&workspace).unwrap();
        let supported = results
            .iter()
            .find(|result| result.method_name == "SupportedTextMethod")
            .expect("supported classification");
        assert_eq!(
            supported.decision,
            RoutingDecision::Interp,
            "supported Text methods run locally: {:?}",
            supported.reasons
        );
        let unsupported = results
            .iter()
            .find(|result| result.method_name == "UnsupportedTextMethod")
            .expect("unsupported classification");
        assert_eq!(unsupported.decision, RoutingDecision::LiveBc);
        assert!(
            unsupported
                .reasons
                .iter()
                .any(|reason| reason.message.contains("unsupported Text.PadLeft")),
            "unexpected reasons: {:?}",
            unsupported.reasons
        );
    }

    #[test]
    fn dictionary_get_routes_to_live_bc_but_supported_methods_stay_local() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/DictRouting.Codeunit.al"),
            r#"codeunit 50174 "Dict Routing"
{
    Subtype = Test;

    [Test]
    procedure SupportedDictMethods()
    var
        d: Dictionary of [Text, Integer];
        n: Integer;
    begin
        d.Add('a', 1);
        d.Set('a', 2);
        n := d.Count();
    end;

    [Test]
    procedure UsesDictGet()
    var
        d: Dictionary of [Text, Integer];
        n: Integer;
    begin
        d.Add('a', 1);
        d.Get('a', n);
    end;
}"#
            .to_string(),
        );
        let results = classify_all(&workspace).unwrap();
        // Codeunit integrity keeps every method on one backend; the Get user
        // must drag the codeunit to LiveBc with an explicit reason.
        let get_user = results
            .iter()
            .find(|result| result.method_name == "UsesDictGet")
            .expect("dict get classification");
        assert_eq!(get_user.decision, RoutingDecision::LiveBc);
        assert!(
            get_user
                .reasons
                .iter()
                .any(|reason| reason.message.contains("unsupported Dictionary.Get")),
            "unexpected reasons: {:?}",
            get_user.reasons
        );
    }

    #[test]
    fn stateful_helper_codeunit_routes_to_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/StatefulHelper.Codeunit.al"),
            r#"codeunit 50164 "Stateful Helper"
{
    var Counter: Integer;

    procedure Next(): Integer
    begin
        Counter := Counter + 1;
        exit(Counter);
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/StatefulHelperTests.Codeunit.al"),
            r#"codeunit 50165 "Stateful Helper Tests"
{
    Subtype = Test;

    [Test]
    procedure UsesStatefulHelper()
    var Helper: Codeunit "Stateful Helper";
    begin
        Helper.Next();
    end;
}"#
            .to_string(),
        );

        let result = classify_all(&workspace).unwrap().remove(0);
        assert_eq!(result.decision, RoutingDecision::LiveBc);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.message.contains("object-level state")),
            "unexpected reasons: {:?}",
            result.reasons
        );
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
