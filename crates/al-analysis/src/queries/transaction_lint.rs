//! Symbol-aware transaction lint for AL workspaces.
//!
//! These rules deliberately live in `al-analysis`, not `al-syntax`: deciding
//! whether a write or `Commit()` is dangerous requires the resolved procedure,
//! event, trigger, interface, and dependency graph. A file-local token scan
//! cannot see the transaction stack that gives either operation its meaning.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use al_insight::graph::{InsightGraph, NodeKey};
use al_insight::index::{CallGraph, EdgeKind, NodeId};
use al_symbols::ObjectKind;
use al_workspace::Workspace;

use super::Range;

/// A `Commit()` reachable after an earlier database mutation in the same
/// transaction stack. The commit prevents a caller from rolling that mutation
/// back.
pub const COMMIT_AFTER_DATABASE_CHANGE: &str = "AL-NL003";
/// A database mutation made directly or transitively from a `[TryFunction]`.
/// AL does not roll these writes back when the try function returns `false`.
pub const DATABASE_WRITE_IN_TRY_STACK: &str = "AL-NL004";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLintSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLintDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub file: PathBuf,
    /// LSP-ready range (0-based lines, UTF-16 columns).
    pub range: Range,
    pub severity: WorkspaceLintSeverity,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceLintRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: WorkspaceLintSeverity,
    pub description: &'static str,
}

const RULES: &[WorkspaceLintRuleInfo] = &[
    WorkspaceLintRuleInfo {
        code: COMMIT_AFTER_DATABASE_CHANGE,
        name: "commit-after-database-change",
        severity: WorkspaceLintSeverity::Warning,
        description: "Commit() is reachable after a database change in the same resolved call/event stack and can prevent caller rollback.",
    },
    WorkspaceLintRuleInfo {
        code: DATABASE_WRITE_IN_TRY_STACK,
        name: "database-write-in-try-stack",
        severity: WorkspaceLintSeverity::Warning,
        description: "A database write is directly or transitively reachable from a [TryFunction] and is not rolled back when the try call fails.",
    },
];

#[must_use]
pub fn transaction_lint_rules() -> &'static [WorkspaceLintRuleInfo] {
    RULES
}

#[derive(Debug, Clone)]
struct EffectSite {
    range: Range,
    byte_start: usize,
    label: String,
}

#[derive(Debug, Clone)]
struct ProcedureEffects {
    node: NodeId,
    file: PathBuf,
    declaration_range: Range,
    reportable: bool,
    is_try_function: bool,
    writes: Vec<EffectSite>,
    commits: Vec<EffectSite>,
    /// Dependency/workspace event named by `[EventSubscriber]`, when present.
    subscriber_target: Option<(String, String)>,
}

/// Run transaction lint across the fully-resolved workspace call/event graph.
///
/// Resolution includes workspace calls, interface dispatch, codeunit-run
/// dispatch, record triggers, event subscribers, and object/method declarations
/// loaded from standard and third-party `.app` packages. When a package embeds
/// AL source, its complete procedure bodies participate in call/effect
/// resolution. Symbol-only packages retain declaration/event fallback behavior.
pub fn transaction_lints(
    workspace: &Workspace,
) -> Result<Vec<WorkspaceLintDiagnostic>, al_workspace::CallGraphBuildError> {
    // Ensure the cached graph is workspace-enriched (package nodes alone are
    // insufficient), then build a private fully-resolved edge set. The shared
    // interactive graph intentionally resolves only high-fanout files eagerly.
    let (insight, cached_call_graph) = workspace.get_or_build_call_graph()?;
    drop(cached_call_graph);
    let mut call_graph = CallGraph::build_from_insight(&insight);
    al_insight::calls::resolve_all_workspace_call_edges(
        &workspace.file_index,
        &workspace.symbols,
        &insight,
        &mut call_graph,
    )?;
    let dependency_sources = workspace.get_or_build_dependency_source_index()?;
    al_insight::calls::resolve_all_workspace_call_edges(
        &dependency_sources,
        &workspace.symbols,
        &insight,
        &mut call_graph,
    )?;

    let mut effects = collect_effects(&workspace.file_index, &insight, true)?;
    effects.extend(collect_effects(&dependency_sources, &insight, false)?);
    if effects.is_empty() {
        return Ok(Vec::new());
    }
    let by_node: HashMap<NodeId, &ProcedureEffects> =
        effects.iter().map(|effect| (effect.node, effect)).collect();

    let mut diagnostics = Vec::new();
    let mut seen: HashSet<(&'static str, PathBuf, u32, u32)> = HashSet::new();

    lint_commits(&effects, &by_node, &call_graph, &mut diagnostics, &mut seen);
    lint_try_stacks(&effects, &by_node, &call_graph, &mut diagnostics, &mut seen);

    diagnostics.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
            .then(a.code.cmp(b.code))
    });
    Ok(diagnostics)
}

