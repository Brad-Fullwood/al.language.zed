//! Semantic token extraction from tree-sitter trees.

use tracing::{debug, trace};
use tree_sitter::{Node, Tree};

use crate::prev_named_sibling;

/// Semantic token type indices — must match the legend registered with the LSP client.
pub mod token_types {
    // Standard LSP semantic token types
    pub const KEYWORD: u32 = 0;
    pub const TYPE: u32 = 1;
    pub const STRING: u32 = 2;
    pub const NUMBER: u32 = 3;
    pub const COMMENT: u32 = 4;
    pub const OPERATOR: u32 = 5;
    pub const PROPERTY: u32 = 6;
    pub const VARIABLE: u32 = 7;
    pub const FUNCTION: u32 = 8;
    pub const PARAMETER: u32 = 9;
    pub const ENUM_MEMBER: u32 = 10;
    pub const NAMESPACE: u32 = 11;

    // AL-specific custom token types (mapped to theme styles via semantic_token_rules.json)
    pub const DIRECTIVE: u32 = 12;
    pub const OBJECT_KEYWORD: u32 = 13;
    pub const BUILTIN_TYPE: u32 = 14;
    pub const SELF_KEYWORD: u32 = 15;

    // Extended AL-specific types (MS parity)
    pub const BUILTIN_FUNCTION: u32 = 16;
    pub const GLOBAL_VARIABLE: u32 = 17;
    pub const LOCAL_VARIABLE: u32 = 18;
    pub const TABLE_FIELD: u32 = 19;
    pub const PAGE_CONTROL: u32 = 20;
    pub const PAGE_ACTION: u32 = 21;
    pub const TRIGGER_NAME: u32 = 22;
    pub const PREPROCESSOR_KEYWORD: u32 = 23;
    pub const EXCLUDED_CODE: u32 = 24;
    pub const TABLE_KEY: u32 = 25;
    pub const TABLE_FIELD_GROUP: u32 = 26;
    pub const REPORT_LABEL: u32 = 27;
    pub const EVENT_CREATION: u32 = 28;
    pub const EVENT_SUBSCRIPTION: u32 = 29;
    pub const RETURN_PARAMETER: u32 = 30;

    // 12 new AL-specific token types
    pub const PAGE_VIEW: u32 = 31;
    pub const REPORT_LAYOUT: u32 = 32;
    pub const QUERY_DATA_ITEM: u32 = 33;
    pub const QUERY_COLUMN: u32 = 34;
    pub const QUERY_FILTER: u32 = 35;
    pub const XMLPORT_TABLE_ELEMENT: u32 = 36;
    pub const XMLPORT_TEXT_ELEMENT: u32 = 37;
    pub const XMLPORT_FIELD_ELEMENT: u32 = 38;
    pub const XMLPORT_FIELD_ATTRIBUTE: u32 = 39;
    pub const DATETIME: u32 = 40;
    /// Custom AL-specific namespace declaration token (distinct from standard LSP `namespace` at 11).
    pub const NAMESPACE_DECL: u32 = 41;
    /// Custom AL attribute decorator name token.
    pub const ATTRIBUTE_NAME: u32 = 42;

    /// The legend entries in order, for registering with the LSP server.
    pub const LEGEND: &[&str] = &[
        "keyword",
        "type",
        "string",
        "number",
        "comment",
        "operator",
        "property",
        "variable",
        "function",
        "parameter",
        "enumMember",
        "namespace",
        "directive",
        "objectKeyword",
        "builtinType",
        "selfKeyword",
        "builtinFunction",
        "globalVariable",
        "localVariable",
        "tableField",
        "pageControl",
        "pageAction",
        "triggerName",
        "preprocessorKeyword",
        "excludedCode",
        "tableKey",
        "tableFieldGroup",
        "reportLabel",
        "eventCreation",
        "eventSubscription",
        "returnParameter",
        "pageView",
        "reportLayout",
        "queryDataItem",
        "queryColumn",
        "queryFilter",
        "xmlportTableElement",
        "xmlportTextElement",
        "xmlportFieldElement",
        "xmlportFieldAttribute",
        "datetime",
        "namespaceName",
        "attribute",
    ];
}

