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

use crate::syntax::context::{detect_context, CompletionContext};

/// Get completions at a position in a document.
#[must_use]
pub fn completions(workspace: &Workspace, uri: &Url, position: Position) -> Vec<CompletionEntry> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return Vec::new();
    };
    let context = detect_context(&text, crate::syntax_lsp::lsp_pos_to_syntax(lsp_pos));
    tracing::debug!(context = ?context, line = lsp_pos.line, character = lsp_pos.character, "completion: detected context");

    let mut items = Vec::new();

    match context {
        CompletionContext::MemberAccess => {
            if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri)
            {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position)
                {
                    if let Some(receiver) = resolution::resolve_expression_type(
                        workspace,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    ) {
                        let lsp_items =
                            resolution::completion_items_for_receiver(workspace, &receiver);
                        items.extend(lsp_items.into_iter().map(from_lsp_completion));
                    }
                }
            }
        }
        CompletionContext::EnumAccess => {
            if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri)
            {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position)
                {
                    let enum_type = resolution::resolve_expression_type(
                        workspace,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    )
                    .unwrap_or_else(|| resolution::ResolvedType {
                        type_name: receiver_expr.clone(),
                        type_subtype: Some(receiver_expr.clone()),
                    });
                    let lsp_items = resolution::enum_completion_items(workspace, &enum_type);
                    items.extend(lsp_items.into_iter().map(from_lsp_completion));
                }
            }
        }
        CompletionContext::TypePosition => {
            for entry in &crate::syntax::language_data::keywords().r#type {
                items.push(CompletionEntry {
                    label: entry.keyword.clone(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
            // Use get_by_kind for each wanted kind directly. Previously this
            // was a single all_entries() pass with per-kind counters and an
            // early-exit that required ALL four counters to hit 50 — so if
            // (say) Interface had fewer than 50 entries, the loop kept
            // scanning every other entry in the index after the other three
            // were already full. The targeted lookups bound work to the
            // requested kind sets and the per-kind cap.
            const TYPE_COMPLETION_CAP: usize = 50;
            for kind in [
                crate::symbols::ObjectKind::Table,
                crate::symbols::ObjectKind::Enum,
                crate::symbols::ObjectKind::Codeunit,
                crate::symbols::ObjectKind::Interface,
            ] {
                for arc in workspace
                    .symbols
                    .get_by_kind(kind)
                    .iter()
                    .take(TYPE_COMPLETION_CAP)
                {
                    items.push(CompletionEntry {
                        label: format!("\"{}\"", arc.name),
                        kind: CompletionKind::Class,
                        detail: Some(format!("{} {}", arc.kind, arc.id)),
                        documentation: None,
                        insert_text: None,
                        sort_text: None,
                    });
                }
            }
        }
        CompletionContext::Default => {
            // Include implicit trigger variables (Rec, xRec, CurrPage, etc.) in the
            // default context. A dedicated TriggerBody detection pass would be needed
            // to offer these only inside trigger bodies, but default context is safe.
            for var in crate::syntax::language_data::implicit_variables() {
                items.push(CompletionEntry {
                    label: var.name.clone(),
                    kind: CompletionKind::Variable,
                    detail: Some(format!("{} — {}", var.r#type, var.description)),
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
            add_default_completions(workspace, uri, position, &mut items);
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
pub async fn completions_full(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Vec<CompletionEntry> {
    let items = completions(workspace, uri, position);
    if !items.is_empty() {
        return items;
    }

    // Bridge fallback: only for member access context.
    // Use get_text_arc to share the cached Arc<String> instead of deep-cloning
    // the entire file contents on every keystroke.
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return items;
    };
    let ctx = crate::syntax::context::detect_context(
        &text,
        crate::syntax_lsp::lsp_pos_to_syntax(lsp_pos),
    );
    if !matches!(ctx, crate::syntax::context::CompletionContext::MemberAccess) {
        return items;
    }

    let Some(guard) = crate::semantic::get_or_init_bridge(workspace).await else {
        return items;
    };
    let Some(bridge) = guard.as_ref() else {
        return items;
    };
    let Ok(path) = uri.to_file_path() else {
        return items;
    };
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

    tracing::debug!(
        count = bridge_items.len(),
        "completions_full: bridge results"
    );
    bridge_items
        .into_iter()
        .map(|item| CompletionEntry {
            sort_text: Some(format!("2_{}", item.label.to_ascii_lowercase())),
            kind: completion_kind_from_str(&item.kind),
            detail: item.detail,
            documentation: item.documentation,
            label: item.label,
            insert_text: None,
        })
        .collect()
}

fn add_default_completions(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    items: &mut Vec<CompletionEntry>,
) {
    let kw_data = crate::syntax::language_data::keywords();
    for entry in kw_data.control.iter().chain(kw_data.operator.iter()) {
        items.push(CompletionEntry {
            label: entry.keyword.clone(),
            kind: CompletionKind::Keyword,
            detail: None,
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }

    if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
        // Prefer cached symbols populated by the file index; fall back to a
        // fresh extraction only for documents not stored on disk. Avoids the
        // full AST walk on every keystroke.
        let file_path = uri.to_file_path().ok();
        let doc_symbols: Vec<super::AlDocumentSymbol> = file_path
            .as_ref()
            .and_then(|p| workspace.file_index.get_cached_symbols(p))
            .unwrap_or_else(|| {
                crate::syntax::extract_document_symbols(&tree, &file_text)
                    .into_iter()
                    .map(Into::into)
                    .collect()
            });
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if super::is_procedure_symbol(child.kind) {
                        items.push(CompletionEntry {
                            label: child.name.clone(),
                            kind: CompletionKind::Function,
                            detail: child.detail.clone(),
                            documentation: None,
                            insert_text: None,
                            sort_text: None,
                        });
                    }
                }
            }
        }

        let resolver = crate::syntax::type_resolver::TypeResolver::new(&tree, &file_text);
        let vars = resolver.variables_at(position.into());
        for var in &vars {
            let subtype = var
                .type_subtype
                .as_ref()
                .map(|s| format!(" \"{}\"", s))
                .unwrap_or_default();
            let label = super::scope_label(&var.scope);
            items.push(CompletionEntry {
                label: var.name.clone(),
                kind: CompletionKind::Variable,
                detail: Some(format!("{}{} ({})", var.type_name, subtype, label)),
                documentation: None,
                insert_text: None,
                sort_text: Some(format!("0_{}", var.name)),
            });
        }
    }

    // O(1): uses pre-computed cache instead of a linear scan over all indexed symbols (ISSUE-162).
    let index_results = workspace.symbols.get_default_completions();
    for entry in &index_results {
        let kind = match entry.kind {
            crate::symbols::ObjectKind::Table | crate::symbols::ObjectKind::TableExtension => {
                CompletionKind::Struct
            }
            crate::symbols::ObjectKind::Codeunit => CompletionKind::Module,
            crate::symbols::ObjectKind::Page | crate::symbols::ObjectKind::PageExtension => {
                CompletionKind::Class
            }
            crate::symbols::ObjectKind::Enum | crate::symbols::ObjectKind::EnumExtension => {
                CompletionKind::Enum
            }
            _ => CompletionKind::Reference,
        };
        items.push(CompletionEntry {
            label: entry.name.clone(),
            kind,
            detail: Some(format!("{} {}", entry.kind, entry.id)),
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }

    let builtins = workspace.builtins.read().unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    for bt in builtins.iter() {
        items.push(CompletionEntry {
            label: bt.name.clone(),
            kind: CompletionKind::Class,
            detail: Some("built-in type".to_string()),
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }
    drop(builtins); // release read lock promptly
}

fn finalize_completion_items(items: &mut Vec<CompletionEntry>) {
    // Sort so that non-keyword items precede keywords before dedup, ensuring
    // a workspace procedure with the same name as a keyword is not shadowed.
    items.sort_by_key(|item| {
        if item.kind == CompletionKind::Keyword {
            1u8
        } else {
            0u8
        }
    });
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.to_lowercase()));

    for item in items.iter_mut() {
        if item.sort_text.is_some() {
            continue;
        }
        let label_lower = item.label.to_lowercase();
        let is_callable = matches!(item.kind, CompletionKind::Function | CompletionKind::Method);
        if is_callable {
            let pc = item
                .detail
                .as_deref()
                .map(|d| super::parse_detail_params(d).len())
                .unwrap_or(0);
            item.sort_text = Some(format!("1_{pc:02}_{label_lower}"));
        } else {
            item.sort_text = Some(format!("1_{label_lower}"));
        }
    }

    // sort_text is now populated for every item, so the fallback
    // to a redundant label lowercase comparison is unnecessary.
    items.sort_by(|a, b| {
        a.sort_text
            .as_deref()
            .unwrap_or("")
            .cmp(b.sort_text.as_deref().unwrap_or(""))
    });
}

/// Convert a tower-lsp CompletionItem to our transport-agnostic type.
fn from_lsp_completion(item: resolution::CompletionCandidate) -> CompletionEntry {
    let kind = match item.kind {
        resolution::CompletionCandidateKind::Variable => CompletionKind::Variable,
        resolution::CompletionCandidateKind::Method => CompletionKind::Method,
        resolution::CompletionCandidateKind::Field => CompletionKind::Field,
        resolution::CompletionCandidateKind::EnumMember => CompletionKind::EnumMember,
    };
    CompletionEntry {
        label: item.label,
        kind,
        detail: item.detail,
        documentation: item.documentation,
        insert_text: item.insert_text,
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
    fn add_default_completions_signature_is_transport_agnostic() {
        // Compile-time guard against re-introducing the lsp_types boundary leak
        // fixed by T015 / arch-001. If anyone widens the parameter back to
        // tower_lsp::lsp_types::Position the function pointer coercion below
        // will fail to type-check.
        let _: fn(&Workspace, &Url, Position, &mut Vec<CompletionEntry>) = add_default_completions;
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
        assert_eq!(
            super::super::parse_detail_params("(A: Text; B: Integer)").len(),
            2
        );
        assert_eq!(
            super::super::parse_detail_params("(A: Text; B: Integer; C: Boolean)").len(),
            3
        );
        assert_eq!(
            super::super::parse_detail_params("(A: List of [Text]; B: Integer)").len(),
            2
        );
        assert_eq!(
            super::super::parse_detail_params("(A: Text): Boolean").len(),
            1
        );
    }

    // --- Failure path tests ---

    #[test]
    fn completions_empty_for_unopened_document() {
        let ws = Workspace::new();
        let uri = test_uri();
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        assert!(
            result.is_empty(),
            "unopened document should return empty completions"
        );
    }

    #[test]
    fn completions_on_empty_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), String::new());
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        // Empty file — may return keywords but should not panic
        let _ = result;
    }

    #[test]
    fn completions_on_malformed_al() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(uri.clone(), "{{{{not valid al code}}}}".to_string());
        let pos = Position {
            line: 0,
            character: 5,
        };
        let result = completions(&ws, &uri, pos);
        // Should not panic on malformed code
        let _ = result;
    }

    #[test]
    fn completions_at_line_beyond_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(uri.clone(), "codeunit 50100 \"X\" { }".to_string());
        // Line 100 doesn't exist — should return empty, not panic
        let pos = Position {
            line: 100,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        let _ = result; // just ensure no panic
    }

    #[test]
    fn completions_include_keywords_in_begin_block() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin

    end;
}"#
            .to_string(),
        );
        let pos = Position {
            line: 4,
            character: 8,
        }; // inside begin block
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"if"),
            "should include 'if' keyword, got: {:?}",
            labels
        );
        assert!(
            labels.contains(&"repeat"),
            "should include 'repeat' keyword"
        );
    }

    #[test]
    fn completions_include_local_procedures() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(
            uri.clone(),
            r#"codeunit 50100 "Test"
{
    procedure Helper()
    begin
    end;

    procedure Caller()
    begin

    end;
}"#
            .to_string(),
        );
        let pos = Position {
            line: 8,
            character: 8,
        };
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"Helper"),
            "should include local procedure 'Helper', got: {:?}",
            labels
        );
    }
}
