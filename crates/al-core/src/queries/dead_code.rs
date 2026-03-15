//! Dead code detection query.
//!
//! Cross-file analysis to find unused procedures, unreferenced table fields,
//! and orphaned event subscribers. Returns a list of `UnusedSymbol` entries
//! with the reason each symbol is considered dead.

use al_syntax::AlParser;
use serde::Serialize;

use crate::workspace::Workspace;

/// The kind of unused symbol found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedKind {
    Procedure,
    Field,
    Subscriber,
}

/// The reason a symbol is considered unused/dead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedReason {
    /// No references found across any workspace files.
    ZeroReferences,
    /// Event subscriber targets a publisher that no longer exists.
    PublisherRemoved,
}

/// A single unused symbol found by dead code analysis.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedSymbol {
    /// Kind of the unused symbol.
    #[serde(rename = "k")]
    pub kind: UnusedKind,
    /// Name of the unused symbol.
    #[serde(rename = "n")]
    pub name: String,
    /// Object that contains this symbol.
    #[serde(rename = "obj")]
    pub object: String,
    /// File path where the symbol is defined (if in workspace).
    #[serde(rename = "f", skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number (1-based) where the symbol is defined.
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Why this symbol is considered dead.
    pub reason: UnusedReason,
}

/// Find all unused symbols in the workspace.
///
/// Scans workspace .al files for:
/// 1. Procedures that are never called from any other file
/// 2. Table fields that are never referenced in any page/report/codeunit
/// 3. Event subscribers whose target publisher no longer exists in the symbol index
pub fn dead_code(workspace: &Workspace) -> Vec<UnusedSymbol> {
    let mut results = Vec::new();

    // Pre-parse all workspace files
    let parsed_files: Vec<(String, String, tree_sitter::Tree)> = workspace
        .file_index
        .files
        .iter()
        .map(|entry| {
            let path = entry.key().to_string_lossy().to_string();
            let text = entry.value().clone();
            let result = AlParser::parse_quick(&text);
            (path, text, result.tree)
        })
        .collect();

    // Collect all text+tree pairs for reference scanning
    let all_files: Vec<(&str, &str, &tree_sitter::Tree)> = parsed_files
        .iter()
        .map(|(p, t, tree)| (p.as_str(), t.as_str(), tree))
        .collect();

    for (file_path, file_text, file_tree) in &all_files {
        let Some(obj_info) = al_syntax::find_object_declaration(file_tree, file_text) else {
            continue;
        };

        // 1. Find unused procedures
        find_unused_procedures(
            file_path,
            file_text,
            file_tree,
            &obj_info.name,
            &all_files,
            &mut results,
        );

        // 2. Find unused table fields (only for table objects)
        let obj_kind_lower = obj_info.kind.to_lowercase();
        if obj_kind_lower == "table" {
            find_unused_fields(
                file_path,
                file_text,
                file_tree,
                &obj_info.name,
                &all_files,
                &mut results,
            );
        }

        // 3. Find orphaned subscribers
        find_orphaned_subscribers(
            file_path,
            file_text,
            file_tree,
            &obj_info.name,
            workspace,
            &mut results,
        );
    }

    results
}

/// Extract procedure declarations from a file and check if they're referenced elsewhere.
fn find_unused_procedures(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    object_name: &str,
    all_files: &[(&str, &str, &tree_sitter::Tree)],
    results: &mut Vec<UnusedSymbol>,
) {
    let root = file_tree.root_node();
    let source = file_text.as_bytes();
    let mut procs = Vec::new();

    collect_procedures(root, source, &mut procs);

    for (proc_name, is_event, line) in &procs {
        // Skip event publishers — they're entry points
        if *is_event {
            continue;
        }

        // Check if this procedure name appears in ANY file (including its own)
        let referenced = all_files.iter().any(|(other_path, other_text, other_tree)| {
            // For the defining file, check if the name is referenced beyond its declaration
            let refs = al_syntax::find_variable_references(other_tree, other_text, proc_name);
            if *other_path == file_path {
                // In the same file, references beyond the declaration count
                // (a procedure that's only mentioned once — at its declaration — is unused)
                refs.len() > 1
            } else {
                !refs.is_empty()
            }
        });

        if !referenced {
            results.push(UnusedSymbol {
                kind: UnusedKind::Procedure,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
            });
        }
    }
}

/// Recursively collect procedure declarations: (name, is_event_publisher, line_1based).
fn collect_procedures(
    node: tree_sitter::Node,
    source: &[u8],
    procs: &mut Vec<(String, bool, u32)>,
) {
    if matches!(
        node.kind(),
        "procedure_declaration" | "event_procedure_declaration"
    ) {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source) {
                let name = name.trim_matches('"').to_string();
                let line = node.start_position().row as u32 + 1;

                // Check for IntegrationEvent or BusinessEvent attribute
                let is_event = has_event_attribute(node, source);

                procs.push((name, is_event, line));
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_procedures(child, source, procs);
    }
}