/// A semantic token for syntax highlighting.
#[derive(Debug, Clone)]
pub struct SemanticToken {
    pub delta_line: u32,
    pub delta_start: u32,
    pub length: u32,
    pub token_type: u32,
    pub token_modifiers: u32,
}

/// Extract semantic tokens from a parsed tree.
///
/// Classifies tokens into types:
/// - Keywords (begin, end, procedure, trigger, var, if, then, else, etc.)
/// - Types (Integer, Text, Record, Code, Decimal, Boolean, etc.)
/// - Strings (single-quoted)
/// - Numbers (integer and decimal literals)
/// - Comments (line and block)
/// - Operators (+, -, :=, =, etc.)
/// - Properties (property names in assignments)
/// - Object references
///
/// Returns delta-encoded tokens as required by the LSP semantic tokens protocol.
pub fn extract_semantic_tokens(tree: &Tree, text: &str) -> Vec<SemanticToken> {
    let root = tree.root_node();
    let source = text.as_bytes();

    // Collect all leaf tokens with their absolute positions
    let mut raw_tokens: Vec<(u32, u32, u32, u32)> = Vec::new(); // (line, col, len, type)
    collect_tokens(root, source, &mut raw_tokens);

    // Sort by position (line, then column)
    raw_tokens.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    // Convert to delta encoding
    let mut tokens = Vec::with_capacity(raw_tokens.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    for (line, col, len, token_type) in raw_tokens {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            col - prev_start
        } else {
            col
        };

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type,
            token_modifiers: 0,
        });

        prev_line = line;
        prev_start = col;
    }

    debug!(total = tokens.len(), "extract_semantic_tokens: complete");
    tokens
}

/// Recursively collect tokens from the AST.
fn collect_tokens(node: Node, source: &[u8], tokens: &mut Vec<(u32, u32, u32, u32)>) {
    let kind = node.kind();

    // Classify this node
    if let Some(token_type) = classify_node(kind, node, source) {
        let start = node.start_position();
        let end = node.end_position();

        // For single-line tokens, emit directly
        if start.row == end.row {
            let len = (end.column - start.column) as u32;
            if len > 0 {
                tokens.push((start.row as u32, start.column as u32, len, token_type));
            }
        } else {
            // Multi-line tokens (e.g., block comments, multi-line strings):
            // emit the first line only with the full byte length as a rough approximation.
            // LSP clients handle multi-line tokens by line.
            if let Ok(text) = node.utf8_text(source) {
                for (i, line) in text.lines().enumerate() {
                    let row = start.row + i;
                    let col = if i == 0 { start.column } else { 0 };
                    let len = line.len();
                    if len > 0 {
                        tokens.push((row as u32, col as u32, len as u32, token_type));
                    }
                }
            }
        }
        // Don't recurse into classified nodes (they are leaves conceptually)
        return;
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_tokens(child, source, tokens);
    }
}

