//! Code completion query.

use url::Url;

use super::Position;
use crate::resolution;
use crate::workspace::Workspace;

/// A completion item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    #[serde(rename = "insertText")]
    pub insert_text: Option<String>,
    /// LSP InsertTextFormat: 1 = PlainText, 2 = Snippet.
    #[serde(rename = "insertTextFormat", skip_serializing_if = "Option::is_none")]
    pub insert_text_format: Option<u8>,
    #[serde(rename = "sortText")]
    pub sort_text: Option<String>,
}

/// Completion item kinds (transport-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Snippet,
    Field,
    Property,
    Method,
    Function,
    Variable,
    Class,
    Module,
    Enum,
    EnumMember,
    Value,
    Text,
    Struct,
    Reference,
}

impl serde::Serialize for CompletionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let n: u32 = match self {
            Self::Text => 1,
            Self::Method => 2,
            Self::Function => 3,
            Self::Field => 5,
            Self::Variable => 6,
            Self::Class => 7,
            Self::Module => 9,
            Self::Property => 10,
            Self::Value => 12,
            Self::Enum => 13,
            Self::Keyword => 14,
            Self::Snippet => 15,
            Self::Reference => 18,
            Self::EnumMember => 20,
            Self::Struct => 22,
        };
        serializer.serialize_u32(n)
    }
}


use al_syntax::context::{CompletionContext, detect_context};

/// Return `true` if the cursor position is inside a comment or string node in the parse tree.
///
/// Checks both the node at the cursor position and all of its ancestors, so that a
/// cursor landing on a child node of a string/comment (e.g. an escape sequence) is
/// also detected.
fn is_inside_comment_or_string(tree: &tree_sitter::Tree, text: &str, lsp_pos: tower_lsp::lsp_types::Position) -> bool {
    // Convert the LSP UTF-16 character offset to a byte column for tree-sitter.
    let line_str = al_syntax::get_source_line(text.as_bytes(), lsp_pos.line as usize);
    let byte_col = crate::resolution::utf16_col_to_byte_offset(line_str, lsp_pos.character as usize);
    let point = tree_sitter::Point {
        row: lsp_pos.line as usize,
        column: byte_col,
    };
    let Some(node) = tree.root_node().descendant_for_point_range(point, point) else {
        return false;
    };

    const SUPPRESSED_KINDS: &[&str] = &["comment", "string", "verbatim_string"];

    // Check the node itself and walk up through ancestors.
    let mut current = Some(node);
    while let Some(n) = current {
        if SUPPRESSED_KINDS.contains(&n.kind()) {
            return true;
        }
        current = n.parent();
    }
    false
}

