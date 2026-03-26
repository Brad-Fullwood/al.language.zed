//! Test-to-code coverage mapping — `al test-coverage`.
//!
//! Static analysis that maps test procedures to the production procedures they
//! call. Builds a call graph by walking procedure bodies looking for identifier
//! references that match known procedure names.
//!
//! Limitations (acknowledged in task):
//! - Does not resolve indirect calls through events or interface dispatch.
//! - Call resolution is name-based (not type-resolved). Overloaded names may
//!   produce false positives.
//!
//! Output: per-test-procedure list of called production procedures, plus a
//! reverse map of untested public production procedures.

use std::collections::{HashMap, HashSet};

use al_syntax::AlParser;
use serde::Serialize;

use crate::queries::tests::{collect_test_procedures, has_test_subtype};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A production procedure identified as being covered by tests.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoveredProcedure {
    /// Name of the production procedure.
    pub name: String,
    /// Object that contains this procedure.
    pub object: String,
    /// File path of the object.
    pub file: String,
    /// Line number of the procedure declaration (1-based).
    pub line: u32,
}

/// A public production procedure with no test coverage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UntestedProcedure {
    /// Procedure name.
    pub name: String,
    /// Object that contains this procedure.
    pub object: String,
    /// File path.
    pub file: String,
    /// Line number (1-based).
    pub line: u32,
}

/// Coverage information for a single test procedure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCoverageEntry {
    /// Test codeunit name.
    pub codeunit: String,
    /// Test procedure name.
    pub test_procedure: String,
    /// Production procedures called (directly) by this test.
    pub covers: Vec<CoveredProcedure>,
}

/// Full coverage report for a workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    /// Per-test coverage entries.
    pub coverage: Vec<TestCoverageEntry>,
    /// Public production procedures not called by any test.
    pub untested: Vec<UntestedProcedure>,
}

// ---------------------------------------------------------------------------
// Internal helper types
// ---------------------------------------------------------------------------

/// A procedure definition collected from workspace files.
#[derive(Debug, Clone)]
struct ProcDef {
    name: String,
    object: String,
    file: String,
    line: u32,
    is_local: bool,
    is_test: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a test-coverage map for the workspace.
///
/// 1. Scans all .al files and collects all procedure definitions.
/// 2. For each test procedure, walks its body collecting called identifiers.
/// 3. Matches identifiers against production procedure names.
/// 4. Reports untested public production procedures.
pub fn test_coverage(workspace: &Workspace) -> CoverageReport {
    // Step 1: collect all procedure defs
    let all_procs = collect_all_procedures(workspace);

    // Build a lookup map: lowercase name → list of ProcDef
    let mut proc_lookup: HashMap<String, Vec<&ProcDef>> = HashMap::new();
    for p in &all_procs {
        proc_lookup
            .entry(p.name.to_lowercase())
            .or_default()
            .push(p);
    }

    // Step 2: for each test procedure, collect the names it calls
    let mut coverage: Vec<TestCoverageEntry> = Vec::new();
    let mut covered_proc_names: HashSet<String> = HashSet::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key().to_string_lossy().to_string();
        let text = entry.value().clone();
        let parse_result = AlParser::parse_quick(&text);
        let tree = &parse_result.tree;

        let Some(obj_info) = al_syntax::find_object_declaration(tree, &text) else {
            continue;
        };
        if obj_info.kind.to_lowercase() != "codeunit" {
            continue;
        }

        let root = tree.root_node();
        let source = text.as_bytes();
        if !has_test_subtype(root, source) {
            continue;
        }

        let test_procs = collect_test_procedures(root, source);
        if test_procs.is_empty() {
            continue;
        }

        let codeunit_name = obj_info.name.clone();

        // For each test proc, find its body in the tree and collect called names
        let mut cursor = root.walk();
        collect_coverage_from_tree(
            root,
            source,
            &codeunit_name,
            &path,
            &test_procs,
            &proc_lookup,
            &mut coverage,
            &mut covered_proc_names,
            &mut cursor,
        );
    }

