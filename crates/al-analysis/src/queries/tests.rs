//! Test discovery query — `al tests`.
//!
//! Uses tree-sitter static analysis to find [Test] codeunits and [Test] procedures
//! in AL source files. No runtime connection to BC required.

use std::collections::HashSet;

use serde::Serialize;

use al_insight::graph::NodeKey;
use al_insight::index::{CallGraph, NodeId};
use al_symbols::ObjectKind;
use al_workspace::Workspace;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProcedure {
    pub name: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCodeunit {
    pub name: String,
    pub id: i32,
    pub file: String,
    pub tests: Vec<TestProcedure>,
}

/// Discover all [Test] codeunits and procedures in workspace .al files.
pub fn discover_tests(workspace: &Workspace) -> Vec<TestCodeunit> {
    let mut results = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let entry_path = entry.key();
        let path = entry_path.to_string_lossy().to_string();
        let Some((text, tree)) = workspace.file_index.get_cached_parse(entry_path) else {
            continue;
        };
        let source = text.as_bytes();

        let Some(obj_info) = al_syntax::find_object_declaration(&tree, &text) else {
            continue;
        };
        if !al_syntax::language_data::is_test_container_kind(&obj_info.kind) {
            continue;
        }

        let obj_id = obj_info.id.unwrap_or(0) as i32;
        let root = tree.root_node();
        let is_test_subtype = has_test_subtype(root, source);
        let test_procs = collect_test_procedures(root, source);

        if is_test_subtype || !test_procs.is_empty() {
            results.push(TestCodeunit {
                name: obj_info.name.clone(),
                id: obj_id,
                file: path,
                tests: test_procs,
            });
        }
    }

    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

/// One discovered test affected by a set of changed files.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedTest {
    pub codeunit_id: i32,
    pub codeunit_name: String,
    pub method_name: String,
    pub file: String,
    /// 1-based line number of the procedure declaration.
    pub line: u32,
}

/// How a set of [`AffectedTest`]s was selected. Surfaced so callers and any
/// user-facing output stay honest about whether the precise call-graph path
/// ran or we fell back to coarse file-name matching ().
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AffectedMode {
    /// Tests selected by transitive call-graph reachability: a test is affected
    /// iff it (or something it transitively calls) reaches a procedure/event of
    /// a changed object.
    CallGraph,
    /// Coarse fallback used when no changed path resolves to a graph node
    /// (e.g. the file isn't an indexed AL object): a test is affected iff its
    /// own source file is in the changed list.
    FileBased,
}

/// Result of [`affected_tests_detailed`]: the affected tests plus the mode that
/// produced them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedTestsResult {
    pub tests: Vec<AffectedTest>,
    pub mode: AffectedMode,
}

/// Return the tests affected by a set of changed files.
///
/// Prefers **call-graph reachability** (): each changed file is mapped to
/// its AL object, every member (procedure / event / subscriber) of that object
/// seeds a backward walk of the call graph, and a test is affected iff its
/// procedure node is reached. This catches tests whose *helpers* changed — not
/// just tests whose own file changed — and ignores tests that only touch
/// unrelated code.
///
/// Falls back to the legacy file-name match when no changed path resolves to a
/// graph node (so the result is never worse than before). Use
/// [`affected_tests_detailed`] when you need to know which mode ran.
pub fn affected_tests(workspace: &Workspace, changed_paths: &[String]) -> Vec<AffectedTest> {
    affected_tests_detailed(workspace, changed_paths).tests
}

/// Like [`affected_tests`] but also reports the [`AffectedMode`] used.
pub fn affected_tests_detailed(
    workspace: &Workspace,
    changed_paths: &[String],
) -> AffectedTestsResult {
    if changed_paths.is_empty() {
        return AffectedTestsResult {
            tests: Vec::new(),
            mode: AffectedMode::CallGraph,
        };
    }

    if let Some(tests) = affected_via_call_graph(workspace, changed_paths) {
        return AffectedTestsResult {
            tests,
            mode: AffectedMode::CallGraph,
        };
    }

    AffectedTestsResult {
        tests: affected_tests_file_based(workspace, changed_paths),
        mode: AffectedMode::FileBased,
    }
}