/// Get completions at a position in a document.
pub fn completions(workspace: &Workspace, uri: &Url, position: Position) -> Vec<CompletionEntry> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return Vec::new();
    };

    // Suppress completions inside comments and string literals.
    if let Some((_, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
        if is_inside_comment_or_string(&tree, &text, lsp_pos) {
            tracing::debug!(line = lsp_pos.line, character = lsp_pos.character, "completion: suppressed inside comment/string");
            return Vec::new();
        }
    }

    let context = detect_context(&text, lsp_pos);
    tracing::debug!(context = ?context, line = lsp_pos.line, character = lsp_pos.character, "completion: detected context");

    let mut items = Vec::new();

    match context {
        CompletionContext::MemberAccess => {
            if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, lsp_pos) {
                    if let Some(receiver) = resolution::resolve_expression_type(
                        workspace, uri, &file_text, &tree, &receiver_expr, lsp_pos,
                    ) {
                        let lsp_items = resolution::completion_items_for_receiver(workspace, &receiver);
                        items.extend(lsp_items.into_iter().map(from_lsp_completion));
                    }
                }
            }
        }
        CompletionContext::EnumAccess => {
            if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, lsp_pos) {
                    let enum_type = resolution::resolve_expression_type(
                        workspace, uri, &file_text, &tree, &receiver_expr, lsp_pos,
                    ).unwrap_or_else(|| {
                        resolution::ResolvedType {
                            type_name: receiver_expr.clone(),
                            type_subtype: Some(receiver_expr.clone()),
                        }
                    });
                    let lsp_items = resolution::enum_completion_items(workspace, &enum_type);
                    items.extend(lsp_items.into_iter().map(from_lsp_completion));
                }
            }
        }
        CompletionContext::TypePosition => {
            for kw in al_syntax::language_data::keywords().r#type.iter() {
                items.push(CompletionEntry {
                    label: kw.keyword.clone(),
                    kind: CompletionKind::Keyword,
                    detail: None, documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
                });
            }
            // Single pass over `all` — collect up to 50 per kind without 4 separate Vec clones
            let wanted_kinds = [
                al_symbols::ObjectKind::Table,
                al_symbols::ObjectKind::Enum,
                al_symbols::ObjectKind::Codeunit,
                al_symbols::ObjectKind::Interface,
            ];
            let mut counts = [0usize; 4];
            for arc in workspace.symbols.all_entries() {
                if let Some(idx) = wanted_kinds.iter().position(|&k| k == arc.kind) {
                    if counts[idx] < 50 {
                        counts[idx] += 1;
                        items.push(CompletionEntry {
                            label: format!("\"{}\"", arc.name),
                            kind: CompletionKind::Class,
                            detail: Some(format!("{} {}", arc.kind, arc.id)),
                            documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
                        });
                    }
                }
                if counts.iter().all(|&c| c >= 50) {
                    break;
                }
            }
        }
        CompletionContext::TriggerBody => {
            for var in al_syntax::language_data::implicit_variables() {
                items.push(CompletionEntry {
                    label: var.name.clone(),
                    kind: CompletionKind::Variable,
                    detail: Some(format!("{} — {}", var.r#type, var.description)),
                    documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
                });
            }
            add_default_completions(workspace, uri, lsp_pos, &mut items);
        }
        CompletionContext::ObjectBody => {
            // At object body level only structural keywords make sense.
            // Variables, builtins, and procedures are only valid inside a begin/end body.
            // Filter control keywords to the structural subset relevant at object body level.
            let structural = ["procedure", "trigger", "var", "local", "internal", "protected"];
            for kw in al_syntax::language_data::keywords().control.iter() {
                if structural.contains(&kw.keyword.as_str()) {
                    items.push(CompletionEntry {
                        label: kw.keyword.clone(),
                        kind: CompletionKind::Keyword,
                        detail: None, documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
                    });
                }
            }
        }
        CompletionContext::Default => {
            add_default_completions(workspace, uri, lsp_pos, &mut items);
        }
    }

    if !items.is_empty() {
        finalize_completion_items(&mut items);
    }
    items
}