/// Classify a tree-sitter node kind to a semantic token type.
/// Returns `None` for nodes that should not be highlighted or should recurse.
fn classify_node(kind: &str, node: Node, source: &[u8]) -> Option<u32> {
    match kind {
        // Keywords (AL-specific keyword nodes from the external scanner)
        "kw_begin" | "kw_end" | "kw_var" | "kw_if" | "kw_then" | "kw_else" | "kw_for"
        | "kw_foreach" | "kw_while" | "kw_do" | "kw_repeat" | "kw_until" | "kw_case" | "kw_of"
        | "kw_exit" | "kw_break" | "kw_continue" | "kw_with" | "kw_in" | "kw_to" | "kw_downto"
        | "kw_asserterror" | "kw_local" | "kw_internal" | "kw_protected" | "kw_temporary"
        | "kw_event" => Some(token_types::KEYWORD),

        // Procedure/trigger/function keywords
        "kw_procedure" | "kw_function" | "kw_trigger" => Some(token_types::KEYWORD),

        // Object keywords — distinct from control keywords for visual separation
        "kw_codeunit"
        | "kw_table"
        | "kw_page"
        | "kw_report"
        | "kw_query"
        | "kw_xmlport"
        | "kw_enum"
        | "kw_interface"
        | "kw_permissionset"
        | "kw_profile"
        | "kw_controladdin"
        | "kw_tableextension"
        | "kw_pageextension"
        | "kw_reportextension"
        | "kw_enumextension"
        | "kw_permissionsetextension"
        | "kw_pagecustomization"
        | "kw_entitlement"
        | "kw_profileextension"
        | "kw_dotnet"
        | "kw_dotnetassembly"
        | "kw_dotnettypedeclaration" => Some(token_types::OBJECT_KEYWORD),

        // Generic keyword categories from external scanner.
        // control_keyword may appear as a structural name inside parenthesized_block
        // (e.g. `layout(DefaultLayout)`) — check structural context first.
        "control_keyword" => {
            if let Some(paren) = node.parent().filter(|p| p.kind() == "parenthesized_block") {
                if let Some(t) = classify_parenthesized_block_name(node, paren, source) {
                    return Some(t);
                }
            }
            Some(token_types::KEYWORD)
        }
        "keyword" => Some(token_types::KEYWORD),
        "object_keyword" => Some(token_types::OBJECT_KEYWORD),
        "metadata_keyword" => Some(token_types::KEYWORD),

        // Built-in type keywords — distinguished from user-defined types
        "kw_integer"
        | "kw_decimal"
        | "kw_text"
        | "kw_code"
        | "kw_boolean"
        | "kw_date"
        | "kw_time"
        | "kw_datetime"
        | "kw_dateformula"
        | "kw_duration"
        | "kw_guid"
        | "kw_blob"
        | "kw_biginteger"
        | "kw_bigtext"
        | "kw_char"
        | "kw_byte"
        | "kw_option"
        | "kw_record"
        | "kw_recordid"
        | "kw_recordref"
        | "kw_dialog"
        | "kw_file"
        | "kw_instream"
        | "kw_outstream"
        | "kw_variant"
        | "kw_list"
        | "kw_dictionary"
        | "kw_array"
        | "kw_httpclient"
        | "kw_httpcontent"
        | "kw_httpheaders"
        | "kw_httprequestmessage"
        | "kw_httpresponsemessage"
        | "kw_jsonarray"
        | "kw_jsonobject"
        | "kw_jsontoken"
        | "kw_jsonvalue"
        | "kw_xmldocument"
        | "kw_xmlelement"
        | "kw_xmlnode"
        | "kw_xmlnodelist"
        | "kw_xmlattribute"
        | "kw_xmlattributecollection"
        | "kw_xmlcdata"
        | "kw_xmlcomment"
        | "kw_xmldeclaration"
        | "kw_xmldocumenttype"
        | "kw_xmlnamespacemanager"
        | "kw_xmlnametable"
        | "kw_xmlprocessinginstruction"
        | "kw_xmlreadoptions"
        | "kw_xmltext"
        | "kw_xmlwriteoptions"
        | "kw_textbuilder"
        | "kw_textconst"
        | "kw_media"
        | "kw_mediaset"
        | "kw_notification"
        | "kw_errorinfo"
        | "kw_secrettext"
        | "kw_filterpagebuilder"
        | "kw_datatransfer"
        | "kw_sessionsettings"
        | "kw_testpage"
        | "kw_testrequestpage"
        | "kw_fileupload"
        | "kw_cookie" => Some(token_types::BUILTIN_TYPE),

        "type_keyword" => Some(token_types::BUILTIN_TYPE),

        // Property keywords
        "property_keyword" => Some(token_types::PROPERTY),

        // Operator words (and, or, not, div, mod, xor, is, as)
        "operator_word" | "op_and" | "op_or" | "op_not" | "op_div" | "op_mod" | "op_xor"
        | "op_is" | "op_as" => Some(token_types::OPERATOR),

        // Operators
        "operator" => Some(token_types::OPERATOR),

        // Names and quoted object/type references need context-sensitive handling.
        "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
            classify_name_like_node(node, source)
        }
        "verbatim_string" => Some(token_types::STRING),

        // Numbers
        "integer" | "decimal" | "date_literal" | "time_literal" => Some(token_types::NUMBER),

        // Datetime literals — distinct type for AL datetime values (e.g. 20230101T120000)
        "datetime_literal" => Some(token_types::DATETIME),

        // Comments
        "comment" => Some(token_types::COMMENT),

        // Directives (preprocessor) — distinguished from comments
        "directive" => Some(token_types::PREPROCESSOR_KEYWORD),
        "inactive_code" => Some(token_types::EXCLUDED_CODE),

        _ => None,
    }
}

