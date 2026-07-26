//! Test-to-code coverage mapping — `al test-coverage`.
//!
//! Static analysis that maps test procedures to reachable production
//! procedures. A conservative direct pass handles uniquely named bare calls;
//! the resolved workspace call graph supplies qualified member, transitive,
//! event, trigger, `Codeunit.Run`, and interface-dispatch edges.
//!
//! The graph pass also credits polymorphic/indirect dispatch:
//! - **interface dispatch** — `IFoo`-typed `.Bar()` covers `Bar` in every
//!   implementor;
//! - **`Codeunit.Run(Codeunit::"X")`** covers `X.OnRun`;
//! - **event publish sites** cover the bound `[EventSubscriber]` handlers.
//!
//! Overloaded/otherwise ambiguous calls are never silently credited. They are
//! returned in `unresolvedCalls`, so the report remains conservative rather
//! than turning an unresolved target into a false coverage claim.
//!
//! Output: per-test-procedure list of called production procedures, plus a
//! reverse map of untested public production procedures.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::queries::tests::{collect_test_procedures, has_test_subtype};
use al_insight::graph::NodeKey;
use al_insight::index::{CallGraph, EdgeKind, NodeId};
use al_workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub enum TestCoverageError {
    #[error(transparent)]
    Workspace(#[from] super::WorkspaceQueryError),
    #[error(transparent)]
    Graph(#[from] al_workspace::CallGraphBuildError),
}

/// A production procedure identified as being covered by tests.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoveredProcedure {
    pub name: String,
    pub object: String,
    pub file: String,
    /// Line number of the procedure declaration (1-based).
    pub line: u32,
}

/// A public production procedure with no test coverage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UntestedProcedure {
    pub name: String,
    pub object: String,
    pub file: String,
    /// Line number (1-based).
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCoverageEntry {
    pub codeunit: String,
    pub test_procedure: String,
    /// Production procedures reachable from this test.
    pub covers: Vec<CoveredProcedure>,
    /// Calls that could not be resolved to one qualified production procedure.
    pub unresolved_calls: Vec<UnresolvedCoverageCall>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedCoverageCall {
    pub name: String,
    pub candidates: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub coverage: Vec<TestCoverageEntry>,
    /// Public production procedures not called by any test.
    pub untested: Vec<UntestedProcedure>,
}

#[derive(Debug, Clone)]
struct ProcDef {
    name: String,
    object: String,
    file: String,
    line: u32,
    is_local: bool,
    is_test: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProcKey {
    object: String,
    name: String,
}

impl ProcKey {
    fn new(object: &str, name: &str) -> Self {
        Self {
            object: object.to_lowercase(),
            name: name.to_lowercase(),
        }
    }
}

pub fn test_coverage(workspace: &Workspace) -> Result<CoverageReport, TestCoverageError> {
    let sources =
        crate::workspace_sources::snapshot(workspace).map_err(super::WorkspaceQueryError::from)?;
    let all_procs = collect_all_procedures(&sources);

    let mut proc_by_name: HashMap<String, Vec<&ProcDef>> = HashMap::new();
    let mut proc_by_key: HashMap<ProcKey, Vec<&ProcDef>> = HashMap::new();
    for p in &all_procs {
        proc_by_name
            .entry(p.name.to_lowercase())
            .or_default()
            .push(p);
        proc_by_key
            .entry(ProcKey::new(&p.object, &p.name))
            .or_default()
            .push(p);
    }

    let mut coverage: Vec<TestCoverageEntry> = Vec::new();
    let mut covered_proc_keys: HashSet<ProcKey> = HashSet::new();

    for source_file in &sources {
        let path = source_file.path.to_string_lossy().to_string();
        if !al_syntax::language_data::is_test_container_kind(&source_file.object.info.kind) {
            continue;
        }

        let root = source_file.tree.root_node();
        let source = source_file.text.as_bytes();
        if !has_test_subtype(root, source) {
            continue;
        }

        let test_procs = collect_test_procedures(root, source);
        if test_procs.is_empty() {
            continue;
        }

        let codeunit_name = source_file.object.info.name.clone();

        let mut cursor = root.walk();
        collect_coverage_from_tree(
            root,
            source,
            &codeunit_name,
            &path,
            &test_procs,
            &proc_by_name,
            &mut coverage,
            &mut covered_proc_keys,
            &mut cursor,
        );
    }

    // Supplement uniquely resolved bare calls with the qualified, transitive
    // workspace call graph. Runs before `untested` is computed so a reachable
    // procedure is not reported as a false-negative.
    augment_coverage_with_call_graph(
        workspace,
        &proc_by_key,
        &mut coverage,
        &mut covered_proc_keys,
    )?;

    let untested: Vec<UntestedProcedure> = all_procs
        .iter()
        .filter(|p| {
            !p.is_local
                && !p.is_test
                && !covered_proc_keys.contains(&ProcKey::new(&p.object, &p.name))
        })
        .map(|p| UntestedProcedure {
            name: p.name.clone(),
            object: p.object.clone(),
            file: p.file.clone(),
            line: p.line,
        })
        .collect();

    Ok(CoverageReport { coverage, untested })
}

/// Credit qualified transitive reachability on top of the conservative bare
/// call pass.
///
/// The shared call graph resolves receiver types, same-object calls, event and
/// trigger flow, `Codeunit.Run`, and interface dispatch. We walk all executable
/// forward edge kinds, but never turn a graph node representing multiple AL
/// overload declarations into a coverage claim: that ambiguity is returned to
/// the caller instead.
fn augment_coverage_with_call_graph(
    workspace: &Workspace,
    proc_by_key: &HashMap<ProcKey, Vec<&ProcDef>>,
    coverage: &mut [TestCoverageEntry],
    covered_keys: &mut HashSet<ProcKey>,
) -> Result<(), al_workspace::CallGraphBuildError> {
    if coverage.is_empty() {
        return Ok(());
    }

    // Build a fully-resolved call graph (same as the affected-test path). This
    // resolves every workspace procedure's edges, including the indirect
    // ones, so a low-fanout test file is not silently skipped.
    let (insight, _cg_guard) = workspace.get_or_build_call_graph()?;
    let mut cg = CallGraph::build_from_insight(&insight);
    al_insight::calls::resolve_all_workspace_call_edges(
        &workspace.file_index,
        &workspace.symbols,
        &insight,
        &mut cg,
    )?;

    for entry in coverage.iter_mut() {
        let Some(test_node) = find_procedure_node(&insight, &entry.codeunit, &entry.test_procedure)
        else {
            continue;
        };

        let mut seen_nodes = HashSet::from([test_node]);
        let mut queue = VecDeque::new();
        for edge in cg.callees_of(test_node) {
            if is_executable_forward_edge(&edge.kind) && seen_nodes.insert(edge.to) {
                queue.push_back(edge.to);
            }
        }
        let mut seen_covered: HashSet<ProcKey> = entry
            .covers
            .iter()
            .map(|covered| ProcKey::new(&covered.object, &covered.name))
            .collect();
        let mut seen_unresolved: HashSet<String> = entry
            .unresolved_calls
            .iter()
            .map(|unresolved| unresolved.name.to_lowercase())
            .collect();

        while let Some(node) = queue.pop_front() {
            let Some(info) = cg.node_info(node) else {
                continue;
            };
            let key = ProcKey::new(&info.object, &info.name);
            let definitions = proc_by_key
                .get(&key)
                .map(|defs| {
                    defs.iter()
                        .copied()
                        .filter(|definition| !definition.is_test)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            match definitions.as_slice() {
                [definition] => {
                    covered_keys.insert(key.clone());
                    if seen_covered.insert(key.clone()) {
                        entry.covers.push(CoveredProcedure {
                            name: definition.name.clone(),
                            object: definition.object.clone(),
                            file: definition.file.clone(),
                            line: definition.line,
                        });
                    }
                }
                [] if info.name.eq_ignore_ascii_case("OnRun") => {
                    // Triggers are executable graph nodes but are not
                    // `procedure_declaration`s in the source procedure table.
                    covered_keys.insert(key.clone());
                    if seen_covered.insert(key.clone()) {
                        entry.covers.push(CoveredProcedure {
                            name: info.name.clone(),
                            object: info.object.clone(),
                            file: String::new(),
                            line: 0,
                        });
                    }
                }
                [] => {}
                many => {
                    if seen_unresolved.insert(info.name.to_lowercase()) {
                        entry.unresolved_calls.push(UnresolvedCoverageCall {
                            name: info.name.clone(),
                            candidates: many
                                .iter()
                                .map(|definition| {
                                    format!("{}:{}:{}", definition.object, definition.file, definition.line)
                                })
                                .collect(),
                            reason: "multiple overload declarations share this graph target; no overload was credited"
                                .to_string(),
                        });
                    }
                }
            }

            for edge in cg.callees_of(node) {
                if is_executable_forward_edge(&edge.kind) && seen_nodes.insert(edge.to) {
                    queue.push_back(edge.to);
                }
            }
        }
        entry.covers.sort_by(|left, right| {
            (&left.object, &left.name, left.line).cmp(&(&right.object, &right.name, right.line))
        });
        entry.unresolved_calls.sort_by(|left, right| {
            (&left.name, &left.candidates).cmp(&(&right.name, &right.candidates))
        });
    }
    Ok(())
}

fn is_executable_forward_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::DirectCall
            | EdgeKind::IndirectCall
            | EdgeKind::TriggerInvocation
            | EdgeKind::RecordTrigger
    )
}

/// Resolve a `(codeunit, procedure)` name pair to its call-graph node,
/// searching procedure nodes regardless of declaring object kind.
fn find_procedure_node(
    insight: &al_insight::graph::InsightGraph,
    codeunit: &str,
    procedure: &str,
) -> Option<NodeId> {
    let obj = codeunit.to_lowercase();
    let method = procedure.to_lowercase();
    for (key, indices) in insight.index.iter() {
        if let NodeKey::Procedure(_, o, m) = key {
            if *o == obj && *m == method {
                return indices.first().map(|idx| NodeId::from(*idx));
            }
        }
    }
    None
}

fn collect_all_procedures(sources: &[crate::workspace_sources::WorkspaceSource]) -> Vec<ProcDef> {
    let mut result = Vec::new();

    for source_file in sources {
        let path = source_file.path.to_string_lossy().to_string();
        let object_name = source_file.object.info.name.clone();

        let root = source_file.tree.root_node();
        let source = source_file.text.as_bytes();
        let is_test_cu =
            al_syntax::language_data::is_test_container_kind(&source_file.object.info.kind)
                && has_test_subtype(root, source);

        collect_procs_recursive(root, source, &object_name, &path, is_test_cu, &mut result);
    }

    result
}

fn collect_procs_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    object: &str,
    file: &str,
    in_test_codeunit: bool,
    result: &mut Vec<ProcDef>,
) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "procedure_declaration" {
                let is_local = has_local_modifier(node, source);
                let is_test = in_test_codeunit && has_test_attr_child(node, source);

                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        result.push(ProcDef {
                            name: name.trim_matches('"').to_string(),
                            object: object.to_string(),
                            file: file.to_string(),
                            line: node.start_position().row as u32 + 1,
                            is_local,
                            is_test,
                        });
                    }
                }
                // Skip children of procedure_declaration
                did_visit = true;
                continue;
            }
        }
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
}