/// Check if a procedure node has an IntegrationEvent or BusinessEvent attribute.
fn has_event_attribute(node: tree_sitter::Node, source: &[u8]) -> bool {
    // Look for attribute nodes as siblings before this node or as children
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" || s.kind() == "attribute_list" {
            if let Ok(text) = s.utf8_text(source) {
                let t = text.to_lowercase();
                if t.contains("integrationevent") || t.contains("businessevent") {
                    return true;
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check children (some grammars nest attributes inside the procedure node)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            if let Ok(text) = child.utf8_text(source) {
                let t = text.to_lowercase();
                if t.contains("integrationevent") || t.contains("businessevent") {
                    return true;
                }
            }
        }
    }

    false
}

/// Extract table field declarations and check if they're referenced in other files.
fn find_unused_fields(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    object_name: &str,
    all_files: &[(&str, &str, &tree_sitter::Tree)],
    results: &mut Vec<UnusedSymbol>,
) {
    let root = file_tree.root_node();
    let source = file_text.as_bytes();
    let mut fields = Vec::new();

    collect_fields(root, source, &mut fields);

    for (field_name, line) in &fields {
        // Check if this field name appears in any OTHER file
        let referenced = all_files.iter().any(|(other_path, other_text, other_tree)| {
            if *other_path == file_path {
                return false; // The defining file doesn't count
            }
            let refs = al_syntax::find_variable_references(other_tree, other_text, field_name);
            !refs.is_empty()
        });

        if !referenced {
            results.push(UnusedSymbol {
                kind: UnusedKind::Field,
                name: field_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
            });
        }
    }
}

/// Recursively collect field declarations: (name, line_1based).
fn collect_fields(node: tree_sitter::Node, source: &[u8], fields: &mut Vec<(String, u32)>) {
    if node.kind() == "field_declaration" {
        // Field name is the "name" field or the second meaningful child
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source) {
                let name = name.trim_matches('"').to_string();
                if !name.is_empty() {
                    fields.push((name, node.start_position().row as u32 + 1));
                }
            }
        } else {
            // Fallback: look for quoted_identifier or string child
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "quoted_identifier" | "string") {
                    if let Ok(name) = child.utf8_text(source) {
                        let name = name.trim_matches('"').trim_matches('\'').to_string();
                        if !name.is_empty() {
                            fields.push((name, node.start_position().row as u32 + 1));
                            break;
                        }
                    }
                }
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fields(child, source, fields);
    }
}

/// Find event subscribers whose target publisher doesn't exist in the symbol index.
fn find_orphaned_subscribers(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    object_name: &str,
    workspace: &Workspace,
    results: &mut Vec<UnusedSymbol>,
) {
    let root = file_tree.root_node();
    let source = file_text.as_bytes();
    let mut subscribers = Vec::new();

    collect_event_subscribers(root, source, &mut subscribers);

    for (proc_name, target_object, _target_event, line) in &subscribers {
        // Check if the target object exists in the symbol index OR in workspace files
        let target_lower = target_object.to_lowercase();
        let exists_in_symbols = !workspace.symbols.search(&target_lower, 1).is_empty();
        let exists_in_workspace = workspace
            .file_index
            .find_by_object_name(&target_lower)
            .is_some();

        if !exists_in_symbols && !exists_in_workspace {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::PublisherRemoved,
            });
        }
    }
}

