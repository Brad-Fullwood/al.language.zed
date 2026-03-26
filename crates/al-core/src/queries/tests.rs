//! Test discovery query — `al tests`.
//!
//! Uses tree-sitter static analysis to find [Test] codeunits and [Test] procedures
//! in AL source files. No runtime connection to BC required.

use al_syntax::AlParser;
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
        let path = entry.key().to_string_lossy().to_string();
        let text = entry.value().clone();
        let parse_result = AlParser::parse_quick(&text);
        let tree = &parse_result.tree;
        let source = text.as_bytes();

        let Some(obj_info) = al_syntax::find_object_declaration(tree, &text) else {
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

/// Collect all procedures with a [Test] attribute.
pub fn collect_test_procedures(root: tree_sitter::Node, source: &[u8]) -> Vec<TestProcedure> {
    let mut procs = Vec::new();
    collect_test_procs_recursive(root, source, &mut procs);
    procs
}

fn collect_test_procs_recursive(
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