fn has_local_modifier(proc_node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "local" {
            return true;
        }
        if let Ok(text) = child.utf8_text(source) {
            if text.eq_ignore_ascii_case("local") {
                return true;
            }
        }
    }
    false
}

fn has_test_attr_child(proc_node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            if let Ok(text) = child.utf8_text(source) {
                if crate::queries::tests::is_test_attribute(text) {
                    return true;
                }
            }
        }
    }
    false
}

// Eight tree-walk inputs (tree-sitter node, source, workspace, file path,
// per-file maps for callers / coverage / unresolved, accumulator). Grouping
// into a struct would not reduce the per-call setup.
#[allow(clippy::too_many_arguments)]
fn collect_coverage_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
    codeunit_name: &str,
    _file: &str,
    test_procs: &[crate::queries::tests::TestProcedure],
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
    coverage: &mut Vec<TestCoverageEntry>,
    covered_keys: &mut HashSet<ProcKey>,
    _cursor: &mut tree_sitter::TreeCursor,
) {
    let test_names: HashSet<String> = test_procs.iter().map(|p| p.name.to_lowercase()).collect();

    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "procedure_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(raw_name) = name_node.utf8_text(source) {
                        let proc_name = raw_name.trim_matches('"');
                        if test_names.contains(&proc_name.to_lowercase()) {
                            let (called, unresolved_calls) = collect_called_identifiers(
                                node,
                                source,
                                codeunit_name,
                                proc_lookup,
                            );
                            for covered in &called {
                                covered_keys.insert(ProcKey::new(&covered.object, &covered.name));
                            }
                            coverage.push(TestCoverageEntry {
                                codeunit: codeunit_name.to_string(),
                                test_procedure: proc_name.to_string(),
                                covers: called,
                                unresolved_calls,
                            });
                        }
                    }
                }
                // Do not descend into procedure_declaration children.
                did_visit = true;
                continue;
            }
        }
        if (!did_visit && cursor.goto_first_child()) || cursor.goto_next_sibling() {
            did_visit = false;
        } else if cursor.goto_parent() {
            did_visit = true;
            if cursor.node() == root {
                break;
            }
        } else {
            break;
        }
    }
}