/// Full completions: native resolution first, then .NET CodeAnalysis bridge for member access.
///
/// This is the single code path for all entry points (LSP and daemon).
/// SemanticBridge already enforces a 30s internal timeout — no outer wrapper needed.
pub async fn completions_full(workspace: &Workspace, uri: &Url, position: Position) -> Vec<CompletionEntry> {
    let items = completions(workspace, uri, position);
    if !items.is_empty() {
        return items;
    }

    // Bridge fallback: only for member access context
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some(text) = workspace.documents.get_text(uri) else { return items; };
    let ctx = al_syntax::context::detect_context(&text, lsp_pos);
    if !matches!(ctx, al_syntax::context::CompletionContext::MemberAccess) {
        return items;
    }

    let Some(guard) = crate::semantic::get_or_init_bridge(workspace).await else { return items; };
    let Some(bridge) = guard.as_ref() else { return items; };
    let Ok(path) = uri.to_file_path() else { return items; };
    let pos = (position.line + 1, position.character + 1);
    let bridge_items = match bridge.completions_at(&path, pos).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "completions_full: bridge error");
            return items;
        }
    };
    if bridge_items.is_empty() {
        return items;
    }

    fn completion_kind_from_str(s: &str) -> CompletionKind {
        match s {
            "Method" | "Function" => CompletionKind::Method,
            "Property" | "Field" => CompletionKind::Field,
            "Variable" => CompletionKind::Variable,
            "Enum" | "EnumMember" => CompletionKind::EnumMember,
            "Class" | "Struct" => CompletionKind::Class,
            "Module" | "Namespace" => CompletionKind::Module,
            "Keyword" => CompletionKind::Keyword,
            "Snippet" => CompletionKind::Snippet,
            _ => CompletionKind::Text,
        }
    }

    tracing::debug!(count = bridge_items.len(), "completions_full: bridge results");
    bridge_items
        .into_iter()
        .map(|item| CompletionEntry {
            sort_text: Some(format!("2_{}", item.label.to_ascii_lowercase())),
            kind: completion_kind_from_str(&item.kind),
            detail: item.detail,
            documentation: item.documentation,
            label: item.label,
            insert_text: None,
            insert_text_format: None,
        })
        .collect()
}

fn add_default_completions(
    workspace: &Workspace,
    uri: &Url,
    position: tower_lsp::lsp_types::Position,
    items: &mut Vec<CompletionEntry>,
) {
    // Control keywords (begin, end, if, for, while, procedure, trigger, var, etc.)
    for kw in al_syntax::language_data::keywords().control.iter() {
        items.push(CompletionEntry {
            label: kw.keyword.clone(),
            kind: CompletionKind::Keyword,
            detail: None, documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
        });
    }
    // Operator keywords (not, and, or, xor, div, mod)
    for kw in al_syntax::language_data::keywords().operator.iter() {
        items.push(CompletionEntry {
            label: kw.keyword.clone(),
            kind: CompletionKind::Keyword,
            detail: None, documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
        });
    }
    // Boolean literals are values, not in keyword data — add them explicitly.
    for lit in ["true", "false"] {
        items.push(CompletionEntry {
            label: lit.to_string(),
            kind: CompletionKind::Keyword,
            detail: None, documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
        });
    }

    if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
        let doc_symbols = al_syntax::extract_document_symbols(&tree, &file_text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if super::is_procedure_symbol(child.kind)
                    {
                        items.push(CompletionEntry {
                            label: child.name.clone(),
                            kind: CompletionKind::Function,
                            detail: child.detail.clone(),
                            documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
                        });
                    }
                }
            }
        }

        let resolver = al_syntax::type_resolver::TypeResolver::new(&tree, &file_text);
        let vars = resolver.variables_at(position);
        for var in &vars {
            let subtype = var.type_subtype.as_ref()
                .map(|s| format!(" \"{}\"", s))
                .unwrap_or_default();
            let label = super::scope_label(&var.scope);
            items.push(CompletionEntry {
                label: var.name.clone(),
                kind: CompletionKind::Variable,
                detail: Some(format!("{}{} ({})", var.type_name, subtype, label)),
                documentation: None,
                insert_text: None,
                insert_text_format: None,
                sort_text: Some(format!("0_{}", var.name)),
            });
        }
    }

    // O(1): uses pre-computed cache instead of a linear scan over all indexed symbols (ISSUE-162).
    let index_results = workspace.symbols.get_default_completions();
    for entry in &index_results {
        let kind = match entry.kind {
            al_symbols::ObjectKind::Table | al_symbols::ObjectKind::TableExtension => CompletionKind::Struct,
            al_symbols::ObjectKind::Codeunit => CompletionKind::Module,
            al_symbols::ObjectKind::Page | al_symbols::ObjectKind::PageExtension => CompletionKind::Class,
            al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension => CompletionKind::Enum,
            _ => CompletionKind::Reference,
        };
        items.push(CompletionEntry {
            label: entry.name.clone(),
            kind,
            detail: Some(format!("{} {}", entry.kind, entry.id)),
            documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
        });
    }

    let builtins = workspace.builtins.read().unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    for bt in builtins.iter() {
        items.push(CompletionEntry {
            label: bt.name.clone(),
            kind: CompletionKind::Class,
            detail: Some("built-in type".to_string()),
            documentation: None, insert_text: None, insert_text_format: None, sort_text: None,
        });
    }
    drop(builtins); // release read lock promptly

    // Global built-in AL functions from language data (available even without
    // the .NET semantic bridge).  Only add entries not already present from the
    // symbol index or local procedures; `finalize_completion_items` deduplicates.
    for func in al_syntax::language_data::builtin_functions() {
        items.push(CompletionEntry {
            label: func.name.clone(),
            kind: CompletionKind::Function,
            detail: Some(func.signature.clone()),
            documentation: Some(func.description.clone()),
            insert_text: None, insert_text_format: None, sort_text: None,
        });
    }
}