/// Call-graph affected-test detection.
///
/// Returns `None` (signalling the caller to fall back to file matching) when no
/// changed path resolves to an indexed AL object, or when the changed objects
/// have no members in the graph — i.e. when a call-graph answer would be
/// vacuous rather than merely empty.
fn affected_via_call_graph(
    workspace: &Workspace,
    changed_paths: &[String],
) -> Option<Vec<AffectedTest>> {
    // Map every changed file to the (kind, name) of the AL object it declares.
    let want: HashSet<(ObjectKind, String)> = changed_paths
        .iter()
        .filter_map(|p| object_identity_for_path(workspace, p))
        .collect();
    if want.is_empty() {
        return None;
    }

    // The cached workspace call graph only resolves high-fanout "Tier 1" files
    // eagerly. For affected-test detection a missing edge is a false negative
    // (a dependent test silently skipped), so build a fully-resolved graph over
    // the node-complete cached insight graph. `get_or_build_call_graph` has
    // already registered every workspace node and enriched the symbol index.
    let insight = {
        let (insight, _cg_guard) = workspace.get_or_build_call_graph();
        insight
    };
    let mut cg = CallGraph::build_from_insight(&insight);
    al_insight::calls::resolve_all_workspace_call_edges(
        &workspace.file_index,
        &workspace.symbols,
        &insight,
        &mut cg,
    );

    // Seeds: every procedure / event / subscriber node of a changed object.
    let mut seeds: Vec<NodeId> = Vec::new();
    for (key, indices) in insight.index.iter() {
        let belongs = match key {
            NodeKey::Procedure(kind, obj, _)
            | NodeKey::Event(kind, obj, _)
            | NodeKey::Subscriber(kind, obj, _) => want.contains(&(*kind, obj.clone())),
            NodeKey::Object(..) => false,
        };
        if belongs {
            seeds.extend(indices.iter().map(|idx| NodeId::from(*idx)));
        }
    }
    if seeds.is_empty() {
        // Changed object(s) exist but contribute no graph members (e.g. an
        // empty table). Don't claim a precise empty answer — let the caller
        // fall back to file matching.
        return None;
    }

    let reachable = cg.reachable_callers(seeds);

    let mut affected = Vec::new();
    for cu in discover_tests(workspace) {
        let kind = object_identity_for_path(workspace, &cu.file)
            .map(|(k, _)| k)
            .unwrap_or(ObjectKind::Codeunit);
        let obj_lower = cu.name.to_lowercase();
        for proc in &cu.tests {
            let key = NodeKey::Procedure(kind, obj_lower.clone(), proc.name.to_lowercase());
            let Some(node_id) = CallGraph::node_id_for(&insight, &key) else {
                continue;
            };
            if reachable.contains(&node_id) {
                affected.push(AffectedTest {
                    codeunit_id: cu.id,
                    codeunit_name: cu.name.clone(),
                    method_name: proc.name.clone(),
                    file: cu.file.clone(),
                    line: proc.line,
                });
            }
        }
    }
    Some(affected)
}

/// Resolve the `(ObjectKind, lowercased name)` of the AL object declared in
/// `path`, using the file index's cached object info. Tries the path as given,
/// then its canonical form (the daemon and the index may disagree on absolute
/// vs symlinked paths).
fn object_identity_for_path(workspace: &Workspace, path: &str) -> Option<(ObjectKind, String)> {
    let from_info = |info: &al_source::file_index::CachedObjectInfo| {
        info.kind
            .parse::<ObjectKind>()
            .ok()
            .map(|k| (k, info.name.to_lowercase()))
    };

    let p = std::path::Path::new(path);
    if let Some(info) = workspace.file_index.object_info.get(p) {
        if let Some(id) = from_info(&info) {
            return Some(id);
        }
    }
    if let Ok(canon) = p.canonicalize() {
        if let Some(info) = workspace.file_index.object_info.get(&canon) {
            if let Some(id) = from_info(&info) {
                return Some(id);
            }
        }
    }
    None
}