/// Collect event subscriber procedures: (proc_name, target_object, target_event, line_1based).
fn collect_event_subscribers(
    node: tree_sitter::Node,
    source: &[u8],
    subscribers: &mut Vec<(String, String, String, u32)>,
) {
    if matches!(
        node.kind(),
        "procedure_declaration" | "event_procedure_declaration"
    ) {
        // Check for EventSubscriber attribute
        let attr_text = get_preceding_attribute(node, source);
        if let Some(ref text) = attr_text {
            if text.to_lowercase().contains("eventsubscriber") {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(proc_name) = name_node.utf8_text(source) {
                        let proc_name = proc_name.trim_matches('"').to_string();
                        let (target_object, target_event) = parse_subscriber_args(text);
                        let line = node.start_position().row as u32 + 1;
                        subscribers.push((proc_name, target_object, target_event, line));
                    }
                }
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_event_subscribers(child, source, subscribers);
    }
}

/// Get the attribute text preceding a procedure node.
fn get_preceding_attribute(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" || s.kind() == "attribute_list" {
            return s.utf8_text(source).ok().map(|s| s.to_string());
        }
        if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check first child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Parse the target object and event from an EventSubscriber attribute text.
/// Format: [EventSubscriber(ObjectType::Codeunit, Codeunit::"Name", 'Event', ...)]
fn parse_subscriber_args(attr_text: &str) -> (String, String) {
    // Find content between parentheses
    let inner = attr_text
        .find('(')
        .and_then(|start| attr_text.rfind(')').map(|end| &attr_text[start + 1..end]))
        .unwrap_or("");

    // Split by commas, respecting quoted strings
    let args = split_args(inner);

    // arg[1] is the target object (e.g., Codeunit::"Sales-Post" or "Sales-Post")
    let target_object = args
        .get(1)
        .map(|s| {
            let s = s.trim();
            // Remove Type:: prefix
            let s = if let Some(pos) = s.find("::") {
                &s[pos + 2..]
            } else {
                s
            };
            s.trim_matches('"').trim_matches('\'').to_string()
        })
        .unwrap_or_default();

    // arg[2] is the target event
    let target_event = args
        .get(2)
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .unwrap_or_default();

    (target_object, target_event)
}

/// Split attribute arguments by comma, respecting quoted strings.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '"';

    for ch in s.chars() {
        match ch {
            '"' | '\'' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
                current.push(ch);
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
                current.push(ch);
            }
            ',' if !in_quotes => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_index::FileIndex;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    /// Helper: create a workspace with specific .al files in the file index.
    fn workspace_with_files(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index.add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    #[ignore = "dead code detection not fully implemented yet — Beyond v1 feature"]
    fn unused_procedure_detected() {
        // Codeunit with a procedure that is never called from anywhere
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50100 "My Codeunit"
{
    procedure UsedProc()
    begin
    end;

    procedure UnusedHelper()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50101 "Caller"
{
    procedure DoWork()
    var
        cu: Codeunit "My Codeunit";
    begin
        cu.UsedProc();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        // UnusedHelper is never referenced from any other file
        assert!(
            unused.iter().any(|u| u.name == "UnusedHelper"
                && u.kind == UnusedKind::Procedure
                && u.reason == UnusedReason::ZeroReferences),
            "Expected UnusedHelper to be flagged. Got: {:?}",
            unused
        );

        // UsedProc IS referenced from Caller.al — should NOT appear
        assert!(
            !unused.iter().any(|u| u.name == "UsedProc"),
            "UsedProc should not be flagged as unused"
        );
    }

    #[test]
    #[ignore = "dead code detection not fully implemented yet — Beyond v1 feature"]
    fn unused_table_field_detected() {
        // Table with a field that no page/report/codeunit references
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
        field(3; "Legacy Flag"; Boolean) { }
    }
}"#,
            ),
            (
                "/src/MyPage.al",
                r#"page 50100 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field("Name"; Rec."Name") { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        // "Legacy Flag" is never used in any page/codeunit/report
        assert!(
            unused.iter().any(|u| u.name == "Legacy Flag"
                && u.kind == UnusedKind::Field
                && u.reason == UnusedReason::ZeroReferences),
            "Expected 'Legacy Flag' field to be flagged. Got: {:?}",
            unused
        );

        // "No." and "Name" ARE referenced in MyPage.al
        assert!(
            !unused.iter().any(|u| u.name == "No." && u.kind == UnusedKind::Field),
            "'No.' should not be flagged as unused"
        );
        assert!(
            !unused.iter().any(|u| u.name == "Name" && u.kind == UnusedKind::Field),
            "'Name' should not be flagged as unused"
        );
    }

    #[test]
    fn orphaned_subscriber_detected() {
        // Subscriber targets an event on "Old Publisher" but that codeunit doesn't exist
        let ws = workspace_with_files(vec![(
            "/src/MySub.al",
            r#"codeunit 50102 "My Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Old Publisher", 'OnOldEvent', '', false, false)]
    local procedure HandleOldEvent()
    begin
    end;
}"#,
        )]);

        // No symbol entry for "Old Publisher" in the index
        let unused = dead_code(&ws);

        assert!(
            unused.iter().any(|u| u.name == "HandleOldEvent"
                && u.kind == UnusedKind::Subscriber
                && u.reason == UnusedReason::PublisherRemoved),
            "Expected orphaned subscriber to be detected. Got: {:?}",
            unused
        );
    }

    #[test]
    fn no_false_positives_for_local_calls() {
        // A procedure called only within the same file should NOT be flagged
        // (local calls are legitimate use)
        let ws = workspace_with_files(vec![(
            "/src/Internal.al",
            r#"codeunit 50103 "Internal"
{
    procedure PublicEntry()
    begin
        InternalHelper();
    end;

    local procedure InternalHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws);

        // InternalHelper is called within the same file — not dead
        assert!(
            !unused.iter().any(|u| u.name == "InternalHelper"),
            "InternalHelper is called locally, should not be flagged"
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let unused = dead_code(&ws);
        assert!(unused.is_empty());
    }

    #[test]
    fn event_publishers_not_flagged_as_unused_procedures() {
        // Integration event publishers should never be flagged — they're entry points
        let ws = workspace_with_files(vec![(
            "/src/Publisher.al",
            r#"codeunit 50104 "My Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost(var SalesHeader: Record "Sales Header")
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws);

        assert!(
            !unused.iter().any(|u| u.name == "OnAfterPost"),
            "Event publishers should not be flagged as dead code"
        );
    }
}