fn finalize_completion_items(items: &mut Vec<CompletionEntry>) {
    // Sort so that non-keyword items precede keywords before dedup, ensuring
    // a workspace procedure with the same name as a keyword is not shadowed.
    items.sort_by_key(|item| if item.kind == CompletionKind::Keyword { 1u8 } else { 0u8 });
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.to_lowercase()));

    for item in items.iter_mut() {
        let label_lower = item.label.to_lowercase();
        let is_callable = matches!(item.kind, CompletionKind::Function | CompletionKind::Method);

        // Set snippet insert text for callable items (Issue C1).
        // Only override if not already set (e.g. by bridge results that supply their own).
        if is_callable && item.insert_text.is_none() {
            item.insert_text = Some(format!("{}($1)", item.label));
            item.insert_text_format = Some(2); // LSP InsertTextFormat::Snippet
        }

        if item.sort_text.is_some() { continue; }
        if is_callable {
            let pc = item.detail.as_deref().map(|d| super::parse_detail_params(d).len()).unwrap_or(0);
            item.sort_text = Some(format!("1_{pc:02}_{label_lower}"));
        } else {
            item.sort_text = Some(format!("1_{label_lower}"));
        }
    }

    // sort_text is now populated for every item, so the fallback
    // to a redundant label lowercase comparison is unnecessary.
    items.sort_by(|a, b| {
        a.sort_text.as_deref().unwrap_or("")
            .cmp(b.sort_text.as_deref().unwrap_or(""))
    });
}