fn lint_commits(
    effects: &[ProcedureEffects],
    by_node: &HashMap<NodeId, &ProcedureEffects>,
    graph: &CallGraph,
    out: &mut Vec<WorkspaceLintDiagnostic>,
    seen: &mut HashSet<(&'static str, PathBuf, u32, u32)>,
) {
    for effect in effects.iter().filter(|effect| effect.reportable) {
        for commit in &effect.commits {
            // Same-procedure ordering is exact: only a write textually before
            // this Commit() is evidence of an earlier mutation.
            let local_write = effect
                .writes
                .iter()
                .filter(|write| write.byte_start < commit.byte_start)
                .min_by_key(|write| write.byte_start);

            let upstream_path = find_upstream_write_path(effect.node, by_node, graph);
            let dependency_event = effect
                .subscriber_target
                .as_ref()
                .filter(|(_, event)| is_post_database_event(event));

            let reason = if let Some(write) = local_write {
                format!("{} occurs earlier in this procedure", write.label)
            } else if let Some(path) = upstream_path {
                format!(
                    "a database-writing caller reaches this commit on transaction path {}",
                    format_path(graph, &path)
                )
            } else if let Some((object, event)) = dependency_event {
                format!(
                    "this subscriber runs after database-changing event {object}::{event} resolved from workspace/dependency symbols"
                )
            } else {
                continue;
            };

            push_once(
                out,
                seen,
                WorkspaceLintDiagnostic {
                    code: COMMIT_AFTER_DATABASE_CHANGE,
                    message: format!(
                        "{}; Commit() can finalize the transaction and prevent callers from rolling a database change back.",
                        capitalize(&reason)
                    ),
                    file: effect.file.clone(),
                    range: commit.range,
                    severity: WorkspaceLintSeverity::Warning,
                },
            );
        }
    }
}

fn lint_try_stacks(
    effects: &[ProcedureEffects],
    by_node: &HashMap<NodeId, &ProcedureEffects>,
    graph: &CallGraph,
    out: &mut Vec<WorkspaceLintDiagnostic>,
    seen: &mut HashSet<(&'static str, PathBuf, u32, u32)>,
) {
    for root in effects.iter().filter(|effect| effect.is_try_function) {
        let reachable = reachable_callees_with_paths(root.node, graph);
        for (node, path) in reachable {
            let Some(effect) = by_node.get(&node).copied() else {
                // A genuinely symbol-only dependency has no body from which a
                // write can be proven. Source-backed dependencies are present
                // in `by_node` and participate normally.
                continue;
            };
            for write in &effect.writes {
                let (file, range, message) = if effect.reportable {
                    (
                        effect.file.clone(),
                        write.range,
                        format!(
                            "{} is reachable from [TryFunction] through {}; database changes in a try-function call stack are not rolled back when the try call fails.",
                            write.label,
                            format_path(graph, &path)
                        ),
                    )
                } else if root.reportable {
                    // The dependency body is known, but editor diagnostics
                    // must point into the user's project. Anchor the warning on
                    // the project TryFunction declaration and name the proven
                    // external write/path in the message.
                    (
                        root.file.clone(),
                        root.declaration_range,
                        format!(
                            "{} in dependency source is reachable from this [TryFunction] through {}; database changes in a try-function call stack are not rolled back when the try call fails.",
                            write.label,
                            format_path(graph, &path)
                        ),
                    )
                } else {
                    continue;
                };
                push_once(
                    out,
                    seen,
                    WorkspaceLintDiagnostic {
                        code: DATABASE_WRITE_IN_TRY_STACK,
                        message,
                        file,
                        range,
                        severity: WorkspaceLintSeverity::Warning,
                    },
                );
            }
        }
    }
}

fn push_once(
    out: &mut Vec<WorkspaceLintDiagnostic>,
    seen: &mut HashSet<(&'static str, PathBuf, u32, u32)>,
    diagnostic: WorkspaceLintDiagnostic,
) {
    let key = (
        diagnostic.code,
        diagnostic.file.clone(),
        diagnostic.range.start.line,
        diagnostic.range.start.character,
    );
    if seen.insert(key) {
        out.push(diagnostic);
    }
}

/// Search callers of `start` until a workspace procedure with a direct write
/// is found. `EventSubscription` is metadata (`subscriber -> event`), not a
/// runtime call direction, so it is excluded from transaction traversal.
fn find_upstream_write_path(
    start: NodeId,
    effects: &HashMap<NodeId, &ProcedureEffects>,
    graph: &CallGraph,
) -> Option<Vec<NodeId>> {
    let mut queue = VecDeque::from([(start, vec![start])]);
    let mut visited = HashSet::from([start]);
    while let Some((node, reverse_path)) = queue.pop_front() {
        for edge in graph
            .callers_of(node)
            .iter()
            .filter(|edge| edge.kind != EdgeKind::EventSubscription)
        {
            if !visited.insert(edge.from) {
                continue;
            }
            let mut next = reverse_path.clone();
            next.push(edge.from);
            if effects
                .get(&edge.from)
                .is_some_and(|effect| !effect.writes.is_empty())
            {
                next.reverse();
                return Some(next);
            }
            queue.push_back((edge.from, next));
        }
    }
    None
}

fn reachable_callees_with_paths(start: NodeId, graph: &CallGraph) -> Vec<(NodeId, Vec<NodeId>)> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([(start, vec![start])]);
    let mut visited = HashSet::from([start]);
    while let Some((node, path)) = queue.pop_front() {
        found.push((node, path.clone()));
        for edge in graph
            .callees_of(node)
            .iter()
            .filter(|edge| edge.kind != EdgeKind::EventSubscription)
        {
            if visited.insert(edge.to) {
                let mut next = path.clone();
                next.push(edge.to);
                queue.push_back((edge.to, next));
            }
        }
    }
    found
}

fn format_path(graph: &CallGraph, path: &[NodeId]) -> String {
    path.iter()
        .map(|id| {
            graph
                .node_info(*id)
                .map(|info| format!("{}::{}", info.object, info.name))
                .unwrap_or_else(|| format!("node#{}", id.0))
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn collect_effects(
    file_index: &al_source::file_index::FileIndex,
    insight: &InsightGraph,
    reportable: bool,
) -> Result<Vec<ProcedureEffects>, al_insight::calls::SourceGraphError> {
    let snapshot: Vec<(PathBuf, al_source::file_index::CachedObjectInfo)> = file_index
        .object_info
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().clone()))
        .collect();
    let mut effects = Vec::new();

    for (path, object) in snapshot {
        let object_kind = object.kind.parse::<ObjectKind>().map_err(|_| {
            al_insight::calls::SourceGraphError::InvalidObjectKind {
                path: path.clone(),
                kind: object.kind.clone(),
            }
        })?;
        object_kind
            .normalize_declaration_id(object.id)
            .map_err(|error| match error {
                al_symbols::DeclarationIdError::Missing { .. } => {
                    al_insight::calls::SourceGraphError::MissingObjectId { path: path.clone() }
                }
                al_symbols::DeclarationIdError::OutOfRange { id, .. } => {
                    al_insight::calls::SourceGraphError::ObjectIdOutOfRange {
                        path: path.clone(),
                        id,
                    }
                }
                al_symbols::DeclarationIdError::Unexpected { id, .. } => {
                    al_insight::calls::SourceGraphError::UnexpectedObjectId {
                        path: path.clone(),
                        id,
                    }
                }
            })?;
        let (source, tree) = file_index.get_cached_parse(&path).ok_or_else(|| {
            al_insight::calls::SourceGraphError::MissingCachedParse { path: path.clone() }
        })?;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
                if let Some(effect) = effects_for_procedure(
                    &path,
                    &object.name,
                    object_kind,
                    node,
                    &tree,
                    &source,
                    insight,
                    reportable,
                ) {
                    effects.push(effect);
                }
                continue;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    Ok(effects)
}

#[allow(clippy::too_many_arguments)]
fn effects_for_procedure(
    path: &std::path::Path,
    object_name: &str,
    object_kind: ObjectKind,
    procedure: tree_sitter::Node<'_>,
    tree: &tree_sitter::Tree,
    source: &str,
    insight: &InsightGraph,
    reportable: bool,
) -> Option<ProcedureEffects> {
    let bytes = source.as_bytes();
    let name_node = procedure.child_by_field_name("name")?;
    let name = name_node
        .utf8_text(bytes)
        .ok()?
        .trim()
        .trim_matches('"')
        .to_string();
    let attributes = procedure_attributes(procedure, bytes);
    let node_key = callable_key(object_kind, object_name, &name, &attributes);
    let node = CallGraph::node_id_for(insight, &node_key)?;
    let is_try_function = attributes
        .iter()
        .any(|(attr, _)| attr.eq_ignore_ascii_case("TryFunction"));
    let subscriber_target = subscriber_target(&attributes)
        .filter(|(object, event)| subscriber_event_is_resolved(insight, object, event));
    let (writes, commits) = collect_effect_sites(procedure, tree, source);

    Some(ProcedureEffects {
        node,
        file: path.to_path_buf(),
        declaration_range: al_syntax::ts_range_to_syntax(&name_node.range(), bytes).into(),
        reportable,
        is_try_function,
        writes,
        commits,
        subscriber_target,
    })
}

fn callable_key(
    object_kind: ObjectKind,
    object_name: &str,
    procedure_name: &str,
    attributes: &[(String, String)],
) -> NodeKey {
    let object = object_name.to_lowercase();
    let procedure = procedure_name.to_lowercase();
    if attributes
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(al_insight::attr_names::EVENT_SUBSCRIBER))
    {
        NodeKey::Subscriber(object_kind, object, procedure)
    } else if attributes.iter().any(|(name, _)| {
        name.eq_ignore_ascii_case(al_insight::attr_names::INTEGRATION_EVENT)
            || name.eq_ignore_ascii_case(al_insight::attr_names::BUSINESS_EVENT)
    }) {
        NodeKey::Event(object_kind, object, procedure)
    } else {
        NodeKey::Procedure(object_kind, object, procedure)
    }
}

/// Attributes normally belong to the procedure node in the current grammar.
/// Keep the preceding-sibling fallback for older grammar trees and malformed
/// but recoverable source, where decorators can be emitted as member siblings.
fn procedure_attributes(procedure: tree_sitter::Node<'_>, source: &[u8]) -> Vec<(String, String)> {
    let mut attrs = al_insight::calls::collect_procedure_attributes(procedure, source);
    if attrs.is_empty() {
        let mut sibling = procedure.prev_named_sibling();
        while let Some(node) = sibling {
            if node.kind() != "attribute" {
                break;
            }
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            let raw = node.utf8_text(source).unwrap_or("").to_string();
            if !name.is_empty() {
                attrs.push((name, raw));
            }
            sibling = node.prev_named_sibling();
        }
    }
    attrs
}

fn subscriber_target(attributes: &[(String, String)]) -> Option<(String, String)> {
    let (_, raw) = attributes
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(al_insight::attr_names::EVENT_SUBSCRIBER))?;
    let args = al_insight::calls::extract_attribute_args(raw);
    Some((
        al_syntax::clean_attr_arg(args.get(1)?),
        al_syntax::clean_attr_arg(args.get(2)?),
    ))
}

fn subscriber_event_is_resolved(insight: &InsightGraph, object: &str, event: &str) -> bool {
    [
        ObjectKind::Codeunit,
        ObjectKind::Table,
        ObjectKind::Page,
        ObjectKind::Report,
        ObjectKind::XmlPort,
        ObjectKind::Query,
        ObjectKind::Interface,
        ObjectKind::TableExtension,
        ObjectKind::PageExtension,
        ObjectKind::ReportExtension,
    ]
    .iter()
    .any(|kind| {
        insight
            .get_node(&NodeKey::Event(
                *kind,
                object.to_lowercase(),
                event.to_lowercase(),
            ))
            .is_some()
    })
}

fn collect_effect_sites(
    procedure: tree_sitter::Node<'_>,
    tree: &tree_sitter::Tree,
    source: &str,
) -> (Vec<EffectSite>, Vec<EffectSite>) {
    let bytes = source.as_bytes();
    let resolver = al_syntax::TypeResolver::new(tree, source);
    let mut writes = Vec::new();
    let mut commits = Vec::new();
    let mut stack = vec![procedure];
    while let Some(node) = stack.pop() {
        if node.kind() == "postfix_expression" {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            let Some(last) = children.last().copied() else {
                continue;
            };
            match last.kind() {
                "call_suffix" => {
                    let Some(primary) = children.first().copied() else {
                        continue;
                    };
                    let name = primary.utf8_text(bytes).unwrap_or("").trim();
                    if name.eq_ignore_ascii_case("Commit") {
                        commits.push(effect_site(primary, bytes, "Commit()"));
                    }
                }
                "member_call_suffix" | "scope_call_suffix" => {
                    let Some(member) = last.child_by_field_name("member") else {
                        continue;
                    };
                    let method = member
                        .utf8_text(bytes)
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"');
                    if !is_database_write_method(method) {
                        continue;
                    }
                    let Some(receiver_node) = children.first().copied() else {
                        continue;
                    };
                    let receiver = receiver_node
                        .utf8_text(bytes)
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"');
                    let pos = syntax_position(source, receiver_node.start_position());
                    let Some(decl) = resolver.resolve_type(receiver, pos) else {
                        continue;
                    };
                    if !matches!(
                        decl.type_name.to_ascii_lowercase().as_str(),
                        "record" | "recordref"
                    ) || declaration_is_temporary(tree, source, &decl)
                    {
                        continue;
                    }
                    writes.push(effect_site(
                        member,
                        bytes,
                        &format!("database write {receiver}.{method}()"),
                    ));
                }
                _ => {}
            }
            // A postfix expression owns its nested suffixes; do not descend and
            // accidentally report the same call twice.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    writes.sort_by_key(|site| site.byte_start);
    commits.sort_by_key(|site| site.byte_start);
    (writes, commits)
}

fn effect_site(node: tree_sitter::Node<'_>, source: &[u8], label: &str) -> EffectSite {
    EffectSite {
        range: al_syntax::ts_range_to_syntax(&node.range(), source).into(),
        byte_start: node.start_byte(),
        label: label.to_string(),
    }
}

fn syntax_position(source: &str, point: tree_sitter::Point) -> al_syntax::SyntaxPosition {
    let line = source.lines().nth(point.row).unwrap_or("");
    al_syntax::SyntaxPosition {
        line: point.row as u32,
        character: al_syntax::byte_col_to_utf16_col(line, point.column),
    }
}

fn declaration_is_temporary(
    tree: &tree_sitter::Tree,
    source: &str,
    decl: &al_syntax::VariableDecl,
) -> bool {
    let point = decl.range.start_point;
    let Some(mut node) = tree.root_node().descendant_for_point_range(point, point) else {
        return false;
    };
    loop {
        if matches!(
            node.kind(),
            "regular_variable_declaration"
                | "variable_declaration"
                | "object_variable_declaration"
                | "parameter"
        ) && node.utf8_text(source.as_bytes()).is_ok_and(|text| {
            text.split(|ch: char| !ch.is_alphanumeric())
                .any(|word| word.eq_ignore_ascii_case("temporary"))
        }) {
            return true;
        }
        if matches!(
            node.kind(),
            "var_section" | "object_var_section" | "procedure_declaration" | "trigger_declaration"
        ) {
            return false;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn is_database_write_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "insert" | "insertifnotexists" | "modify" | "modifyall" | "delete" | "deleteall" | "rename"
    )
}

fn is_post_database_event(event: &str) -> bool {
    let lower = event.to_ascii_lowercase();
    ["insert", "modify", "delete", "rename"]
        .iter()
        .any(|op| lower == format!("onafter{op}event") || lower == format!("onafter{op}"))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn workspace(files: &[(&str, &str)]) -> Workspace {
        let ws = Workspace::new();
        for (name, source) in files {
            ws.file_index.add_file(
                PathBuf::from(format!("/project/{name}")),
                source.to_string(),
            );
        }
        ws
    }

    fn install_dependency_source_package(
        workspace: &Workspace,
        sources: &[(&str, &str)],
    ) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let app_path = directory.path().join("Dependency_Source.app");
        let manifest = r#"<?xml version="1.0" encoding="utf-8"?>
<Package><App Id="00000000-0000-0000-0000-000000000099" Name="Dependency Source" Publisher="Test" Version="1.0.0.0" /></Package>"#;
        let symbols = r#"{
  "Codeunits": [
    { "Id": 70000, "Name": "Dependency Publisher", "Methods": [] },
    { "Id": 70001, "Name": "Dependency Writer", "Methods": [] }
  ]
}"#;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"NAVX");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 28]);
        let mut archive_bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(Cursor::new(&mut archive_bytes));
            let options = zip::write::SimpleFileOptions::default();
            archive.start_file("NavxManifest.xml", options).unwrap();
            archive.write_all(manifest.as_bytes()).unwrap();
            archive.start_file("SymbolReference.json", options).unwrap();
            archive.write_all(symbols.as_bytes()).unwrap();
            for (path, source) in sources {
                archive.start_file(*path, options).unwrap();
                archive.write_all(source.as_bytes()).unwrap();
            }
            archive.finish().unwrap();
        }
        bytes.extend_from_slice(&archive_bytes);
        std::fs::write(&app_path, bytes).unwrap();
        let loaded = workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .expect("valid dependency package");
        assert_eq!(loaded.len(), 1);
        workspace.invalidate_insight_graph();
        directory
    }

    #[test]
    fn commit_after_local_write_is_reported() {
        let ws = workspace(&[(
            "Local.al",
            r#"codeunit 50100 "Local"
{
    procedure Post()
    var
        Customer: Record Customer;
    begin
        Customer.Modify();
        Commit();
    end;
}"#,
        )]);
        let diagnostics = transaction_lints(&ws).unwrap();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == COMMIT_AFTER_DATABASE_CHANGE),
            "expected commit diagnostic, got {diagnostics:?}"
        );
    }

    #[test]
    fn commit_in_transitive_callee_is_reported() {
        let ws = workspace(&[(
            "Stack.al",
            r#"codeunit 50100 "Stack"
{
    procedure Start()
    var
        Customer: Record Customer;
    begin
        Customer.Modify();
        Finish();
    end;

    local procedure Finish()
    begin
        Commit();
    end;
}"#,
        )]);
        let diagnostics = transaction_lints(&ws).unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|d| d.code == COMMIT_AFTER_DATABASE_CHANGE)
            .expect("transitive commit must be reported");
        assert!(diagnostic.message.contains("Stack::Start -> Stack::Finish"));
    }

    #[test]
    fn write_in_transitive_try_stack_is_reported() {
        let ws = workspace(&[(
            "Try.al",
            r#"codeunit 50100 "Try Stack"
{
    [TryFunction]
    procedure TryPost()
    begin
        WriteCustomer();
    end;

    local procedure WriteCustomer()
    var
        Customer: Record Customer;
    begin
        Customer.Insert();
    end;
}"#,
        )]);
        let diagnostics = transaction_lints(&ws).unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|d| d.code == DATABASE_WRITE_IN_TRY_STACK)
            .expect("try-stack write must be reported");
        assert!(diagnostic
            .message
            .contains("Try Stack::TryPost -> Try Stack::WriteCustomer"));
    }

    #[test]
    fn temporary_record_write_is_not_reported() {
        let ws = workspace(&[(
            "Temp.al",
            r#"codeunit 50100 "Temporary"
{
    [TryFunction]
    procedure FillBuffer()
    var
        Buffer: Record Customer temporary;
    begin
        Buffer.Insert();
        Commit();
    end;
}"#,
        )]);
        let diagnostics = transaction_lints(&ws).unwrap();
        assert!(
            diagnostics.is_empty(),
            "temporary writes are not database changes: {diagnostics:?}"
        );
    }

    #[test]
    fn dependency_post_write_event_marks_subscriber_commit() {
        let ws = workspace(&[(
            "Subscriber.al",
            r#"codeunit 50100 "Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::Customer, 'OnAfterModifyEvent', '', false, false)]
    local procedure AfterCustomerModify()
    begin
        Commit();
    end;
}"#,
        )]);
        // The target object comes from dependency symbols in production. A
        // minimal symbol entry is enough to prove the same resolution path.
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        }]);
        let diagnostics = transaction_lints(&ws).unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|d| d.code == COMMIT_AFTER_DATABASE_CHANGE)
            .expect("post-write dependency event must make Commit unsafe");
        assert!(diagnostic.message.contains("dependency symbols"));
    }

    #[test]
    fn dependency_source_write_and_event_reach_project_commit() {
        let ws = workspace(&[(
            "Subscriber.al",
            r#"codeunit 50100 "Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Dependency Publisher", 'OnAfterMutate', '', false, false)]
    local procedure AfterDependencyMutation()
    begin
        Commit();
    end;
}"#,
        )]);
        let _package = install_dependency_source_package(
            &ws,
            &[(
                "src/DependencyPublisher.al",
                r#"codeunit 70000 "Dependency Publisher"
{
    procedure Mutate()
    var
        Customer: Record Customer;
    begin
        Customer.Modify();
        OnAfterMutate();
    end;

    [IntegrationEvent(false, false)]
    local procedure OnAfterMutate()
    begin
    end;
}"#,
            )],
        );

        let diagnostics = transaction_lints(&ws).unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == COMMIT_AFTER_DATABASE_CHANGE)
            .expect("dependency source write/event stack must reach project Commit");
        assert!(diagnostic.message.contains("Dependency Publisher::Mutate"));
        assert!(!diagnostic.message.contains("dependency symbols"));
    }

    #[test]
    fn project_try_function_reports_write_inside_dependency_source() {
        let ws = workspace(&[(
            "TryDependency.al",
            r#"codeunit 50100 "Try Dependency"
{
    [TryFunction]
    procedure TryDependencyWrite()
    var
        Writer: Codeunit "Dependency Writer";
    begin
        Writer.WriteCustomer();
    end;
}"#,
        )]);
        let _package = install_dependency_source_package(
            &ws,
            &[(
                "src/DependencyWriter.al",
                r#"codeunit 70001 "Dependency Writer"
{
    procedure WriteCustomer()
    var
        Customer: Record Customer;
    begin
        Customer.Modify();
    end;
}"#,
            )],
        );

        let diagnostics = transaction_lints(&ws).unwrap();
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == DATABASE_WRITE_IN_TRY_STACK)
            .expect("dependency source write must be visible from project TryFunction");
        assert_eq!(diagnostic.file, PathBuf::from("/project/TryDependency.al"));
        assert!(diagnostic.message.contains("in dependency source"));
        assert!(diagnostic
            .message
            .contains("Dependency Writer::WriteCustomer"));
    }

    #[test]
    fn try_stack_follows_record_event_into_subscriber_body() {
        let ws = workspace(&[(
            "EventStack.al",
            r#"codeunit 50100 "Event Stack"
{
    [TryFunction]
    procedure TryModifyCustomer()
    var
        Customer: Record Customer;
    begin
        Customer.Modify(true);
    end;

    [EventSubscriber(ObjectType::Table, Database::Customer, 'OnAfterModifyEvent', '', false, false)]
    local procedure AfterCustomerModify()
    var
        Vendor: Record Vendor;
    begin
        Vendor.Modify();
    end;
}"#,
        )]);
        ws.symbols.add_entries(&[
            al_symbols::SymbolEntry {
                kind: ObjectKind::Table,
                id: 18,
                name: "Customer".to_string(),
                package: "Base Application".to_string(),
                ..Default::default()
            },
            al_symbols::SymbolEntry {
                kind: ObjectKind::Table,
                id: 23,
                name: "Vendor".to_string(),
                package: "Base Application".to_string(),
                ..Default::default()
            },
        ]);

        let diagnostics = transaction_lints(&ws).unwrap();
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DATABASE_WRITE_IN_TRY_STACK
                    && diagnostic.message.contains("Vendor.Modify")
                    && diagnostic.message.contains(
                        "Event Stack::TryModifyCustomer -> Event Stack::AfterCustomerModify",
                    )
            }),
            "record event must reach subscriber body: {diagnostics:?}"
        );
    }

    #[test]
    fn no_write_means_no_transaction_diagnostic() {
        let ws = workspace(&[(
            "Clean.al",
            r#"codeunit 50100 "Clean"
{
    procedure Work()
    begin
        Message('No write');
    end;
}"#,
        )]);
        assert!(transaction_lints(&ws).unwrap().is_empty());
    }

    #[test]
    fn rule_catalog_exposes_both_graph_rules() {
        assert_eq!(transaction_lint_rules().len(), 2);
        assert!(transaction_lint_rules()
            .iter()
            .any(|rule| rule.code == COMMIT_AFTER_DATABASE_CHANGE));
        assert!(transaction_lint_rules()
            .iter()
            .any(|rule| rule.code == DATABASE_WRITE_IN_TRY_STACK));
    }
}
