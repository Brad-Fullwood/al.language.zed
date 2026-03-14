//! Code completion query.

use url::Url;

use super::Position;
use crate::resolution;
use crate::workspace::Workspace;

/// A completion item.
#[derive(Debug, Clone)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub insert_text: Option<String>,
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

/// AL keywords for general completion.
const AL_KEYWORDS: &[&str] = &[
    "begin", "end", "var", "procedure", "trigger", "local", "internal", "protected",
    "if", "then", "else", "case", "of", "for", "to", "downto", "do", "foreach", "in",
    "while", "repeat", "until", "exit", "break", "with", "asserterror",
    "true", "false", "not", "and", "or", "xor", "div", "mod",
];

/// AL type keywords for type position completions.
const AL_TYPE_KEYWORDS: &[&str] = &[
    "Integer", "Decimal", "Text", "Code", "Boolean", "Date", "Time", "DateTime",
    "DateFormula", "Duration", "Guid", "BigInteger", "BigText", "Char", "Byte", "Blob",
    "Option", "Record", "RecordId", "RecordRef", "Variant", "Dialog", "File",
    "InStream", "OutStream", "List", "Dictionary", "Array",
    "HttpClient", "HttpContent", "HttpHeaders", "HttpRequestMessage", "HttpResponseMessage",
    "JsonArray", "JsonObject", "JsonToken", "JsonValue",
    "XmlDocument", "XmlElement", "XmlNode", "XmlNodeList",
    "TextBuilder", "Notification", "ErrorInfo", "SecretText", "FilterPageBuilder",
    "Media", "MediaSet", "SessionSettings", "Label", "Enum", "Interface",
    "Codeunit", "Page", "Report", "Query", "XmlPort", "Action", "TestPage", "TestRequestPage",
];

/// AL built-in trigger-context variables.
const TRIGGER_VARIABLES: &[(&str, &str)] = &[
    ("Rec", "Record — the current record"),
    ("xRec", "Record — the previous record"),
    ("CurrPage", "Page — the current page"),
    ("CurrReport", "Report — the current report"),
    ("CurrFieldNo", "Integer — the current field number"),
    ("RequestOptionsPage", "Page — the request options page"),
];

use al_syntax::context::{CompletionContext, detect_context};

/// Get completions at a position in a document.
pub fn completions(workspace: &Workspace, uri: &Url, position: Position) -> Vec<CompletionEntry> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some(text) = workspace.documents.get_text(uri) else {
        return Vec::new();
    };
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
            for kw in AL_TYPE_KEYWORDS {
                items.push(CompletionEntry {
                    label: kw.to_string(),
                    kind: CompletionKind::Keyword,
                    detail: None, documentation: None, insert_text: None, sort_text: None,
                });
            }
            for kind in &[
                al_symbols::ObjectKind::Table,
                al_symbols::ObjectKind::Enum,
                al_symbols::ObjectKind::Codeunit,
                al_symbols::ObjectKind::Interface,
            ] {
                let entries = workspace.symbols.get_by_kind(*kind);
                for entry in entries.iter().take(50) {
                    items.push(CompletionEntry {
                        label: format!("\"{}\"", entry.name),
                        kind: CompletionKind::Class,
                        detail: Some(format!("{} {}", entry.kind, entry.id)),
                        documentation: None, insert_text: None, sort_text: None,
                    });
                }
            }
        }
        CompletionContext::TriggerBody => {
            for (name, detail) in TRIGGER_VARIABLES {
                items.push(CompletionEntry {
                    label: name.to_string(),
                    kind: CompletionKind::Variable,
                    detail: Some(detail.to_string()),
                    documentation: None, insert_text: None, sort_text: None,
                });
            }
            add_default_completions(workspace, uri, &text, lsp_pos, &mut items);
        }
        CompletionContext::Default => {
            add_default_completions(workspace, uri, &text, lsp_pos, &mut items);
        }
    }

    if !items.is_empty() {
        finalize_completion_items(&mut items);
    }
    items
}