    // Step 3: untested public (non-local, non-test) procedures
    let untested: Vec<UntestedProcedure> = all_procs
        .iter()
        .filter(|p| {
            !p.is_local && !p.is_test && !covered_proc_names.contains(&p.name.to_lowercase())
        })
        .map(|p| UntestedProcedure {
            name: p.name.clone(),
            object: p.object.clone(),
            file: p.file.clone(),
            line: p.line,
        })
        .collect();

    CoverageReport { coverage, untested }
}

// ---------------------------------------------------------------------------
// Implementation helpers
// ---------------------------------------------------------------------------

fn collect_all_procedures(workspace: &Workspace) -> Vec<ProcDef> {
    let mut result = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key().to_string_lossy().to_string();
        let text = entry.value().clone();
        let parse_result = AlParser::parse_quick(&text);
        let tree = &parse_result.tree;

        let Some(obj_info) = al_syntax::find_object_declaration(tree, &text) else {
            continue;
        };
        let object_name = obj_info.name.clone();

        let root = tree.root_node();
        let source = text.as_bytes();
        let is_test_cu =
            obj_info.kind.to_lowercase() == "codeunit" && has_test_subtype(root, source);

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

#[allow(clippy::too_many_arguments)]
fn collect_coverage_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
    codeunit_name: &str,
    _file: &str,
    test_procs: &[crate::queries::tests::TestProcedure],
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
    coverage: &mut Vec<TestCoverageEntry>,
    covered_names: &mut HashSet<String>,
    _cursor: &mut tree_sitter::TreeCursor,
) {
    // Build a set of test proc names for fast lookup
    let test_names: HashSet<String> = test_procs.iter().map(|p| p.name.to_lowercase()).collect();

    // Walk the tree: when we find a procedure_declaration whose name is a test,
    // collect all identifier calls inside its body.
    // Iterative TreeCursor walk — owns its own cursor.
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
                            let called = collect_called_identifiers(node, source, proc_lookup);
                            for c in &called {
                                covered_names.insert(c.name.to_lowercase());
                            }
                            coverage.push(TestCoverageEntry {
                                codeunit: codeunit_name.to_string(),
                                test_procedure: proc_name.to_string(),
                                covers: called,
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

/// Collect all identifiers in the node's subtree that match known production procedure names.
fn collect_called_identifiers(
    node: tree_sitter::Node,
    source: &[u8],
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
) -> Vec<CoveredProcedure> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    collect_identifiers_recursive(node, source, proc_lookup, &mut seen, &mut result);
    result
}

fn collect_identifiers_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    proc_lookup: &HashMap<String, Vec<&ProcDef>>,
    seen: &mut HashSet<String>,
    result: &mut Vec<CoveredProcedure>,
) {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            // Look for function call patterns: identifier followed by argument_list
            // In AL tree-sitter: method_call / function_call / invocation_expression
            let kind = node.kind();
            if kind == "method_call"
                || kind == "function_call"
                || kind == "invocation_expression"
                || kind == "call_expression"
            {
                // Find the callee identifier
                if let Some(callee) = find_callee_name(node, source) {
                    let key = callee.to_lowercase();
                    if !seen.contains(&key) {
                        if let Some(defs) = proc_lookup.get(&key) {
                            // Only include non-test, non-local procedures
                            for def in defs {
                                if !def.is_test {
                                    seen.insert(key.clone());
                                    result.push(CoveredProcedure {
                                        name: def.name.clone(),
                                        object: def.object.clone(),
                                        file: def.file.clone(),
                                        line: def.line,
                                    });
                                    break;
                                }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_workspace_returns_empty_report() {
        let workspace = crate::workspace::Workspace::new();
        let report = test_coverage(&workspace);
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
}
