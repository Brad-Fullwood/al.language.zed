//! Completion handler.
//!
//! Context-aware completions:
//! - After `.` -> member completions (methods, fields of the type)
//! - After `::` -> enum values
//! - In type position -> type keywords + object types from index
//! - Default -> keywords + procedures + variables + symbols from index

use tower_lsp::lsp_types::*;

use crate::parsing;
use crate::resolution;
use crate::server::AlServer;

/// AL keywords for general completion.
const AL_KEYWORDS: &[&str] = &[
    "begin",
    "end",
    "var",
    "procedure",
    "trigger",
    "local",
    "internal",
    "protected",
    "if",
    "then",
    "else",
    "case",
    "of",
    "for",
    "to",
    "downto",
    "do",
    "foreach",
    "in",
    "while",
    "repeat",
    "until",
    "exit",
    "break",
    "with",
    "asserterror",
    "true",
    "false",
    "not",
    "and",
    "or",
    "xor",
    "div",
    "mod",
];

/// AL type keywords for type position completions.
const AL_TYPE_KEYWORDS: &[&str] = &[
    "Integer",
    "Decimal",
    "Text",
    "Code",
    "Boolean",
    "Date",
    "Time",
    "DateTime",
    "DateFormula",
    "Duration",
    "Guid",
    "BigInteger",
    "BigText",
    "Char",
    "Byte",
    "Blob",
    "Option",
    "Record",
    "RecordId",
    "RecordRef",
    "Variant",
    "Dialog",
    "File",
    "InStream",
    "OutStream",
    "List",
    "Dictionary",
    "Array",
    "HttpClient",
    "HttpContent",
    "HttpHeaders",
    "HttpRequestMessage",
    "HttpResponseMessage",
    "JsonArray",
    "JsonObject",
    "JsonToken",
    "JsonValue",
    "XmlDocument",
    "XmlElement",
    "XmlNode",
    "XmlNodeList",
    "TextBuilder",
    "Notification",
    "ErrorInfo",
    "SecretText",
    "FilterPageBuilder",
    "Media",
    "MediaSet",
    "SessionSettings",
    "Label",
    "Enum",
    "Interface",
    "Codeunit",
    "Page",
    "Report",
    "Query",
    "XmlPort",
    "Action",
    "TestPage",
    "TestRequestPage",
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

/// Handle textDocument/completion.
pub(crate) fn handle_completion(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<CompletionResponse> {
    let text = server.documents.get_text(uri)?;
    let context = detect_context(&text, position);

    let mut items = Vec::new();

    match context {
        CompletionContext::MemberAccess => {
            if let Some((file_text, tree)) = parsing::get_or_parse(server, uri) {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position) {
                    if let Some(receiver) = resolution::resolve_expression_type(
                        server,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    ) {
                        items.extend(resolution::completion_items_for_receiver(server, &receiver));
                    }
                }
            }
        }

        CompletionContext::EnumAccess => {
            if let Some((file_text, tree)) = parsing::get_or_parse(server, uri) {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position) {
                    if let Some(enum_type) = resolution::resolve_expression_type(
                        server,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    ) {
                        items.extend(resolution::enum_completion_items(server, &enum_type));
                    }
                }
            }
        }

        CompletionContext::TypePosition => {
            // Add type keywords
            for kw in AL_TYPE_KEYWORDS {
                items.push(CompletionItem {
                    label: kw.to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..Default::default()
                });
            }

            // Add object type names from the index (tables, enums, codeunits, etc.)
            for kind in &[
                al_symbols::ObjectKind::Table,
                al_symbols::ObjectKind::Enum,
                al_symbols::ObjectKind::Codeunit,
                al_symbols::ObjectKind::Interface,
            ] {
                let entries = server.symbols.get_by_kind(*kind);
                for entry in entries.iter().take(50) {
                    items.push(CompletionItem {
                        label: format!("\"{}\"", entry.name),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(format!("{} {}", entry.kind, entry.id)),
                        ..Default::default()
                    });
                }
            }
        }

        CompletionContext::TriggerBody => {
            // Add trigger-specific variables
            for (name, detail) in TRIGGER_VARIABLES {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(detail.to_string()),
                    ..Default::default()
                });
            }
            // Also include default items
            add_default_completions(server, uri, &text, position, &mut items);
        }

        CompletionContext::Default => {
            add_default_completions(server, uri, &text, position, &mut items);
        }
    }

    if items.is_empty() {
        None
    } else {
        finalize_completion_items(&mut items);
        Some(CompletionResponse::Array(items))
    }
}

