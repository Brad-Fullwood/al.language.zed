//! Test discovery query — `al tests`.
//!
//! Uses tree-sitter static analysis to find [Test] codeunits and [Test] procedures
//! in AL source files. No runtime connection to BC required.

use serde::Serialize;

use crate::workspace::Workspace;

/// A discovered AL test procedure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProcedure {
    pub name: String,
    pub line: u32,
}

/// A discovered AL test codeunit.
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

        let Some(obj_info) = crate::syntax::find_object_declaration(&tree, &text) else {
            continue;
        };
        if obj_info.kind.to_lowercase() != "codeunit" {
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
    /// The test codeunit's object ID.
    pub codeunit_id: i32,
    /// The test codeunit's display name.
    pub codeunit_name: String,
    /// The test method name.
    pub method_name: String,
    /// File of the test procedure.
    pub file: String,
    /// 1-based line number of the procedure declaration.
    pub line: u32,
}

/// Return tests whose source file appears in `changed_paths`.
///
/// Phase 2 simplification: a test is "affected" iff its own file is
/// in the changed list. Phase 3 will deepen this to walk the
/// CallGraph backwards from each changed procedure to its test
/// callers (`callers_of` is already cheap on the existing graph).
pub fn affected_tests(workspace: &Workspace, changed_paths: &[String]) -> Vec<AffectedTest> {
    if changed_paths.is_empty() {
        return Vec::new();
    }
    // Normalise both sides to absolute path strings for comparison.
    let normalised_changed: std::collections::HashSet<String> = changed_paths
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
pub fn has_test_subtype(root: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = root.walk();
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if node.kind() == "property" || node.kind() == "property_assignment" {
                if let Ok(text) = node.utf8_text(source) {
                    let lower = text.to_lowercase();
                    if lower.contains("subtype") && lower.contains("test") {
                        return true;
                    }
                }
            }
        }
        #[allow(clippy::if_same_then_else)]
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
        } else if cursor.goto_next_sibling() {
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

/// Collect all procedures with a [Test] attribute.
pub fn collect_test_procedures(root: tree_sitter::Node, source: &[u8]) -> Vec<TestProcedure> {
    let mut procs = Vec::new();
    collect_test_procs_iterative(root, source, &mut procs);
    procs
}

/// Iterative tree-walk (despite the historical `_recursive` name, retained
/// elsewhere in this crate's history): uses `tree_sitter::TreeCursor`
/// goto_first_child / goto_next_sibling / goto_parent. No self-recursion,
/// no Vec stack needed because the cursor IS the stack. Renamed in T065
/// to reflect the actual shape so a CLAUDE.md "no recursive tree-sitter"
/// audit can pass on a grep without manually inspecting the body.
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

/// Check if a procedure has a [Test] attribute (child or sibling).
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
    // Fallback: prev siblings
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
    use crate::syntax::AlParser;

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
        let workspace = crate::workspace::Workspace::new();
        let results = discover_tests(&workspace);
        assert!(results.is_empty());
    }
}

#[cfg(test)]
mod adversarial_j_tests {
    use super::*;
    use crate::syntax::AlParser;

    /// Finding adversarial_j_3: has_test_subtype must NOT fire on a codeunit that has
    /// Subtype = Normal even if the text of the property node contains the word "test"
    /// in a different context (e.g. a second property line).
    ///
    /// This test will PASS because the grammar creates separate property nodes — the
    /// "test" word appears in a comment/separate property, not the Subtype value.
    /// If the grammar ever groups them into one node, this documents the expected behaviour.
    #[test]
    fn test_has_test_subtype_does_not_match_subtype_normal_adversarial_j_3() {
        // A codeunit with Subtype = Normal — must NOT be treated as a test codeunit.
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

    /// Finding adversarial_j_3: has_test_subtype substring match — verify that a
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
