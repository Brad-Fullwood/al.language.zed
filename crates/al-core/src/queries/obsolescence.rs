//! Obsolescence timeline query.
//!
//! Finds all symbols marked with ObsoleteState attribute and reports their
//! projected removal timeline and caller count.

use serde::Serialize;

use crate::workspace::Workspace;
use al_syntax::AlParser;

/// ObsoleteState value from the attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObsoleteState {
    Pending,
    Removed,
    Unknown,
}

/// A symbol marked as obsolete.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsoleteEntry {
    /// Object name containing the symbol.
    pub object: String,
    /// Symbol name (procedure, field, object, etc.).
    pub symbol: String,
    /// Kind of symbol: "object", "procedure", "field".
    pub kind: String,
    /// Obsolescence state.
    pub state: ObsoleteState,
    /// The ObsoleteReason text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The ObsoleteTag (version or date).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// File path (if in workspace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Number of callers/references found in the workspace.
    pub caller_count: u32,
}

/// Find all obsolete symbols in the workspace files.
pub fn obsolescence_timeline(workspace: &Workspace) -> Vec<ObsoleteEntry> {
    let mut results = Vec::new();

    // Pre-parse all workspace files
    let parsed: Vec<(String, String, tree_sitter::Tree)> = workspace
        .file_index
        .files
        .iter()
        .map(|e| {
            let path = e.key().to_string_lossy().to_string();
            let text = e.value().clone();
            let result = AlParser::parse_quick(&text);
            (path, text, result.tree)
        })
        .collect();

    let all: Vec<(&str, &str, &tree_sitter::Tree)> = parsed
        .iter()
        .map(|(p, t, tree)| (p.as_str(), t.as_str(), tree))
        .collect();

    for (file_path, file_text, file_tree) in &all {
        scan_file_for_obsolete(file_path, file_text, file_tree, &all, &mut results);
    }

    // Also scan symbol packages for obsolete entries
    let symbols = workspace.symbols.search("", usize::MAX);
    for sym in &symbols {
        for method in &sym.methods {
            for attr in &method.attributes {
                if attr.name.eq_ignore_ascii_case("Obsolete") {
                    let reason = attr.arguments.first().cloned().map(|s| s.trim_matches('\'').trim_matches('"').to_string());
                    let tag = attr.arguments.get(1).cloned().map(|s| s.trim_matches('\'').trim_matches('"').to_string());
                    results.push(ObsoleteEntry {
                        object: sym.name.clone(),
                        symbol: method.name.clone(),
                        kind: "procedure".to_string(),
                        state: ObsoleteState::Pending,
                        reason,
                        tag,
                        file: None,
                        line: None,
                        caller_count: 0,
                    });
                }
            }
        }
    }

    results
}

fn scan_file_for_obsolete(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    all_files: &[(&str, &str, &tree_sitter::Tree)],
    results: &mut Vec<ObsoleteEntry>,
) {
    let Some(obj_info) = al_syntax::find_object_declaration(file_tree, file_text) else {
        return;
    };

    let root = file_tree.root_node();
    let source = file_text.as_bytes();

    // Check for object-level Obsolete attribute
    let obj_obsolete = extract_obsolete_from_preceding_attr(root, source);
    if let Some((state, reason, tag)) = obj_obsolete {
        let caller_count = count_references_in_files(all_files, &obj_info.name);
        results.push(ObsoleteEntry {
            object: obj_info.name.clone(),
            symbol: obj_info.name.clone(),
            kind: "object".to_string(),
            state,
            reason,
            tag,
            file: Some(file_path.to_string()),
            line: Some(1),
            caller_count,
        });
    }

    // Check for procedure-level Obsolete attributes
    scan_procedures_for_obsolete(file_path, file_text, root, source, &obj_info.name, all_files, results);
}