/// Convert a tower-lsp CompletionItem to our transport-agnostic type.
fn from_lsp_completion(item: tower_lsp::lsp_types::CompletionItem) -> CompletionEntry {
    let kind = match item.kind {
        Some(tower_lsp::lsp_types::CompletionItemKind::KEYWORD) => CompletionKind::Keyword,
        Some(tower_lsp::lsp_types::CompletionItemKind::SNIPPET) => CompletionKind::Snippet,
        Some(tower_lsp::lsp_types::CompletionItemKind::FIELD) => CompletionKind::Field,
        Some(tower_lsp::lsp_types::CompletionItemKind::PROPERTY) => CompletionKind::Property,
        Some(tower_lsp::lsp_types::CompletionItemKind::METHOD) => CompletionKind::Method,
        Some(tower_lsp::lsp_types::CompletionItemKind::FUNCTION) => CompletionKind::Function,
        Some(tower_lsp::lsp_types::CompletionItemKind::VARIABLE) => CompletionKind::Variable,
        Some(tower_lsp::lsp_types::CompletionItemKind::CLASS) => CompletionKind::Class,
        Some(tower_lsp::lsp_types::CompletionItemKind::MODULE) => CompletionKind::Module,
        Some(tower_lsp::lsp_types::CompletionItemKind::ENUM) => CompletionKind::Enum,
        Some(tower_lsp::lsp_types::CompletionItemKind::ENUM_MEMBER) => CompletionKind::EnumMember,
        Some(tower_lsp::lsp_types::CompletionItemKind::VALUE) => CompletionKind::Value,
        Some(tower_lsp::lsp_types::CompletionItemKind::STRUCT) => CompletionKind::Struct,
        Some(tower_lsp::lsp_types::CompletionItemKind::REFERENCE) => CompletionKind::Reference,
        _ => CompletionKind::Text,
    };
    let documentation = item.documentation.map(|d| match d {
        tower_lsp::lsp_types::Documentation::String(s) => s,
        tower_lsp::lsp_types::Documentation::MarkupContent(m) => m.value,
    });
    // Convert InsertTextFormat: PLAIN_TEXT=1, SNIPPET=2.
    let insert_text_format = item.insert_text_format.map(|f| {
        if f == tower_lsp::lsp_types::InsertTextFormat::SNIPPET { 2u8 } else { 1u8 }
    });
    CompletionEntry {
        label: item.label,
        kind,
        detail: item.detail,
        documentation,
        insert_text: item.insert_text,
        insert_text_format,
        sort_text: item.sort_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    #[test]
    fn completion_kind_serializes_to_lsp_integer() {
        assert_eq!(serde_json::to_value(CompletionKind::Function).unwrap(), 3);
        assert_eq!(serde_json::to_value(CompletionKind::Field).unwrap(), 5);
        assert_eq!(serde_json::to_value(CompletionKind::Variable).unwrap(), 6);
        assert_eq!(serde_json::to_value(CompletionKind::Class).unwrap(), 7);
        assert_eq!(serde_json::to_value(CompletionKind::Keyword).unwrap(), 14);
    }

    #[test]
    fn count_params_works() {
        // Verify that parse_detail_params (the canonical implementation) counts correctly.
        assert_eq!(super::super::parse_detail_params("()").len(), 0);
        assert_eq!(super::super::parse_detail_params("").len(), 0);
        assert_eq!(super::super::parse_detail_params("(A: Text)").len(), 1);
        assert_eq!(super::super::parse_detail_params("(A: Text; B: Integer)").len(), 2);
        assert_eq!(super::super::parse_detail_params("(A: Text; B: Integer; C: Boolean)").len(), 3);
        assert_eq!(super::super::parse_detail_params("(A: List of [Text]; B: Integer)").len(), 2);
        assert_eq!(super::super::parse_detail_params("(A: Text): Boolean").len(), 1);
    }

    // --- Failure path tests ---

    #[test]
    fn completions_empty_for_unopened_document() {
        let ws = Workspace::new();
        let uri = test_uri();
        let pos = Position { line: 0, character: 0 };
        let result = completions(&ws, &uri, pos);
        assert!(result.is_empty(), "unopened document should return empty completions");
    }

    #[test]
    fn completions_on_empty_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), String::new());
        let pos = Position { line: 0, character: 0 };
        let result = completions(&ws, &uri, pos);
        // Empty file — may return keywords but should not panic
        let _ = result;
    }

    #[test]
    fn completions_on_malformed_al() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), "{{{{not valid al code}}}}".to_string());
        let pos = Position { line: 0, character: 5 };
        let result = completions(&ws, &uri, pos);
        // Should not panic on malformed code
        let _ = result;
    }

    #[test]
    fn completions_at_line_beyond_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), "codeunit 50100 \"X\" { }".to_string());
        // Line 100 doesn't exist — should return empty, not panic
        let pos = Position { line: 100, character: 0 };
        let result = completions(&ws, &uri, pos);
        let _ = result; // just ensure no panic
    }

    #[test]
    fn completions_include_keywords_in_begin_block() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin

    end;
}"#.to_string());
        let pos = Position { line: 4, character: 8 }; // inside begin block
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"if"), "should include 'if' keyword, got: {:?}", labels);
        assert!(labels.contains(&"repeat"), "should include 'repeat' keyword");
    }

    #[test]
    fn completions_include_local_procedures() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure Helper()
    begin
    end;

    procedure Caller()
    begin

    end;
}"#.to_string());
        let pos = Position { line: 8, character: 8 };
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"Helper"), "should include local procedure 'Helper', got: {:?}", labels);
    }

    // --- C-04: global built-in functions ---

    #[test]
    fn completions_include_builtin_functions_in_begin_block() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin

    end;
}"#.to_string());
        let pos = Position { line: 4, character: 8 }; // inside begin block
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"Message"), "should include 'Message' builtin, got: {:?}", labels);
        assert!(labels.contains(&"Error"), "should include 'Error' builtin");
        assert!(labels.contains(&"Confirm"), "should include 'Confirm' builtin");
        assert!(labels.contains(&"StrSubstNo"), "should include 'StrSubstNo' builtin");
        assert!(labels.contains(&"Format"), "should include 'Format' builtin");
        // Verify they are Function kind
        let message_kind = result.iter()
            .find(|c| c.label == "Message")
            .map(|c| c.kind);
        assert_eq!(message_kind, Some(CompletionKind::Function), "'Message' should be Function kind");
    }

    #[test]
    fn completions_builtin_functions_have_signature_detail() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin

    end;
}"#.to_string());
        let pos = Position { line: 4, character: 8 };
        let result = completions(&ws, &uri, pos);
        let message = result.iter().find(|c| c.label == "Message");
        assert!(message.is_some(), "Message should appear in completions");
        let message = message.unwrap();
        assert!(message.detail.is_some(), "Message should have a detail/signature");
        assert!(message.detail.as_deref().unwrap_or("").contains("Text"),
            "Message detail should mention Text parameter");
    }

    // --- Comment/string suppression tests ---

    #[test]
    fn completions_empty_inside_line_comment() {
        let ws = Workspace::new();
        let uri = test_uri();
        // Line 2 (0-indexed) is the comment line: "    // TODO: implement properly"
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Hello World"
{
    // TODO: implement properly
    procedure DoSomething()
    begin
    end;
}"#.to_string());
        // Position 20 is inside "// TODO: implement properly"
        let pos = Position { line: 2, character: 20 };
        let result = completions(&ws, &uri, pos);
        assert!(result.is_empty(),
            "completions inside a line comment should be empty, got {} items", result.len());
    }

    #[test]
    fn completions_empty_inside_string_literal() {
        let ws = Workspace::new();
        let uri = test_uri();
        // Line 4 (0-indexed) is: "        Message('Hello World');"
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure badName()
    begin
        Message('Hello World');
    end;
}"#.to_string());
        // Position 22 is inside the string 'Hello World'
        let pos = Position { line: 4, character: 22 };
        let result = completions(&ws, &uri, pos);
        assert!(result.is_empty(),
            "completions inside a string literal should be empty, got {} items", result.len());
    }

    #[test]
    fn completions_empty_inside_verbatim_string() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        x: Text;
    begin
        x := @'some verbatim text';
    end;
}"#.to_string());
        // Position 15 is inside @'some verbatim text'
        let pos = Position { line: 6, character: 15 };
        let result = completions(&ws, &uri, pos);
        assert!(result.is_empty(),
            "completions inside a verbatim string should be empty, got {} items", result.len());
    }

    #[test]
    fn completions_non_empty_in_procedure_body_after_comment() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), r#"codeunit 50100 "Hello World"
{
    // TODO: implement properly
    procedure DoSomething()
    begin

    end;
}"#.to_string());
        // Position inside the procedure body (line 4 is empty inside begin/end)
        let pos = Position { line: 5, character: 8 };
        let result = completions(&ws, &uri, pos);
        assert!(!result.is_empty(),
            "completions after a comment but inside procedure body should return items");
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"if"), "should include 'if' keyword");
    }
}
