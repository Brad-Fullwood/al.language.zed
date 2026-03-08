//! Completion handler.
//!
//! Context-aware completions:
//! - After `.` -> member completions (methods, fields of the type)
//! - After `::` -> enum values
//! - In type position -> type keywords + object types from index
//! - Default -> keywords + procedures + variables + symbols from index

use tower_lsp::lsp_types::*;

use crate::parsing;
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

/// Detect the completion context from the cursor position.
#[derive(Debug, PartialEq)]
pub(crate) enum CompletionContext {
    /// After a `.` — member access
    MemberAccess,
    /// After `::` — enum member
    EnumAccess,
    /// In a type position (after `:` in a var declaration)
    TypePosition,
    /// Inside a trigger body — add trigger-specific variables
    #[allow(dead_code)]
    TriggerBody,
    /// Default completion context
    Default,
}

/// Detect the completion context from the text before the cursor.
pub(crate) fn detect_context(text: &str, position: Position) -> CompletionContext {
    let line_idx = position.line as usize;
    let col = position.character as usize;

    let line = match text.lines().nth(line_idx) {
        Some(l) => l,
        None => return CompletionContext::Default,
    };

    let prefix = if col <= line.len() {
        &line[..col]
    } else {
        line
    };

    let trimmed = prefix.trim_end();

    if trimmed.ends_with("::") {
        return CompletionContext::EnumAccess;
    }

    if trimmed.ends_with('.') {
        return CompletionContext::MemberAccess;
    }

    // Check if we're in a type position: look for "name:" or "name :" pattern
    let before_cursor = prefix.trim();
    if before_cursor.ends_with(':') && !before_cursor.ends_with(":=") {
        return CompletionContext::TypePosition;
    }

    // Check if the line before has a var declaration pattern
    // but exclude lines that contain := (assignment)
    if !before_cursor.contains(":=") {
        if let Some(colon_pos) = before_cursor.rfind(':') {
            let after_colon = before_cursor[colon_pos + 1..].trim();
            // If there's a colon earlier on the line and we're typing the type
            if !after_colon.is_empty() {
                // Likely typing a type name
                return CompletionContext::TypePosition;
            }
        }
    }

    CompletionContext::Default
}

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
            // Get the identifier before the dot
            let line_idx = position.line as usize;
            let col = position.character as usize;
            let line = text.lines().nth(line_idx)?;
            let prefix = &line[..col.min(line.len())];
            let before_dot = prefix.trim_end().strip_suffix('.')?;
            let var_name = extract_last_identifier(before_dot);

            // Look up methods/fields from package symbols
            let symbols = server.symbols.get_by_name(var_name);
            for entry in &symbols {
                for method in &entry.methods {
                    if method.is_local {
                        continue;
                    }
                    items.push(method_to_completion_item(method));
                }
                for field in &entry.fields {
                    items.push(field_to_completion_item(field));
                }
            }

            // Look up built-in type methods
            let builtins = server.builtins.read().unwrap().clone();
            for bt in builtins.iter() {
                if bt.name.eq_ignore_ascii_case(var_name) {
                    for method in &bt.methods {
                        items.push(builtin_method_to_completion_item(method));
                    }
                }
            }
        }

        CompletionContext::EnumAccess => {
            // Get the enum name before ::
            let line_idx = position.line as usize;
            let col = position.character as usize;
            let line = text.lines().nth(line_idx)?;
            let prefix = &line[..col.min(line.len())];
            let before_colons = prefix.trim_end().strip_suffix("::")?;
            let enum_name = extract_last_identifier(before_colons);

            // Find enum values from index
            let symbols = server.symbols.get_by_name(enum_name);
            for entry in &symbols {
                if matches!(
                    entry.kind,
                    al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension
                ) {
                    for ev in &entry.enum_values {
                        items.push(CompletionItem {
                            label: ev.name.clone(),
                            kind: Some(CompletionItemKind::ENUM_MEMBER),
                            detail: Some(format!("value({})", ev.ordinal)),
                            ..Default::default()
                        });
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
            add_default_completions(server, uri, &text, &mut items);
        }

        CompletionContext::Default => {
            add_default_completions(server, uri, &text, &mut items);
        }
    }

    if items.is_empty() {
        None
    } else {
        Some(CompletionResponse::Array(items))
    }
}

/// Add default completions: keywords, local procedures, symbols.
fn add_default_completions(server: &AlServer, uri: &Url, text: &str, items: &mut Vec<CompletionItem>) {
    // Keywords
    for kw in AL_KEYWORDS {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    // Extract procedures from the current file
    if let Some((_, tree)) = parsing::get_or_parse(server, uri) {
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

/// Extract the last identifier from a string (e.g., "Rec" from "Rec").
pub(crate) fn extract_last_identifier(s: &str) -> &str {
    let s = s.trim();
    // Handle quoted identifiers
    if s.ends_with('"') {
        if let Some(start) = s[..s.len() - 1].rfind('"') {
            return &s[start + 1..s.len() - 1];
        }
    }
    // Find last word boundary
    let bytes = s.as_bytes();
    let end = bytes.len();
    // Walk backwards to find identifier start
    for i in (0..bytes.len()).rev() {
        let ch = bytes[i] as char;
        if ch.is_alphanumeric() || ch == '_' {
            continue;
        }
        return &s[i + 1..end];
    }
    &s[..end]
}

/// Convert a MethodSymbol from the index to a CompletionItem.
fn method_to_completion_item(method: &al_symbols::MethodSymbol) -> CompletionItem {
    let params: Vec<String> = method
        .parameters
        .iter()
        .map(|p| {
            let var_prefix = if p.is_var { "var " } else { "" };
            format!("{}{}: {}", var_prefix, p.name, p.type_name)
        })
        .collect();
    let detail = format!("({})", params.join("; "));

    CompletionItem {
        label: method.name.clone(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some(detail),
        ..Default::default()
    }
}

/// Convert a FieldSymbol from the index to a CompletionItem.
fn field_to_completion_item(field: &al_symbols::FieldSymbol) -> CompletionItem {
    CompletionItem {
        label: field.name.clone(),
        kind: Some(CompletionItemKind::FIELD),
        detail: Some(format!("{}: {}", field.id, field.type_name)),
        ..Default::default()
    }
}

/// Convert a BuiltinMethod to a CompletionItem.
fn builtin_method_to_completion_item(method: &al_semantic::BuiltinMethod) -> CompletionItem {
    let params: Vec<String> = method
        .parameters
        .iter()
        .map(|p| {
            let var_prefix = if p.is_var { "var " } else { "" };
            format!("{}{}: {}", var_prefix, p.name, p.type_name)
        })
        .collect();
    let detail = format!("({})", params.join("; "));

    CompletionItem {
        label: method.name.clone(),
        kind: Some(CompletionItemKind::METHOD),
        detail: Some(detail),
        documentation: if method.documentation.is_empty() {
            None
        } else {
            Some(Documentation::String(method.documentation.clone()))
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_context_member_access() {
        let text = "Rec.\n";
        let pos = Position {
            line: 0,
            character: 4,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::MemberAccess);
    }

    #[test]
    fn test_detect_context_enum_access() {
        let text = "MyEnum::\n";
        let pos = Position {
            line: 0,
            character: 8,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::EnumAccess);
    }

    #[test]
    fn test_detect_context_type_position() {
        let text = "x:\n";
        let pos = Position {
            line: 0,
            character: 2,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::TypePosition);
    }

    #[test]
    fn test_detect_context_not_type_after_assign() {
        let text = "x:=\n";
        let pos = Position {
            line: 0,
            character: 3,
        };
        // := is assignment, not type position
        assert_eq!(detect_context(text, pos), CompletionContext::Default);
    }

    #[test]
    fn test_detect_context_default() {
        let text = "Message\n";
        let pos = Position {
            line: 0,
            character: 7,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::Default);
    }

    #[test]
    fn test_extract_last_identifier() {
        assert_eq!(extract_last_identifier("Rec"), "Rec");
        assert_eq!(extract_last_identifier("x.Rec"), "Rec");
        assert_eq!(extract_last_identifier("  MyVar  "), "MyVar");
    }

    #[test]
    fn test_extract_last_identifier_quoted() {
        assert_eq!(
            extract_last_identifier("\"Customer Ledger Entry\""),
            "Customer Ledger Entry"
        );
    }
}