/// Add default completions: keywords, local procedures, variables, symbols.
fn add_default_completions(server: &AlServer, uri: &Url, text: &str, position: Position, items: &mut Vec<CompletionItem>) {
    // Keywords
    for kw in AL_KEYWORDS {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    // Extract procedures from the current file + add visible variables via TypeResolver
    if let Some((file_text, tree)) = parsing::get_or_parse(server, uri) {
        let doc_symbols = al_syntax::extract_document_symbols(&tree, text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT {
                        items.push(CompletionItem {
                            label: child.name.clone(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            detail: child.detail.clone(),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        // Add local/global variables visible at the cursor position via TypeResolver
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
            items.push(CompletionItem {
                label: var.name.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(format!("{}{} ({})", var.type_name, subtype, scope_label)),
                sort_text: Some(format!("0_{}", var.name)), // Sort variables first
                ..Default::default()
            });
        }
    }

    // Add symbols from index (top-level objects, limited)
    let index_results = server.symbols.search("", 30);
    for entry in &index_results {
        items.push(CompletionItem {
            label: entry.name.clone(),
            kind: Some(match entry.kind {
                al_symbols::ObjectKind::Table | al_symbols::ObjectKind::TableExtension => {
                    CompletionItemKind::STRUCT
                }
                al_symbols::ObjectKind::Codeunit => CompletionItemKind::MODULE,
                al_symbols::ObjectKind::Page | al_symbols::ObjectKind::PageExtension => {
                    CompletionItemKind::CLASS
                }
                al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension => {
                    CompletionItemKind::ENUM
                }
                _ => CompletionItemKind::REFERENCE,
            }),
            detail: Some(format!("{} {}", entry.kind, entry.id)),
            ..Default::default()
        });
    }

    // Add built-in type names
    let builtins = server.builtins.read().unwrap().clone();
    for bt in builtins.iter() {
        items.push(CompletionItem {
            label: bt.name.clone(),
            kind: Some(CompletionItemKind::CLASS),
            detail: Some("built-in type".to_string()),
            ..Default::default()
        });
    }
}

fn finalize_completion_items(items: &mut Vec<CompletionItem>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.to_lowercase()));
    for item in items.iter_mut() {
        if item.sort_text.is_none() {
            item.sort_text = Some(format!("1_{}", item.label.to_lowercase()));
        }
    }
    items.sort_by(|a, b| {
        a.sort_text
            .as_deref()
            .unwrap_or(a.label.as_str())
            .cmp(b.sort_text.as_deref().unwrap_or(b.label.as_str()))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
}

// Tests for detect_context, extract_last_identifier moved to al_syntax::context

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_server;
    use std::path::PathBuf;

    #[test]
    fn member_completion_uses_receiver_chain_before_cursor() {
        let server = test_server();
        let path = PathBuf::from("/tmp/item-journal-api.page.al");
        let uri = Url::from_file_path(&path).expect("file uri");
        let source = r#"page 50201 "Item Journal API"
{
    layout
    {
        area(Content)
        {
            repeater(Records)
            {
                field(status; this.StatusText)
                {
                }
            }
        }
    }

    var
        StatusText: Text;

    trigger OnAfterGetRecord()
    begin
    end;
}"#;

        server.documents.open(uri.clone(), source.to_string());
        server.workspace_files.insert(path.clone(), source.to_string());
        server
            .workspace_objects
            .insert("item journal api".to_string(), path);

        let response = handle_completion(
            &server,
            &uri,
            Position {
                line: 8,
                character: 35,
            },
        )
        .expect("completion response");

        let items = match response {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        let labels = items.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert!(labels.iter().any(|label| label == "StatusText"));
        assert!(labels.iter().any(|label| label == "OnAfterGetRecord"));
        assert!(!labels.iter().any(|label| label == "begin"));
    }

    #[test]
    fn enum_completion_includes_workspace_enum_values() {
        let server = test_server();
        let page_path = PathBuf::from("/tmp/report.al");
        let enum_path = PathBuf::from("/tmp/status.enum.al");
        let uri = Url::from_file_path(&page_path).expect("file uri");
        let source = r#"report 1 Test
{
    trigger OnPreReport()
    begin
        if Staging.Status:: then;
    end;

    var
        Staging: Record "Item Journal Staging";
}"#;
        let table_source = r#"table 1 "Item Journal Staging"
{
    fields
    {
        field(1; Status; Enum "IJL Status")
        {
        }
    }
}"#;
        let table_path = PathBuf::from("/tmp/staging.table.al");
        let enum_source = r#"enum 1 "IJL Status"
{
    value(0; Pending) { }
    value(1; Posting) { }
}"#;

        server.documents.open(uri.clone(), source.to_string());
        server.workspace_files.insert(page_path.clone(), source.to_string());
        server.workspace_objects.insert("test".to_string(), page_path);
        server
            .workspace_files
            .insert(table_path.clone(), table_source.to_string());
        server
            .workspace_objects
            .insert("item journal staging".to_string(), table_path);
        server
            .workspace_files
            .insert(enum_path.clone(), enum_source.to_string());
        server
            .workspace_objects
            .insert("ijl status".to_string(), enum_path);

        let response = handle_completion(
            &server,
            &uri,
            Position {
                line: 4,
                character: 28,
            },
        )
        .expect("enum completion response");

        let items = match response {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        let labels = items.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert!(labels.iter().any(|label| label == "Pending"));
        assert!(labels.iter().any(|label| label == "Posting"));
    }
}