fn add_default_completions(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    position: tower_lsp::lsp_types::Position,
    items: &mut Vec<CompletionEntry>,
) {
    for kw in AL_KEYWORDS {
        items.push(CompletionEntry {
            label: kw.to_string(),
            kind: CompletionKind::Keyword,
            detail: None, documentation: None, insert_text: None, sort_text: None,
        });
    }

    if let Some((file_text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) {
        let doc_symbols = al_syntax::extract_document_symbols(&tree, text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                        || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT
                    {
                        items.push(CompletionEntry {
                            label: child.name.clone(),
                            kind: CompletionKind::Function,
                            detail: child.detail.clone(),
                            documentation: None, insert_text: None, sort_text: None,
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
            let scope_label = match var.scope {
                al_syntax::type_resolver::VariableScope::Local => "local",
                al_syntax::type_resolver::VariableScope::Parameter => "parameter",
                al_syntax::type_resolver::VariableScope::Global => "global",
                al_syntax::type_resolver::VariableScope::SelfImplicit => "self",
                al_syntax::type_resolver::VariableScope::TriggerImplicit => "trigger",
            };
            items.push(CompletionEntry {
                label: var.name.clone(),
                kind: CompletionKind::Variable,
                detail: Some(format!("{}{} ({})", var.type_name, subtype, scope_label)),
                documentation: None,
                insert_text: None,
                sort_text: Some(format!("0_{}", var.name)),
            });
        }
    }

    let index_results = workspace.symbols.search("", 30);
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
            documentation: None, insert_text: None, sort_text: None,
        });
    }

    let builtins = match workspace.builtins.read() {
        Ok(guard) => guard.clone(),
        Err(_) => return,
    };
    for bt in builtins.iter() {
        items.push(CompletionEntry {
            label: bt.name.clone(),
            kind: CompletionKind::Class,
            detail: Some("built-in type".to_string()),
            documentation: None, insert_text: None, sort_text: None,
        });
    }
}

fn count_params(detail: &str) -> usize {
    let start = match detail.find('(') { Some(i) => i + 1, None => return 0 };
    let end = match detail.rfind(')') { Some(i) => i, None => return 0 };
    if start >= end { return 0; }
    let inner = detail[start..end].trim();
    if inner.is_empty() { return 0; }
    let mut count = 1usize;
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ';' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

fn finalize_completion_items(items: &mut Vec<CompletionEntry>) {
    // Sort so that non-keyword items precede keywords before dedup, ensuring
    // a workspace procedure with the same name as a keyword is not shadowed.
    items.sort_by_key(|item| if item.kind == CompletionKind::Keyword { 1u8 } else { 0u8 });
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.to_lowercase()));

    for item in items.iter_mut() {
        if item.sort_text.is_some() { continue; }
        let is_callable = matches!(item.kind, CompletionKind::Function | CompletionKind::Method);
        if is_callable {
            let pc = item.detail.as_deref().map(count_params).unwrap_or(0);
            item.sort_text = Some(format!("1_{pc:02}_{}", item.label.to_lowercase()));
        } else {
            item.sort_text = Some(format!("1_{}", item.label.to_lowercase()));
        }
    }

    items.sort_by(|a, b| {
        a.sort_text.as_deref().unwrap_or(a.label.as_str())
            .cmp(b.sort_text.as_deref().unwrap_or(b.label.as_str()))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
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
    CompletionEntry {
        label: item.label,
        kind,
        detail: item.detail,
        documentation,
        insert_text: item.insert_text,
        sort_text: item.sort_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_params_works() {
        assert_eq!(count_params("()"), 0);
        assert_eq!(count_params(""), 0);
        assert_eq!(count_params("(A: Text)"), 1);
        assert_eq!(count_params("(A: Text; B: Integer)"), 2);
        assert_eq!(count_params("(A: Text; B: Integer; C: Boolean)"), 3);
        assert_eq!(count_params("(A: List of [Text]; B: Integer)"), 2);
        assert_eq!(count_params("(A: Text): Boolean"), 1);
    }
}