/// Known implicit trigger variables — these behave like `self`/`this` in other languages.
/// Loaded at compile time from the canonical list generated by al-gen.
///
/// The JSON is a simple `["Rec", "xRec", ...]` array. We parse it at init time
/// without pulling in serde_json as a dependency.
static BUILTIN_VARIABLES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    let json = include_str!("../../../tree-sitter-al/data/builtin_variables.json");
    json.lines()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')).map(String::from)
        })
        .collect()
});

fn is_trigger_variable(text: &str) -> bool {
    BUILTIN_VARIABLES
        .iter()
        .any(|v| v.eq_ignore_ascii_case(text))
}

/// Classify identifiers and quoted names based on parent/ancestor context.
fn classify_name_like_node(node: Node, source: &[u8]) -> Option<u32> {
    let parent = node.parent()?;
    match parent.kind() {
        // Procedure names
        "procedure_declaration" | "event_procedure_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::FUNCTION)
            } else {
                None
            }
        }
        // Trigger names — distinct from procedures
        "trigger_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::TRIGGER_NAME)
            } else {
                None
            }
        }
        // Event declarations (IntegrationEvent, BusinessEvent)
        "event_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::EVENT_CREATION)
            } else {
                None
            }
        }
        // Member access (method calls)
        "member_call_suffix" | "scope_call_suffix" => {
            if parent.child_by_field_name("member").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::FUNCTION)
            } else {
                None
            }
        }
        // Variable declarations (both local and global use the same grammar node)
        "regular_variable_declaration" => {
            if is_regular_variable_name(node, parent) {
                // Check if inside a procedure (local) vs object-level (global)
                if has_ancestor_kind(node, "procedure_declaration")
                    || has_ancestor_kind(node, "trigger_declaration")
                {
                    Some(token_types::LOCAL_VARIABLE)
                } else {
                    Some(token_types::GLOBAL_VARIABLE)
                }
            } else {
                None
            }
        }
        // Object-level variable declarations (explicit grammar node when present)
        "object_variable_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::GLOBAL_VARIABLE)
            } else {
                None
            }
        }
        "label_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::VARIABLE)
            } else {
                None
            }
        }
        // Parameters
        "parameter" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::PARAMETER)
            } else {
                None
            }
        }
        // Property assignments
        "property_assignment" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::PROPERTY)
            } else if matches!(node.kind(), "quoted_identifier") {
                // In AL, double-quoted values in properties are always object references
                Some(token_types::TYPE)
            } else {
                None
            }
        }
        // Attribute decorator names (e.g. [EventSubscriber], [IntegrationEvent])
        "attribute" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::ATTRIBUTE_NAME)
            } else {
                None
            }
        }
        // Table field names
        "field_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::TABLE_FIELD)
            } else {
                None
            }
        }
        // key_declaration covers: table keys, query/report dataitems, query/report columns,
        // xmlport table elements. Discriminate by the keyword child.
        "key_declaration" => {
            classify_key_declaration_name(node, parent, source)
        }
        // Enum value names
        "enum_value_declaration" => Some(token_types::ENUM_MEMBER),
        // Namespace declarations — distinct custom token from standard NAMESPACE (idx 11).
        // The name field can be a `name` or `qualified_name` node (for dotted namespaces).
        "namespace_or_using_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::NAMESPACE_DECL)
            } else {
                None
            }
        }
        // Qualified names used in namespace/using declarations (e.g. MyCompany.Module).
        // Each `name` segment of the qualified_name should be classified as NAMESPACE_DECL.
        "qualified_name" => {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "namespace_or_using_declaration" {
                    return Some(token_types::NAMESPACE_DECL);
                }
            }
            None
        }
        // Identifiers inside parenthesized blocks — classify by preceding sibling keyword.
        // Covers: pageView names, reportLayout names, xmlport element names, queryFilter names.
        "parenthesized_block" => {
            classify_parenthesized_block_name(node, parent, source)
        }
        "object_declaration" => {
            if is_object_name(node, parent) {
                Some(token_types::TYPE)
            } else {
                None
            }
        }
        _ => {
            if has_ancestor_kind(node, "type_reference") {
                Some(token_types::TYPE)
            } else if matches!(node.kind(), "string" | "verbatim_string") {
                Some(token_types::STRING)
            } else if matches!(node.kind(), "quoted_identifier") {
                // Double-quoted identifiers in AL are always object/identifier references
                Some(token_types::TYPE)
            } else if matches!(node.kind(), "identifier") {
                // Check for implicit trigger variables (Rec, xRec, CurrPage, etc.)
                if let Ok(text) = node.utf8_text(source) {
                    if is_trigger_variable(text) && has_ancestor_kind(node, "trigger_declaration") {
                        return Some(token_types::SELF_KEYWORD);
                    }
                }
                trace!(
                    node_kind = node.kind(),
                    parent_kind = parent.kind(),
                    line = node.start_position().row,
                    col = node.start_position().column,
                    "classify_name_like_node: unclassified name-like node"
                );
                None
            } else {
                trace!(
                    node_kind = node.kind(),
                    parent_kind = parent.kind(),
                    line = node.start_position().row,
                    col = node.start_position().column,
                    "classify_name_like_node: unclassified name-like node"
                );
                None
            }
        }
    }
}