fn scan_procedures_for_obsolete(
    file_path: &str,
    _file_text: &str,
    node: tree_sitter::Node,
    source: &[u8],
    object_name: &str,
    all_files: &[(&str, &str, &tree_sitter::Tree)],
    results: &mut Vec<ObsoleteEntry>,
) {
    if matches!(node.kind(), "procedure_declaration" | "event_procedure_declaration") {
        if let Some(obs) = extract_obsolete_from_preceding_attr(node, source) {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("(unknown)")
                .trim_matches('"')
                .to_string();

            let line = node.start_position().row as u32 + 1;
            let caller_count = count_references_in_files(all_files, &name);

            results.push(ObsoleteEntry {
                object: object_name.to_string(),
                symbol: name,
                kind: "procedure".to_string(),
                state: obs.0,
                reason: obs.1,
                tag: obs.2,
                file: Some(file_path.to_string()),
                line: Some(line),
                caller_count,
            });
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_procedures_for_obsolete(file_path, _file_text, child, source, object_name, all_files, results);
    }
}

/// Extract ObsoleteState/ObsoleteReason/ObsoleteTag from attributes preceding a node.
/// Returns (state, reason, tag) if any Obsolete attribute is found.
fn extract_obsolete_from_preceding_attr(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<(ObsoleteState, Option<String>, Option<String>)> {
    // Check preceding siblings for attribute nodes
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" || s.kind() == "attribute_list" {
            if let Ok(text) = s.utf8_text(source) {
                if let Some(result) = parse_obsolete_attr(text) {
                    return Some(result);
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            if let Ok(text) = child.utf8_text(source) {
                if let Some(result) = parse_obsolete_attr(text) {
                    return Some(result);
                }
            }
        }
    }
    None
}

fn parse_obsolete_attr(text: &str) -> Option<(ObsoleteState, Option<String>, Option<String>)> {
    let lower = text.to_lowercase();

    // Check for ObsoleteState property in text
    if lower.contains("obsoletestate") {
        let state = if lower.contains("pending") {
            ObsoleteState::Pending
        } else if lower.contains("removed") {
            ObsoleteState::Removed
        } else {
            ObsoleteState::Unknown
        };

        let reason = extract_property_value(text, "ObsoleteReason");
        let tag = extract_property_value(text, "ObsoleteTag");
        return Some((state, reason, tag));
    }

    // Also check for [Obsolete('reason', 'tag')] attribute form
    if lower.trim_start().starts_with("[obsolete") || lower.contains("obsolete(") {
        let reason = extract_attr_arg(text, 0);
        let tag = extract_attr_arg(text, 1);
        return Some((ObsoleteState::Pending, reason, tag));
    }

    None
}

fn extract_property_value(text: &str, prop_name: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let prop_lower = prop_name.to_lowercase();
    if let Some(pos) = lower.find(&prop_lower) {
        let after = &text[pos + prop_lower.len()..];
        // Skip "= " and extract quoted value
        let after = after.trim_start_matches(|c: char| c == ' ' || c == '=' || c == ':');
        let after = after.trim_start_matches('\'').trim_start_matches('"');
        let end = after.find(|c: char| c == '\'' || c == '"' || c == ';' || c == '\n')
            .unwrap_or(after.len().min(200));
        let val = after[..end].trim().to_string();
        if !val.is_empty() { return Some(val); }
    }
    None
}

fn extract_attr_arg(text: &str, idx: usize) -> Option<String> {
    // Find content between ( and )
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    if start >= end { return None; }
    let inner = &text[start+1..end];

    // Split by comma, respecting quotes
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_q = false;
    let mut qc = '\'';
    for ch in inner.chars() {
        match ch {
            '\'' | '"' if !in_q => { in_q = true; qc = ch; }
            c if c == qc && in_q => { in_q = false; }
            ',' if !in_q => { args.push(current.trim().to_string()); current = String::new(); }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() { args.push(current.trim().to_string()); }

    args.get(idx).map(|s| s.trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
}

fn count_references_in_files(
    all_files: &[(&str, &str, &tree_sitter::Tree)],
    name: &str,
) -> u32 {
    all_files
        .iter()
        .map(|(_, text, tree)| al_syntax::find_call_references(tree, text, name) as u32)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index.add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn finds_obsolete_procedure() {
        let ws = workspace_with(vec![(
            "/src/Legacy.al",
            r#"codeunit 50100 "Legacy"
{
    [Obsolete('Use NewProc instead', '24.0')]
    procedure OldProc()
    begin
    end;

    procedure NewProc()
    begin
    end;
}"#,
        )]);

        let entries = obsolescence_timeline(&ws);
        assert!(
            entries.iter().any(|e| e.symbol == "OldProc"),
            "Should find OldProc as obsolete: {:?}", entries
        );
    }

    #[test]
    fn non_obsolete_not_reported() {
        let ws = workspace_with(vec![(
            "/src/Current.al",
            r#"codeunit 50100 "Current"
{
    procedure ActiveProc()
    begin
    end;
}"#,
        )]);

        let entries = obsolescence_timeline(&ws);
        assert!(
            !entries.iter().any(|e| e.symbol == "ActiveProc"),
            "ActiveProc should not be flagged: {:?}", entries
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let entries = obsolescence_timeline(&ws);
        assert!(entries.is_empty());
    }
}