fn collect_called_identifiers(
    node: tree_sitter::Node,
    source: &[u8],
    caller_object: &str,
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
) -> (Vec<CoveredProcedure>, Vec<UnresolvedCoverageCall>) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    let mut unresolved = Vec::new();
    collect_identifiers_recursive(
        node,
        source,
        caller_object,
        proc_lookup,
        &mut seen,
        &mut result,
        &mut unresolved,
    );
    (result, unresolved)
}

fn collect_identifiers_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    caller_object: &str,
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
    seen: &mut HashSet<String>,
    result: &mut Vec<CoveredProcedure>,
    unresolved: &mut Vec<UnresolvedCoverageCall>,
) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            let kind = node.kind();
            if kind == "method_call"
                || kind == "function_call"
                || kind == "invocation_expression"
                || kind == "call_expression"
            {
                if let Some(callee) = find_callee_name(node, source) {
                    let key = callee.to_lowercase();
                    if seen.insert(key.clone()) {
                        if let Some(defs) = proc_lookup.get(&key) {
                            let production = defs
                                .iter()
                                .copied()
                                .filter(|definition| !definition.is_test)
                                .collect::<Vec<_>>();
                            let same_object = production
                                .iter()
                                .copied()
                                .filter(|definition| {
                                    definition.object.eq_ignore_ascii_case(caller_object)
                                })
                                .collect::<Vec<_>>();
                            let candidates = if same_object.is_empty() {
                                production
                            } else {
                                same_object
                            };
                            match candidates.as_slice() {
                                [definition] => result.push(CoveredProcedure {
                                    name: definition.name.clone(),
                                    object: definition.object.clone(),
                                    file: definition.file.clone(),
                                    line: definition.line,
                                }),
                                [] => {}
                                many => unresolved.push(UnresolvedCoverageCall {
                                    name: callee.to_string(),
                                    candidates: many
                                        .iter()
                                        .map(|definition| {
                                            format!(
                                                "{}:{}:{}",
                                                definition.object,
                                                definition.file,
                                                definition.line
                                            )
                                        })
                                        .collect(),
                                    reason: "bare call matches multiple declarations; no candidate was credited"
                                        .to_string(),
                                }),
                            }
                        }
                    }
                }
            }
        }
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
}