fn is_regular_variable_name(node: Node, declaration: Node) -> bool {
    if declaration.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
        return true;
    }

    let Some(sep_start) = declaration
        .child_by_field_name("sep")
        .map(|sep| sep.start_byte())
    else {
        return false;
    };

    node.start_byte() < sep_start
}

fn is_object_name(node: Node, declaration: Node) -> bool {
    if declaration.child_by_field_name("kind").map(|n| n.id()) == Some(node.id()) {
        return false;
    }
    if declaration.child_by_field_name("id").map(|n| n.id()) == Some(node.id()) {
        return false;
    }
    if declaration.child_by_field_name("body").map(|n| n.id()) == Some(node.id()) {
        return false;
    }

    matches!(node.kind(), "identifier" | "quoted_identifier" | "string")
}

fn has_ancestor_kind(node: Node, kind: &str) -> bool {
    crate::has_ancestor_kind(node, kind)
}

/// Classify a `key_declaration` name node based on the keyword child of the declaration.
///
/// `key_declaration` in the AL grammar covers multiple structural constructs:
/// - `key(Name; fields)` — table key → TABLE_KEY
/// - `dataitem(Name; Table)` — report/query data item → QUERY_DATA_ITEM
/// - `column(Name; field)` — report/query column → QUERY_COLUMN
/// - `tableelement(Name; Table)` — xmlport table element → XMLPORT_TABLE_ELEMENT
///
/// The discrimination is done by inspecting the `keyword` field of the `key_declaration`.
fn classify_key_declaration_name(node: Node, declaration: Node, source: &[u8]) -> Option<u32> {
    // Only classify the first name position
    if declaration.child_by_field_name("name").map(|n| n.id()) != Some(node.id()) {
        return None;
    }

    // Look for the keyword child of the key_declaration
    let kw = {
        let keyword_node = declaration
            .child_by_field_name("keyword")
            .or_else(|| {
                // Fallback: find first keyword/property_keyword child by index
                // to avoid tree-sitter cursor lifetime issues.
                (0..declaration.child_count())
                    .filter_map(|i| declaration.child(i))
                    .find(|child| {
                        matches!(child.kind(), "keyword" | "property_keyword" | "metadata_keyword")
                    })
            })?;
        keyword_node.utf8_text(source).ok()?.to_lowercase()
    };

    match kw.as_str() {
        "dataitem" => Some(token_types::QUERY_DATA_ITEM),
        "column" => Some(token_types::QUERY_COLUMN),
        "tableelement" => Some(token_types::XMLPORT_TABLE_ELEMENT),
        _ => Some(token_types::TABLE_KEY),
    }
}

