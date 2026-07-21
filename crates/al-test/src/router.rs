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
    /// Replays a previously-captured snapshot if one exists, else falls
    /// back to `LiveBc`.
    Snapshot,
}

impl RoutingDecision {
    /// Stable string form for wire serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            RoutingDecision::Interp => "interp",
            RoutingDecision::InterpRecord => "interpRecord",
            RoutingDecision::LiveBc => "liveBc",
            RoutingDecision::Snapshot => "snapshot",
        }
    }

    /// Whether a test with this decision **actually executes locally** today
    /// (pure Rust interpreter, no Business Central server contact).
    ///
    /// Both interpreter tiers run locally; `LiveBc` and `Snapshot` require an
    /// external or previously captured runtime.
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
            RoutingDecision::Snapshot => "replays a captured snapshot, else routes to live BC",
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
    name: String,
}

type ProcedureCatalog = HashMap<(String, String), ProcedureLocation>;

const LOCAL_RECORD_METHODS: &[&str] = &[
    "init",
    "get",
    "insert",
    "modify",
    "delete",
    "find",
    "findset",
    "findfirst",
    "findlast",
    "next",
    "setrange",
    "setfilter",
    "count",
    "countapprox",
    "isempty",
    "reset",
    "setcurrentkey",
    "deleteall",
    "calcfields",
];

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
pub fn classify_all(workspace: &Workspace) -> Vec<ClassifyResult> {
    let codeunits = al_analysis::queries::tests::discover_tests(workspace);
    classify_codeunits(workspace, &codeunits)
}