fn find_callee_name<'a>(node: tree_sitter::Node, source: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "name" => {
                if let Ok(text) = child.utf8_text(source) {
                    return Some(text.trim_matches('"'));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_workspace_returns_empty_report() {
        let workspace = al_workspace::Workspace::new();
        let report = test_coverage(&workspace).unwrap();
        assert!(report.coverage.is_empty());
        assert!(report.untested.is_empty());
    }

    #[test]
    fn test_coverage_entry_serializes() {
        let entry = TestCoverageEntry {
            codeunit: "TestCU".to_string(),
            test_procedure: "TestSomething".to_string(),
            covers: vec![CoveredProcedure {
                name: "DoWork".to_string(),
                object: "MyCodeunit".to_string(),
                file: "MyCodeunit.al".to_string(),
                line: 10,
            }],
            unresolved_calls: Vec::new(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("testProcedure"));
        assert!(json.contains("TestSomething"));
        assert!(json.contains("DoWork"));
    }

    #[test]
    fn untested_procedure_serializes() {
        let p = UntestedProcedure {
            name: "PublicProc".to_string(),
            object: "Obj".to_string(),
            file: "Obj.al".to_string(),
            line: 5,
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("PublicProc"));
    }

    #[test]
    fn coverage_report_serializes() {
        let report = CoverageReport {
            coverage: Vec::new(),
            untested: Vec::new(),
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("coverage"));
        assert!(json.contains("untested"));
    }

    use std::path::PathBuf;

    fn workspace_with(files: &[(&str, &str)]) -> al_workspace::Workspace {
        let ws = al_workspace::Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    const PROD_CU: &str = r#"codeunit 50100 "Prod CU"
{
    procedure PublicProc()
    begin
    end;

    local procedure HelperProc()
    begin
    end;
}"#;

    const TEST_CU: &str = r#"codeunit 50101 "Test CU"
{
    Subtype = Test;

    [Test]
    procedure TestPublicProc()
    begin
        PublicProc();
    end;

    procedure SetupHelper()
    begin
    end;
}"#;

    #[test]
    fn test_codeunit_yields_coverage_entry_and_excludes_test_procs_from_untested() {
        let ws = workspace_with(&[("/src/Test.al", TEST_CU)]);
        let report = test_coverage(&ws).unwrap();

        assert_eq!(report.coverage.len(), 1, "one [Test] proc => one entry");
        let entry = &report.coverage[0];
        assert_eq!(entry.codeunit, "Test CU");
        assert_eq!(entry.test_procedure, "TestPublicProc");

        assert!(
            !report
                .untested
                .iter()
                .any(|u| u.name.eq_ignore_ascii_case("TestPublicProc")),
            "[Test] procedures are not production code and must not be untested"
        );

        // SetupHelper is a plain (non-[Test]) public proc inside a test
        // codeunit; the is_test gating is attribute-based, so it counts as
        // public production code and surfaces as untested.
        assert!(
            report.untested.iter().any(|u| u.name == "SetupHelper"),
            "non-[Test] helper in a test codeunit counts as untested production"
        );
    }

    #[test]
    fn untested_lists_public_excludes_local() {
        let ws = workspace_with(&[("/src/Prod.al", PROD_CU)]);
        let report = test_coverage(&ws).unwrap();

        assert!(
            report.coverage.is_empty(),
            "a workspace with no Subtype=Test codeunit yields no coverage"
        );

        let names: Vec<&str> = report.untested.iter().map(|u| u.name.as_str()).collect();
        assert!(
            names.contains(&"PublicProc"),
            "public proc with no coverage must be untested, got {names:?}"
        );
        assert!(
            !names.contains(&"HelperProc"),
            "local procedures must be excluded from untested, got {names:?}"
        );

        // Line number is 1-based: `procedure PublicProc()` is on source line 3.
        let pub_entry = report
            .untested
            .iter()
            .find(|u| u.name == "PublicProc")
            .expect("PublicProc present");
        assert_eq!(pub_entry.line, 3, "1-based line of PublicProc declaration");
        assert_eq!(pub_entry.object, "Prod CU");
        assert_eq!(pub_entry.file, "/src/Prod.al");
    }

    #[test]
    fn non_codeunit_object_skipped_for_coverage_but_procs_collected() {
        let table = r#"table 50200 "My Table"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
    }

    procedure Recalculate()
    begin
    end;
}"#;
        let ws = workspace_with(&[("/src/MyTable.al", table)]);
        let report = test_coverage(&ws).unwrap();

        assert!(report.coverage.is_empty(), "tables produce no coverage");
        assert!(
            report
                .untested
                .iter()
                .any(|u| u.name == "Recalculate" && u.object == "My Table"),
            "public table procedures are collected as untested production code"
        );
    }

    #[test]
    fn normal_codeunit_not_treated_as_test() {
        let normal = r#"codeunit 50300 "Normal CU"
{
    [Test]
    procedure LooksLikeTest()
    begin
    end;
}"#;
        let ws = workspace_with(&[("/src/Normal.al", normal)]);
        let report = test_coverage(&ws).unwrap();

        assert!(
            report.coverage.is_empty(),
            "no Subtype=Test => not a test codeunit => no coverage"
        );
        // Because the codeunit is not a test codeunit, is_test is false, so the
        // public procedure is reported as untested production code.
        assert!(
            report.untested.iter().any(|u| u.name == "LooksLikeTest"),
            "a [Test] proc in a NON-test codeunit is still untested production"
        );
    }

    #[test]
    fn test_codeunit_without_test_procs_yields_no_coverage() {
        let cu = r#"codeunit 50400 "Empty Test CU"
{
    Subtype = Test;

    procedure JustAHelper()
    begin
    end;
}"#;
        let ws = workspace_with(&[("/src/EmptyTest.al", cu)]);
        let report = test_coverage(&ws).unwrap();
        assert!(
            report.coverage.is_empty(),
            "test codeunit with zero [Test] procs => no coverage entries"
        );
    }

    #[test]
    fn multiple_test_codeunits_each_contribute_entries() {
        let cu_a = r#"codeunit 50500 "Test A"
{
    Subtype = Test;
    [Test]
    procedure TA()
    begin
    end;
}"#;
        let cu_b = r#"codeunit 50501 "Test B"
{
    Subtype = Test;
    [Test]
    procedure TB1()
    begin
    end;
    [Test]
    procedure TB2()
    begin
    end;
}"#;
        let ws = workspace_with(&[("/src/A.al", cu_a), ("/src/B.al", cu_b)]);
        let report = test_coverage(&ws).unwrap();

        assert_eq!(report.coverage.len(), 3, "1 + 2 [Test] procedures");
        let from_b = report
            .coverage
            .iter()
            .filter(|e| e.codeunit == "Test B")
            .count();
        assert_eq!(from_b, 2, "both Test B procedures carry the right codeunit");
    }

    #[test]
    fn find_callee_name_returns_first_identifier() {
        let src = r#"codeunit 50600 "X"
{
    procedure P()
    begin
        DoThing();
    end;
}"#;
        let result = al_syntax::AlParser::parse_quick(src);
        let tree = result.tree;
        let source = src.as_bytes();

        let mut cursor = tree.root_node().walk();
        let mut stack = vec![tree.root_node()];
        let mut found: Option<String> = None;
        while let Some(node) = stack.pop() {
            if let Some(name) = find_callee_name(node, source) {
                if name == "DoThing" {
                    found = Some(name.to_string());
                    break;
                }
            }
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        assert_eq!(
            found.as_deref(),
            Some("DoThing"),
            "find_callee_name should locate the callee identifier"
        );
    }

    #[test]
    fn has_local_modifier_detects_local_keyword() {
        let src = r#"codeunit 50700 "Y"
{
    procedure Public()
    begin
    end;

    local procedure Private()
    begin
    end;
}"#;
        let result = al_syntax::AlParser::parse_quick(src);
        let tree = result.tree;
        let source = src.as_bytes();

        let mut cursor = tree.root_node().walk();
        let mut stack = vec![tree.root_node()];
        let mut public_is_local = None;
        let mut private_is_local = None;
        while let Some(node) = stack.pop() {
            if node.kind() == "procedure_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        let is_local = has_local_modifier(node, source);
                        match name {
                            "Public" => public_is_local = Some(is_local),
                            "Private" => private_is_local = Some(is_local),
                            _ => {}
                        }
                    }
                }
            }
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        assert_eq!(public_is_local, Some(false), "plain procedure is not local");
        assert_eq!(private_is_local, Some(true), "local procedure is local");
    }

    // ---- indirect-dispatch coverage --------------------------------

    #[test]
    fn coverage_credits_interface_dispatch_to_implementor() {
        let iface = r#"interface IFoo
{
    procedure Bar()
}"#;
        let impl_a = r#"codeunit 50101 "Impl A" implements "IFoo"
{
    procedure Bar()
    begin
    end;
}"#;
        let test_cu = r#"codeunit 50100 "Dispatch Test"
{
    Subtype = Test;

    [Test]
    procedure TestDispatch()
    var
        Foo: Interface "IFoo";
    begin
        Foo.Bar();
    end;
}"#;
        let ws = workspace_with(&[
            ("/ws/IFoo.Interface.al", iface),
            ("/ws/ImplA.Codeunit.al", impl_a),
            ("/ws/DispatchTest.Codeunit.al", test_cu),
        ]);
        let report = test_coverage(&ws).unwrap();

        let entry = report
            .coverage
            .iter()
            .find(|e| e.test_procedure == "TestDispatch")
            .expect("TestDispatch coverage entry");
        assert!(
            entry
                .covers
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case("Bar") && c.object == "Impl A"),
            "interface dispatch must credit Impl A.Bar as covered, got {:?}",
            entry.covers
        );
        // And the now-covered implementor must not appear as untested.
        assert!(
            !report
                .untested
                .iter()
                .any(|u| u.name.eq_ignore_ascii_case("Bar")),
            "indirectly-covered Bar must not be reported untested"
        );
    }

    #[test]
    fn qualified_same_named_calls_credit_only_the_resolved_object_and_transitive_callee() {
        let target = r#"codeunit 50101 "Target A"
{
    procedure Shared()
    begin
        Helper();
    end;

    procedure Helper()
    begin
    end;
}"#;
        let other = r#"codeunit 50102 "Target B"
{
    procedure Shared()
    begin
    end;
}"#;
        let test_cu = r#"codeunit 50100 "Qualified Test"
{
    Subtype = Test;

    [Test]
    procedure TestQualified()
    var
        Target: Codeunit "Target A";
    begin
        Target.Shared();
    end;
}"#;
        let ws = workspace_with(&[
            ("/ws/TargetA.Codeunit.al", target),
            ("/ws/TargetB.Codeunit.al", other),
            ("/ws/QualifiedTest.Codeunit.al", test_cu),
        ]);
        let report = test_coverage(&ws).unwrap();
        let entry = report
            .coverage
            .iter()
            .find(|entry| entry.test_procedure == "TestQualified")
            .expect("TestQualified coverage entry");

        assert!(
            entry.unresolved_calls.is_empty(),
            "{:?}",
            entry.unresolved_calls
        );
        assert!(entry
            .covers
            .iter()
            .any(|covered| covered.object == "Target A" && covered.name == "Shared"));
        assert!(
            entry
                .covers
                .iter()
                .any(|covered| covered.object == "Target A" && covered.name == "Helper"),
            "transitive same-object call must be reachable: {:?}",
            entry.covers
        );
        assert!(
            !entry
                .covers
                .iter()
                .any(|covered| covered.object == "Target B"),
            "same-named method in another object must not receive false coverage"
        );
        assert!(report
            .untested
            .iter()
            .any(|procedure| procedure.object == "Target B" && procedure.name == "Shared"));
    }

    #[test]
    fn overloaded_graph_target_is_reported_unresolved_and_not_credited() {
        let target = r#"codeunit 50101 "Overloaded Target"
{
    procedure Shared(Value: Integer)
    begin
    end;

    procedure Shared(Value: Text)
    begin
    end;
}"#;
        let test_cu = r#"codeunit 50100 "Overload Test"
{
    Subtype = Test;

    [Test]
    procedure TestOverload()
    var
        Target: Codeunit "Overloaded Target";
    begin
        Target.Shared(1);
    end;
}"#;
        let ws = workspace_with(&[
            ("/ws/Overloaded.Codeunit.al", target),
            ("/ws/OverloadTest.Codeunit.al", test_cu),
        ]);
        let report = test_coverage(&ws).unwrap();
        let entry = report
            .coverage
            .iter()
            .find(|entry| entry.test_procedure == "TestOverload")
            .expect("TestOverload coverage entry");

        assert!(
            !entry
                .covers
                .iter()
                .any(|covered| covered.object == "Overloaded Target" && covered.name == "Shared"),
            "an unresolved overload must not be falsely credited"
        );
        let unresolved = entry
            .unresolved_calls
            .iter()
            .find(|call| call.name == "Shared")
            .expect("overload ambiguity must be explicit");
        assert_eq!(unresolved.candidates.len(), 2);
        assert_eq!(
            report
                .untested
                .iter()
                .filter(|procedure| {
                    procedure.object == "Overloaded Target" && procedure.name == "Shared"
                })
                .count(),
            2,
            "both overload declarations remain uncredited"
        );
    }

    #[test]
    fn coverage_credits_codeunit_run_to_onrun() {
        let worker = r#"codeunit 50201 "Worker CU"
{
    trigger OnRun()
    begin
    end;
}"#;
        let test_cu = r#"codeunit 50200 "Run Test"
{
    Subtype = Test;

    [Test]
    procedure TestRun()
    begin
        Codeunit.Run(Codeunit::"Worker CU");
    end;
}"#;
        let ws = workspace_with(&[
            ("/ws/Worker.Codeunit.al", worker),
            ("/ws/RunTest.Codeunit.al", test_cu),
        ]);
        let report = test_coverage(&ws).unwrap();

        let entry = report
            .coverage
            .iter()
            .find(|e| e.test_procedure == "TestRun")
            .expect("TestRun coverage entry");
        assert!(
            entry
                .covers
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case("OnRun") && c.object == "Worker CU"),
            "Codeunit.Run must credit Worker CU.OnRun, got {:?}",
            entry.covers
        );
    }
}