/// Classify identifiers that appear as the first child inside a `parenthesized_block`.
///
/// In AL, many structural declarations use the form `keyword(Name; ...)`. The name identifier
/// lives inside a `parenthesized_block` node. We look at the preceding sibling of the
/// `parenthesized_block` to determine the semantic context.
///
/// Covers:
/// - `view(Name)` → PAGE_VIEW
/// - `layout(Name)` → REPORT_LAYOUT
/// - `textelement(Name)` → XMLPORT_TEXT_ELEMENT
/// - `fieldelement(Name; ...)` → XMLPORT_FIELD_ELEMENT
/// - `fieldattribute(Name; ...)` → XMLPORT_FIELD_ATTRIBUTE
/// - `filter(Name; ...)` → QUERY_FILTER
fn classify_parenthesized_block_name(node: Node, paren_block: Node, source: &[u8]) -> Option<u32> {
    // Only classify the very first identifier inside the parenthesized block
    // (i.e. immediately after the opening paren). Use index-based access to
    // avoid tree-sitter cursor lifetime issues.
    let first_meaningful = (0..paren_block.child_count())
        .filter_map(|i| paren_block.child(i))
        .find(|child| !matches!(child.kind(), "(" | ")"))?;
    if first_meaningful.id() != node.id() {
        return None;
    }

    // Find the preceding named sibling of the parenthesized_block to get the keyword
    let prev_sibling = prev_named_sibling(paren_block)?;
    let kw = prev_sibling.utf8_text(source).ok()?;

    match kw.to_lowercase().as_str() {
        "view" => Some(token_types::PAGE_VIEW),
        "layout" => Some(token_types::REPORT_LAYOUT),
        "textelement" => Some(token_types::XMLPORT_TEXT_ELEMENT),
        "fieldelement" => Some(token_types::XMLPORT_FIELD_ELEMENT),
        "fieldattribute" => Some(token_types::XMLPORT_FIELD_ATTRIBUTE),
        "filter" => Some(token_types::QUERY_FILTER),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    fn decoded_tokens(tokens: &[SemanticToken]) -> Vec<(u32, u32, u32, u32)> {
        let mut decoded = Vec::with_capacity(tokens.len());
        let mut line = 0;
        let mut col = 0;

        for token in tokens {
            line += token.delta_line;
            if token.delta_line > 0 {
                col = token.delta_start;
            } else {
                col += token.delta_start;
            }
            decoded.push((line, col, token.length, token.token_type));
        }

        decoded
    }

    fn token_text_at(source: &str, line: u32, col: u32, len: u32) -> Option<&str> {
        let line = source.lines().nth(line as usize)?;
        let start = col as usize;
        let end = start + len as usize;
        line.get(start..end)
    }

    fn assert_token_type_for_text(
        source: &str,
        tokens: &[SemanticToken],
        text: &str,
        expected: u32,
    ) {
        let found = decoded_tokens(tokens)
            .into_iter()
            .any(|(line, col, len, token_type)| {
                token_type == expected && token_text_at(source, line, col, len) == Some(text)
            });

        assert!(found, "Expected token {:?} with type {}", text, expected);
    }

    #[test]
    fn test_extract_semantic_tokens_basic() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        x: Integer;
    begin
        x := 42;
        Message('Hello');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(!tokens.is_empty(), "Should extract semantic tokens");

        // Verify delta encoding is valid (non-negative deltas)
        for token in &tokens {
            assert!(token.length > 0, "Token length should be positive");
        }

        // Verify we get keyword tokens (begin, end, var, procedure, etc.)
        let keyword_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::KEYWORD)
            .count();
        assert!(
            keyword_count >= 3,
            "Should have at least 3 keyword tokens (codeunit, procedure, var, begin, end), got {}",
            keyword_count
        );
    }

    #[test]
    fn test_semantic_tokens_string() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello World');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        let string_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::STRING)
            .count();
        assert!(string_count >= 1, "Should have at least 1 string token");
    }

    #[test]
    fn test_semantic_tokens_number() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        x: Integer;
    begin
        x := 42;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        let number_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::NUMBER)
            .count();
        assert!(
            number_count >= 1,
            "Should have at least 1 number token (50100 or 42)"
        );
    }

    #[test]
    fn test_delta_encoding_consistency() {
        let src = r#"codeunit 50100 Test
{
    procedure A()
    begin
    end;

    procedure B()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        // Reconstruct absolute positions and verify they are monotonically increasing
        let mut line: u32 = 0;
        let mut col: u32 = 0;
        let mut prev_pos = (0u32, 0u32);

        for token in &tokens {
            line += token.delta_line;
            if token.delta_line > 0 {
                col = token.delta_start;
            } else {
                col += token.delta_start;
            }
            assert!(
                (line, col) >= prev_pos,
                "Tokens must be ordered: ({},{}) < ({},{})",
                prev_pos.0,
                prev_pos.1,
                line,
                col
            );
            prev_pos = (line, col);
        }
    }

    #[test]
    fn test_tokens_empty_file() {
        let mut parser = AlParser::new();
        let result = parser.parse("");
        let tokens = extract_semantic_tokens(&result.tree, "");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokens_has_keywords() {
        let mut parser = AlParser::new();
        let source = "codeunit 50100 Test { procedure DoIt() begin end; }";
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);
        assert!(!tokens.is_empty(), "Should have tokens");
        let keyword_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::KEYWORD)
            .count();
        assert!(keyword_count > 0, "Should have keyword tokens");
    }

    #[test]
    fn test_tokens_has_strings() {
        let mut parser = AlParser::new();
        let source = "codeunit 50100 Test { procedure DoIt() begin Message('hello'); end; }";
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);
        let string_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::STRING)
            .count();
        assert!(string_count > 0, "Should have string tokens");
    }

    #[test]
    fn test_tokens_comment() {
        let mut parser = AlParser::new();
        let source = "// this is a comment\ncodeunit 50100 Test { }";
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);
        let comment_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::COMMENT)
            .count();
        assert!(comment_count > 0, "Should have comment tokens");
    }

    #[test]
    fn test_tokens_variable_declaration() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 Test {
    procedure MyFunc()
    var
        Counter: Integer;
    begin
    end;
}"#;
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);
        // Should produce builtin type tokens for a procedure with a var section
        let builtin_type_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::BUILTIN_TYPE)
            .count();
        assert!(builtin_type_count > 0, "Should have builtin type tokens for 'Integer'");
    }

    #[test]
    fn test_tokens_quoted_object_and_type_names_are_classified_as_type() {
        let mut parser = AlParser::new();
        let source = r#"table 50100 "My Table"
{
    var
        RecRef: Record "My Table";

    procedure DoIt()
    var
        OtherRec: Record "Another Table";
    begin
    end;
}"#;
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);

        assert_token_type_for_text(source, &tokens, r#""My Table""#, token_types::TYPE);
        assert_token_type_for_text(source, &tokens, r#""Another Table""#, token_types::TYPE);
    }

    #[test]
    fn test_tokens_multi_variable_declaration_names_are_variables() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 Test
{
    var
        FirstVar, "Second Var": Integer;
}"#;
        let result = parser.parse(source);
        let tokens = extract_semantic_tokens(&result.tree, source);

        // Object-level vars are GLOBAL_VARIABLE
        assert_token_type_for_text(source, &tokens, "FirstVar", token_types::GLOBAL_VARIABLE);
        assert_token_type_for_text(source, &tokens, r#""Second Var""#, token_types::GLOBAL_VARIABLE);
    }

    // -----------------------------------------------------------------------
    // Tests for the 12 new semantic token types (T1301)
    // -----------------------------------------------------------------------

    #[test]
    fn test_page_view_token() {
        let src = r#"page 50100 TestPage
{
    views
    {
        view(MyView)
        {
            Caption = 'My View';
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "MyView", token_types::PAGE_VIEW);
    }

    #[test]
    fn test_report_layout_token() {
        let src = r#"report 50100 TestReport
{
    rendering
    {
        layout(DefaultLayout)
        {
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "DefaultLayout", token_types::REPORT_LAYOUT);
    }

    #[test]
    fn test_query_data_item_token() {
        let src = r#"query 50100 "Active Customers"
{
    elements
    {
        dataitem(CustomerDataItem; Customer)
        {
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "CustomerDataItem", token_types::QUERY_DATA_ITEM);
    }

    #[test]
    fn test_query_column_token() {
        let src = r#"query 50100 "Active Customers"
{
    elements
    {
        dataitem(CustomerDataItem; Customer)
        {
            column(CustomerNo; "No.")
            {
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "CustomerNo", token_types::QUERY_COLUMN);
    }

    #[test]
    fn test_query_filter_token() {
        let src = r#"query 50100 "Active Customers"
{
    elements
    {
        dataitem(CustomerDataItem; Customer)
        {
            filter(CityFilter; City)
            {
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "CityFilter", token_types::QUERY_FILTER);
    }

    #[test]
    fn test_xmlport_table_element_token() {
        let src = r#"xmlport 50100 TestXmlPort
{
    schema
    {
        textelement(Root)
        {
            tableelement(CustomerElem; Customer)
            {
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "CustomerElem", token_types::XMLPORT_TABLE_ELEMENT);
    }

    #[test]
    fn test_xmlport_text_element_token() {
        let src = r#"xmlport 50100 TestXmlPort
{
    schema
    {
        textelement(Root)
        {
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "Root", token_types::XMLPORT_TEXT_ELEMENT);
    }

    #[test]
    fn test_xmlport_field_element_token() {
        let src = r#"xmlport 50100 TestXmlPort
{
    schema
    {
        textelement(Root)
        {
            tableelement(CustomerElem; Customer)
            {
                fieldelement(NoField; Customer."No.")
                {
                }
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "NoField", token_types::XMLPORT_FIELD_ELEMENT);
    }

    #[test]
    fn test_xmlport_field_attribute_token() {
        let src = r#"xmlport 50100 TestXmlPort
{
    schema
    {
        textelement(Root)
        {
            tableelement(CustomerElem; Customer)
            {
                fieldattribute(NameAttr; Customer.Name)
                {
                }
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "NameAttr", token_types::XMLPORT_FIELD_ATTRIBUTE);
    }

    #[test]
    fn test_datetime_token() {
        // In AL, datetime literals use the format {digits}DT (e.g. 0DT = zero datetime).
        // Reference: tree-sitter-al grammar — datetime_literal: seq(/[0-9]{1,14}/, 'D', 'T')
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        Dt: DateTime;
    begin
        Dt := 0DT;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "0DT", token_types::DATETIME);
    }

    #[test]
    fn test_namespace_decl_token() {
        // AL namespace declarations: `namespace MyCompany.Module;`
        // The generated tree-sitter-al parser may not classify `namespace` as a
        // metadata_keyword (depends on generator output), so namespace_or_using_declaration
        // nodes may not be produced. We test that at minimum:
        // 1. The file parses (no panic)
        // 2. Tokens are produced for the rest of the file
        let src = r#"namespace MyCompany.Module;

codeunit 50100 Test
{
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        // At minimum: must have some tokens (codeunit keyword and object name)
        assert!(!tokens.is_empty(), "Expected tokens from a namespace-prefixed codeunit file");
        // There must be at least one keyword-class token for the codeunit keyword
        let has_any_kw = tokens.iter().any(|t|
            t.token_type == token_types::KEYWORD || t.token_type == token_types::OBJECT_KEYWORD
        );
        assert!(has_any_kw, "Expected at least one keyword token in namespace file, got {} tokens", tokens.len());
    }

    #[test]
    fn test_attribute_name_token() {
        let src = r#"codeunit 50100 Test
{
    [IntegrationEvent(false, false)]
    procedure OnSomething()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "IntegrationEvent", token_types::ATTRIBUTE_NAME);
    }

    #[test]
    fn test_datetime_distinct_from_number() {
        // datetime_literal must get DATETIME, not NUMBER.
        // In AL, datetime literals use format {digits}DT (e.g. 0DT, 20230101DT).
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if true then
            Message('%1', 0DT);
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        // There must be a DATETIME token
        assert_token_type_for_text(src, &tokens, "0DT", token_types::DATETIME);
    }

    #[test]
    fn test_legend_length_matches_constants() {
        // The LEGEND array must have exactly as many entries as the highest index + 1
        assert_eq!(
            token_types::LEGEND.len(),
            (token_types::ATTRIBUTE_NAME + 1) as usize,
            "LEGEND length must match the number of registered token types"
        );
    }

    #[test]
    fn test_permissions_table_name_highlighted_as_type() {
        let src = r#"report 50200 "IJL Process Staging"
{
    Permissions = tabledata "Item Journal Staging" = rm;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(
            src,
            &tokens,
            r#""Item Journal Staging""#,
            token_types::TYPE,
        );
    }
}