/// Classify a known set of test codeunits (so callers that already
/// computed `discover_tests` don't pay for it twice).
pub fn classify_codeunits(
    workspace: &Workspace,
    codeunits: &[TestCodeunit],
) -> Vec<ClassifyResult> {
    let (insight, cached_graph) = workspace.get_or_build_call_graph();
    drop(cached_graph);
    let mut graph = CallGraph::build_from_insight(&insight);
    al_insight::calls::resolve_all_workspace_call_edges(
        &workspace.file_index,
        &workspace.symbols,
        &insight,
        &mut graph,
    );
    let catalog = build_procedure_catalog(workspace);
    let mut out = Vec::new();
    for cu in codeunits {
        let kind = workspace
            .file_index
            .object_info
            .get(std::path::Path::new(&cu.file))
            .and_then(|info| info.kind.parse::<ObjectKind>().ok())
            .unwrap_or(ObjectKind::Codeunit);
        for proc in &cu.tests {
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
            let (mut decision, mut reasons) = classify_reachable(workspace, &graph, &catalog, root);
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
                let (lifecycle_decision, lifecycle_reasons) =
                    classify_reachable(workspace, &graph, &catalog, lifecycle_root);
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
                    classify_reachable(workspace, &graph, &catalog, handler_root);
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
    }
    out
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
                            name: clean,
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
) -> (RoutingDecision, Vec<RoutingReason>) {
    let mut decision = RoutingDecision::Interp;
    let mut reasons = Vec::new();
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(node) = queue.pop_front() {
        let Some(info) = graph.node_info(node) else {
            continue;
        };
        let key = (
            info.object.to_ascii_lowercase(),
            info.name.to_ascii_lowercase(),
        );
        if let Some(location) = catalog.get(&key) {
            classify_procedure_ast(
                workspace,
                location,
                &mut decision,
                &mut reasons,
                node != root,
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

fn classify_procedure_ast(
    workspace: &Workspace,
    location: &ProcedureLocation,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    reachable: bool,
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
                reachable,
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

fn classify_call(
    workspace: &Workspace,
    resolver: &al_syntax::TypeResolver<'_>,
    node: tree_sitter::Node<'_>,
    source: &[u8],
    file: &std::path::Path,
    outcome: (&mut RoutingDecision, &mut Vec<RoutingReason>),
    reachable: bool,
) {
    let (decision, reasons) = outcome;
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
        return;
    };
    let type_name = decl.type_name.to_ascii_lowercase();
    if type_name == "record" {
        let local = LOCAL_RECORD_METHODS
            .iter()
            .any(|candidate| method.eq_ignore_ascii_case(candidate));
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
            line: Some(node.start_position().row as u32),
        },
    );
}

fn push_reason(reasons: &mut Vec<RoutingReason>, reason: RoutingReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
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

#[cfg(any())]
mod superseded_router_helpers {
    use super::*;

    fn classify_type_reference(
        workspace: &Workspace,
        node: tree_sitter::Node<'_>,
        source: &[u8],
        file: &std::path::Path,
        decision: &mut RoutingDecision,
        reasons: &mut Vec<RoutingReason>,
        reachable: bool,
    ) {
        let text = node.utf8_text(source).unwrap_or("").trim();
        let mut parts = text.splitn(2, char::is_whitespace);
        let type_name = parts.next().unwrap_or("").trim_matches('"');
        let subtype = parts
            .next()
            .unwrap_or("")
            .trim()
            .trim_end_matches(" temporary")
            .trim_matches('"');
        if type_name.eq_ignore_ascii_case("record") {
            let workspace_table = !subtype.is_empty()
                && workspace
                    .file_index
                    .find_by_object_name(subtype)
                    .and_then(|path| {
                        workspace
                            .file_index
                            .object_info
                            .get(&path)
                            .map(|info| info.kind.clone())
                    })
                    .is_some_and(|kind| {
                        matches!(
                            kind.to_ascii_lowercase().as_str(),
                            "table" | "tableextension"
                        )
                    });
            let (floor, message) = if workspace_table {
                (
                    RoutingDecision::InterpRecord,
                    format!("uses workspace record table '{subtype}'"),
                )
            } else {
                (
                    RoutingDecision::LiveBc,
                    format!("uses record table '{subtype}' without a workspace table definition"),
                )
            };
            promote(decision, reasons, floor, &message, file, node, reachable);
        } else if PLATFORM_TYPES
            .iter()
            .any(|candidate| type_name.eq_ignore_ascii_case(candidate))
        {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("uses platform type {type_name}"),
                file,
                node,
                reachable,
            );
        }
    }

    fn classify_call(
        resolver: &al_syntax::TypeResolver<'_>,
        node: tree_sitter::Node<'_>,
        source: &[u8],
        file: &std::path::Path,
        decision: &mut RoutingDecision,
        reasons: &mut Vec<RoutingReason>,
        reachable: bool,
    ) {
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        let Some(last) = children.last().copied() else {
            return;
        };
        match last.kind() {
            "call_suffix" => {
                let name = children
                    .first()
                    .and_then(|child| child.utf8_text(source).ok())
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                if PLATFORM_GLOBALS
                    .iter()
                    .any(|candidate| name.eq_ignore_ascii_case(candidate))
                {
                    promote(
                        decision,
                        reasons,
                        RoutingDecision::LiveBc,
                        &format!("calls platform operation {name}"),
                        file,
                        node,
                        reachable,
                    );
                }
            }
            "member_call_suffix" | "scope_call_suffix" => {
                let receiver = children
                    .first()
                    .and_then(|child| child.utf8_text(source).ok())
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                let method = last
                    .child_by_field_name("member")
                    .and_then(|member| member.utf8_text(source).ok())
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                let point = node.start_position();
                let position = al_syntax::SyntaxPosition {
                    line: point.row as u32,
                    character: point.column as u32,
                };
                if let Some(decl) = resolver.resolve_type(receiver, position) {
                    if decl.type_name.eq_ignore_ascii_case("record") {
                        let local = LOCAL_RECORD_METHODS
                            .iter()
                            .any(|candidate| method.eq_ignore_ascii_case(candidate));
                        let (floor, message) = if local {
                            (
                                RoutingDecision::InterpRecord,
                                format!("calls supported Record.{method}"),
                            )
                        } else {
                            (
                                RoutingDecision::LiveBc,
                                format!("calls Record.{method}, which requires BC semantics"),
                            )
                        };
                        promote(decision, reasons, floor, &message, file, node, reachable);
                    } else if PLATFORM_TYPES
                        .iter()
                        .any(|candidate| decl.type_name.eq_ignore_ascii_case(candidate))
                    {
                        promote(
                            decision,
                            reasons,
                            RoutingDecision::LiveBc,
                            &format!("calls {}.{method}", decl.type_name),
                            file,
                            node,
                            reachable,
                        );
                    }
                } else if ["report", "xmlport", "page", "codeunit"]
                    .iter()
                    .any(|candidate| receiver.eq_ignore_ascii_case(candidate))
                    && matches!(method.to_ascii_lowercase().as_str(), "run" | "runmodal")
                {
                    promote(
                        decision,
                        reasons,
                        RoutingDecision::LiveBc,
                        &format!("calls {receiver}.{method}"),
                        file,
                        node,
                        reachable,
                    );
                }
            }
            _ => {}
        }
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
        push_reason(
            reasons,
            RoutingReason {
                message: if reachable {
                    format!("reachable call path: {message}")
                } else {
                    message.to_string()
                },
                file: Some(file.to_string_lossy().into_owned()),
                line: Some(node.start_position().row as u32 + 1),
            },
        );
    }

    fn push_reason(reasons: &mut Vec<RoutingReason>, reason: RoutingReason) {
        if !reasons.iter().any(|existing| existing == &reason) {
            reasons.push(reason);
        }
    }
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
                    if LOCAL_RECORD_METHODS
                        .iter()
                        .any(|candidate| method.eq_ignore_ascii_case(candidate))
                    {
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
                RoutingDecision::Snapshot => 2,
                RoutingDecision::LiveBc => 3,
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
    fn unknown_pattern_stays_interp() {
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
        assert_eq!(
            RoutingDecision::Snapshot.max(RoutingDecision::Interp),
            RoutingDecision::Snapshot
        );
    }

    #[test]
    fn as_str_is_stable() {
        assert_eq!(RoutingDecision::Interp.as_str(), "interp");
        assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
        assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
        assert_eq!(RoutingDecision::Snapshot.as_str(), "snapshot");
    }

    #[test]
    fn both_interpreter_tiers_run_locally() {
        assert!(RoutingDecision::Interp.runs_locally());
        assert!(RoutingDecision::InterpRecord.runs_locally());
        assert!(!RoutingDecision::LiveBc.runs_locally());
        assert!(!RoutingDecision::Snapshot.runs_locally());
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
    fn workspace_record_table_is_local_but_package_table_requires_live_bc() {
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

        let classified = classify_all(&workspace);
        let local = classified
            .iter()
            .find(|result| result.method_name == "WorkspaceRecord")
            .expect("workspace record classification");
        assert_eq!(local.decision, RoutingDecision::InterpRecord);

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
        let result = affected_tests_detailed(&ws, &changed);

        assert_eq!(result.mode, AffectedMode::CallGraph);
        assert_eq!(result.tests.len(), 1, "got {:?}", result.tests);
        assert_eq!(result.tests[0].method_name, "TestCallsHelper");
    }
}