/// Legacy fallback: a test is affected iff its own file is in `changed_paths`.
fn affected_tests_file_based(workspace: &Workspace, changed_paths: &[String]) -> Vec<AffectedTest> {
    // Normalise both sides to absolute path strings for comparison.
    let normalised_changed: HashSet<String> = changed_paths
        .iter()
        .map(|p| {
            std::path::Path::new(p)
                .canonicalize()
                .ok()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .collect();

    let mut affected = Vec::new();
    for cu in discover_tests(workspace) {
        let cu_canonical = std::path::Path::new(&cu.file)
            .canonicalize()
            .ok()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|| cu.file.clone());
        if normalised_changed.contains(&cu_canonical) || normalised_changed.contains(&cu.file) {
            for proc in cu.tests {
                affected.push(AffectedTest {
                    codeunit_id: cu.id,
                    codeunit_name: cu.name.clone(),
                    method_name: proc.name,
                    file: cu.file.clone(),
                    line: proc.line,
                });
            }
        }
    }
    affected
}

/// Check if the codeunit has `Subtype = Test`.
///
/// tightened from a fragile substring match
/// (`lower.contains("subtype") && lower.contains("test")`) to a proper
/// `<key> = <value>` parse. Pre-a property like
/// `Description = 'Has Subtype = Test in description';` would have
/// false-matched, and any property whose name contained both substrings
/// (e.g. a hypothetical SubtypeFilter property) would have too. Now we
/// only match when the property KEY is exactly `subtype` (case-
/// insensitive) AND the value is exactly `test`.
pub fn has_test_subtype(root: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "property" || node.kind() == "property_assignment" {
                if let Ok(text) = node.utf8_text(source) {
                    if let Some((key, value)) = text.split_once('=') {
                        let k = key.trim();
                        // Strip trailing ; and surrounding whitespace; tolerate
                        // single-quoted enum values (the AL printer doesn't quote
                        // bare identifiers but we're robust either way).
                        let v = value.trim().trim_end_matches(';').trim().trim_matches('\'');
                        if k.eq_ignore_ascii_case("subtype") && v.eq_ignore_ascii_case("test") {
                            return true;
                        }
                    }
                }
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
    false
}

pub fn collect_test_procedures(root: tree_sitter::Node, source: &[u8]) -> Vec<TestProcedure> {
    let mut procs = Vec::new();
    collect_test_procs_iterative(root, source, &mut procs);
    procs
}

/// Iterative tree-walk (despite the historical `_recursive` name, retained
/// elsewhere in this crate's history): uses `tree_sitter::TreeCursor`
/// goto_first_child / goto_next_sibling / goto_parent. No self-recursion,
/// no Vec stack needed because the cursor is the stack.
fn collect_test_procs_iterative(
    root: tree_sitter::Node,
    source: &[u8],
    procs: &mut Vec<TestProcedure>,
) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "procedure_declaration" {
                if has_test_attribute(node, source) {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(name) = name_node.utf8_text(source) {
                            procs.push(TestProcedure {
                                name: name.trim_matches('"').to_string(),
                                line: node.start_position().row as u32 + 1,
                            });
                        }
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

fn has_test_attribute(proc_node: tree_sitter::Node, source: &[u8]) -> bool {
    // In AL tree-sitter grammar, attributes are children of procedure_declaration
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            if let Ok(text) = child.utf8_text(source) {
                if is_test_attribute(text) {
                    return true;
                }
            }
        }
    }
    let mut sibling = proc_node.prev_sibling();
    while let Some(s) = sibling {
        match s.kind() {
            "attribute" | "attribute_list" => {
                if let Ok(text) = s.utf8_text(source) {
                    if is_test_attribute(text) {
                        return true;
                    }
                }
            }
            "comment" => {}
            _ => break,
        }
        sibling = s.prev_sibling();
    }
    false
}

/// Return true if the attribute text is `[Test]` (case-insensitive, not TestPermissions etc.).
pub fn is_test_attribute(text: &str) -> bool {
    let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case("test"))
}

#[cfg(test)]
mod test_discovery {
    use super::*;
    use al_syntax::AlParser;

    #[test]
    fn test_attribute_detection() {
        assert!(is_test_attribute("[test]"));
        assert!(is_test_attribute("[Test]"));
        assert!(!is_test_attribute("[TestPermissions(All)]"));
        assert!(!is_test_attribute("[TestInitialize]"));
        assert!(!is_test_attribute("[TestCleanup]"));
    }

    #[test]
    fn discover_test_procedures_from_source() {
        let source = r#"codeunit 50100 "My Tests"
{
    Subtype = Test;

    [Test]
    procedure TestSomething()
    begin
    end;

    [Test]
    procedure TestAnotherThing()
    begin
    end;

    procedure NotATest()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let tree = &result.tree;
        let root = tree.root_node();
        let bytes = source.as_bytes();

        let procs = collect_test_procedures(root, bytes);
        assert_eq!(
            procs.len(),
            2,
            "Expected 2 test procs, got: {:?}",
            procs.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert_eq!(procs[0].name, "TestSomething");
        assert_eq!(procs[1].name, "TestAnotherThing");
    }

    #[test]
    fn discover_test_subtype() {
        let source = r#"codeunit 50100 "My Tests"
{
    Subtype = Test;

    procedure Setup()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let tree = &result.tree;
        let root = tree.root_node();
        let bytes = source.as_bytes();
        assert!(has_test_subtype(root, bytes));
    }

    #[test]
    fn non_test_codeunit_ignored() {
        let source = r#"codeunit 50101 "Regular Codeunit"
{
    procedure DoSomething()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let tree = &result.tree;
        let root = tree.root_node();
        let bytes = source.as_bytes();
        assert!(!has_test_subtype(root, bytes));
        let procs = collect_test_procedures(root, bytes);
        assert!(procs.is_empty());
    }

    #[test]
    fn discover_tests_empty_workspace() {
        let workspace = al_workspace::Workspace::new();
        let results = discover_tests(&workspace);
        assert!(results.is_empty());
    }
}

#[cfg(test)]
mod adversarial_j_tests {
    use super::*;
    use al_syntax::AlParser;

    /// `has_test_subtype` must not fire on a codeunit that has
    /// Subtype = Normal even if the text of the property node contains the word "test"
    /// in a different context (e.g. a second property line).
    ///
    /// This test will PASS because the grammar creates separate property nodes — the
    /// "test" word appears in a comment/separate property, not the Subtype value.
    /// If the grammar ever groups them into one node, this documents the expected behaviour.
    #[test]
    fn test_has_test_subtype_does_not_match_subtype_normal_adversarial_j_3() {
        let source = r#"codeunit 50200 "Normal Codeunit"
{
    Subtype = Normal;

    procedure TestSomething()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let root = result.tree.root_node();
        let bytes = source.as_bytes();
        assert!(
            !has_test_subtype(root, bytes),
            "Subtype = Normal must NOT be detected as a test subtype"
        );
    }

    /// Negative companion: Subtype = Test must be detected.
    #[test]
    fn test_has_test_subtype_detects_subtype_test_adversarial_j_3() {
        let source = r#"codeunit 50201 "Test Codeunit"
{
    Subtype = Test;

    procedure Setup()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let root = result.tree.root_node();
        let bytes = source.as_bytes();
        assert!(
            has_test_subtype(root, bytes),
            "Subtype = Test must be detected as a test subtype"
        );
    }

    /// Verify that `has_test_subtype` does not treat a
    /// property like Subtype = Normal with a trailing comment containing the word
    /// "test" does NOT trigger a false positive.  This tests the boundary case where
    /// tree-sitter might include comment trivia in the property node text.
    #[test]
    fn test_has_test_subtype_comment_with_test_word_not_false_positive_adversarial_j_3() {
        // If the grammar includes the comment in the property node text, lower() would
        // contain both "subtype" and "test" — triggering a false positive.
        let source = r#"codeunit 50202 "Normal Codeunit With Comment"
{
    // This codeunit has test-like naming but is NOT a test codeunit
    Subtype = Normal;

    procedure Run()
    begin
    end;
}
"#;
        let result = AlParser::parse_quick(source);
        let root = result.tree.root_node();
        let bytes = source.as_bytes();
        assert!(
            !has_test_subtype(root, bytes),
            "Comment containing 'test' must NOT cause false-positive test subtype detection"
        );
    }
}

/// call-graph reachability for affected-test detection. A test is affected
/// iff it transitively calls a procedure of a changed object — not merely
/// because its own file changed (the old file-based heuristic).
#[cfg(test)]
mod affected_call_graph {
    use super::*;
    use std::path::PathBuf;

    const HELPER: &str = r#"codeunit 50100 Helper
{
    procedure DoWork()
    begin
    end;
}
"#;

    const UNRELATED: &str = r#"codeunit 50101 Unrelated
{
    procedure Other()
    begin
    end;
}
"#;

    // MidCu.Middle calls Helper.DoWork — the middle hop of the chain.
    const MIDCU: &str = r#"codeunit 50102 MidCu
{
    procedure Middle()
    var
        H: Codeunit Helper;
    begin
        H.DoWork();
    end;
}
"#;

    // TestDirect    → Helper.DoWork   (direct dependency)
    // TestTransitive→ MidCu.Middle    (→ Helper.DoWork; transitive)
    // TestUnrelated → Unrelated.Other (independent)
    const TESTS: &str = r#"codeunit 50103 MyTests
{
    Subtype = Test;

    [Test]
    procedure TestDirect()
    var
        H: Codeunit Helper;
    begin
        H.DoWork();
    end;

    [Test]
    procedure TestTransitive()
    var
        M: Codeunit MidCu;
    begin
        M.Middle();
    end;

    [Test]
    procedure TestUnrelated()
    var
        U: Codeunit Unrelated;
    begin
        U.Other();
    end;
}
"#;

    fn build_ws() -> Workspace {
        let ws = Workspace::new();
        ws.file_index
            .add_file(PathBuf::from("/ws/helper.al"), HELPER.to_string());
        ws.file_index
            .add_file(PathBuf::from("/ws/unrelated.al"), UNRELATED.to_string());
        ws.file_index
            .add_file(PathBuf::from("/ws/midcu.al"), MIDCU.to_string());
        ws.file_index
            .add_file(PathBuf::from("/ws/tests.al"), TESTS.to_string());
        ws
    }

    fn affected_names(ws: &Workspace, changed: &[&str]) -> (AffectedMode, Vec<String>) {
        let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
        let result = affected_tests_detailed(ws, &changed);
        let mut names: Vec<String> = result.tests.iter().map(|t| t.method_name.clone()).collect();
        names.sort();
        (result.mode, names)
    }

    #[test]
    fn changing_helper_marks_direct_and_transitive_callers() {
        let ws = build_ws();
        let (mode, names) = affected_names(&ws, &["/ws/helper.al"]);
        assert_eq!(mode, AffectedMode::CallGraph, "should use the call graph");
        // TestDirect calls Helper directly; TestTransitive reaches it via MidCu.
        assert!(
            names.contains(&"TestDirect".to_string()),
            "direct caller affected; got {names:?}"
        );
        assert!(
            names.contains(&"TestTransitive".to_string()),
            "transitive caller A->B->H affected; got {names:?}"
        );
        // The unrelated test must NOT be dragged in.
        assert!(
            !names.contains(&"TestUnrelated".to_string()),
            "unrelated test must not be affected; got {names:?}"
        );
        assert_eq!(names.len(), 2, "exactly the two reachable tests: {names:?}");
    }

    #[test]
    fn changing_unrelated_proc_does_not_mark_helper_callers() {
        let ws = build_ws();
        let (mode, names) = affected_names(&ws, &["/ws/unrelated.al"]);
        assert_eq!(mode, AffectedMode::CallGraph);
        assert_eq!(
            names,
            vec!["TestUnrelated".to_string()],
            "only the test that calls Unrelated is affected; got {names:?}"
        );
    }

    #[test]
    fn changing_intermediate_marks_only_transitive_caller() {
        // Changing MidCu (the middle hop) affects the test that goes through it,
        // but not the test that calls Helper directly nor the unrelated one.
        let ws = build_ws();
        let (mode, names) = affected_names(&ws, &["/ws/midcu.al"]);
        assert_eq!(mode, AffectedMode::CallGraph);
        assert_eq!(
            names,
            vec!["TestTransitive".to_string()],
            "only TestTransitive reaches MidCu; got {names:?}"
        );
    }

    #[test]
    fn changing_the_test_file_itself_marks_all_its_tests() {
        // Superset guarantee: the old file-based behaviour (a changed test file
        // marks all its tests) is preserved — each test proc is its own seed.
        let ws = build_ws();
        let (mode, names) = affected_names(&ws, &["/ws/tests.al"]);
        assert_eq!(mode, AffectedMode::CallGraph);
        assert_eq!(
            names,
            vec![
                "TestDirect".to_string(),
                "TestTransitive".to_string(),
                "TestUnrelated".to_string()
            ]
        );
    }

    #[test]
    fn unindexed_changed_path_falls_back_to_file_based() {
        // A changed path that isn't an indexed AL object can't be mapped to a
        // graph node, so detection honestly reports the file-based fallback.
        let ws = build_ws();
        let (mode, names) = affected_names(&ws, &["/ws/does-not-exist.al"]);
        assert_eq!(
            mode,
            AffectedMode::FileBased,
            "must report the fallback mode"
        );
        assert!(names.is_empty(), "no test file changed; got {names:?}");
    }

    #[test]
    fn empty_changed_set_is_empty() {
        let ws = build_ws();
        let result = affected_tests_detailed(&ws, &[]);
        assert!(result.tests.is_empty());
    }
}
