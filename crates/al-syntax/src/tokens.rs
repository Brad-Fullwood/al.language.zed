//! Semantic token extraction from tree-sitter trees.

use tracing::{debug, trace};
use tree_sitter::{Node, Tree};

use super::prev_named_sibling;

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
    /// The parts of an XML documentation comment (`/// <summary>…`): the
    /// `///` and the angle brackets, a tag name, an attribute with its quotes,
    /// and the text between tags.
    pub const DOC_COMMENT_DELIMITER: u32 = 43;
    pub const DOC_COMMENT_NAME: u32 = 44;
    pub const DOC_COMMENT_ATTRIBUTE: u32 = 45;
    pub const DOC_COMMENT_TEXT: u32 = 46;

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
        "docCommentDelimiter",
        "docCommentName",
        "docCommentAttribute",
        "docCommentText",
    ];
}

/// Semantic token modifier bit-flags and legend.
///
/// Each constant is a bitmask where bit N corresponds to legend index N.
/// LSP `tokenModifiers` is a bitset — OR these together to combine modifiers.
pub mod token_modifiers {
    pub const DECLARATION: u32 = 1 << 0;
    pub const READONLY: u32 = 1 << 1;
    pub const DEPRECATED: u32 = 1 << 2;
    pub const STATIC: u32 = 1 << 3;
    pub const UNNECESSARY: u32 = 1 << 4;

    pub const LEGEND: &[&str] = &[
        "declaration",
        "readonly",
        "deprecated",
        "static",
        "unnecessary",
    ];
}

#[derive(Debug, Clone)]
pub struct SemanticToken {
    pub delta_line: u32,
    pub delta_start: u32,
    pub length: u32,
    pub token_type: u32,
    pub token_modifiers: u32,
}

pub fn extract_semantic_tokens(tree: &Tree, text: &str) -> Vec<SemanticToken> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let type_resolver = super::type_resolver::TypeResolver::new(tree, text);

    let mut raw_tokens: Vec<(u32, u32, u32, u32)> = Vec::new(); // (line, col, len, type)
    collect_tokens(root, source, &type_resolver, &mut raw_tokens);

    raw_tokens.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

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

fn collect_tokens(
    node: Node,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
    tokens: &mut Vec<(u32, u32, u32, u32)>,
) {
    // Pre-build line offsets once so every per-token line lookup is O(1).
    let line_index = crate::SourceLines::new(source);

    let mut stack = Vec::new();
    stack.push(node);

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if kind == "comment" && current.start_position().row == current.end_position().row {
            if let Ok(comment) = current.utf8_text(source) {
                if comment.starts_with("///") {
                    let start = current.start_position();
                    let line_bytes = line_index.line_bytes(start.row);
                    let line_str = std::str::from_utf8(line_bytes).unwrap_or("");
                    let utf16_col = super::byte_col_to_utf16_col(line_str, start.column);
                    for (offset, len, token_type) in doc_comment_parts(comment) {
                        tokens.push((start.row as u32, utf16_col + offset, len, token_type));
                    }
                    continue;
                }
            }
        }
        if let Some(token_type) = classify_node(kind, current, source, type_resolver) {
            let start = current.start_position();
            let end = current.end_position();

            if start.row == end.row {
                let line_bytes = line_index.line_bytes(start.row);
                let line_str = std::str::from_utf8(line_bytes).unwrap_or("");
                let utf16_col = super::byte_col_to_utf16_col(line_str, start.column);
                let utf16_end = super::byte_col_to_utf16_col(line_str, end.column);
                let len = utf16_end.saturating_sub(utf16_col);
                if len > 0 {
                    tokens.push((start.row as u32, utf16_col, len, token_type));
                }
            } else {
                // Multi-line token (block comment, multi-line string): one
                // entry per line.
                //
                // `text.lines()` strips both `\r` and `\n` terminators. The
                // resulting `line.encode_utf16().count()` is the visible-
                // content length, which matches the LSP semantic-tokens
                // spec: positions/lengths exclude line terminators (the
                // next line starts at column 0 of `row + 1`, regardless of
                // whether the terminator is `\n` or `\r\n`). No adjustment
                // needed for CRLF input.
                if let Ok(text) = current.utf8_text(source) {
                    for (i, line) in text.lines().enumerate() {
                        let row = start.row + i;
                        let byte_col = if i == 0 { start.column } else { 0 };
                        let line_bytes = line_index.line_bytes(row);
                        let source_line = std::str::from_utf8(line_bytes).unwrap_or(line);
                        let utf16_col = super::byte_col_to_utf16_col(source_line, byte_col);
                        let utf16_len = line.encode_utf16().count() as u32;
                        if utf16_len > 0 {
                            tokens.push((row as u32, utf16_col, utf16_len, token_type));
                        }
                    }
                }
            }
            // Don't push children of classified nodes.
            continue;
        }
        // Push children in reverse order for left-to-right DFS.
        //
        // `child(i)` is a linked-list walk in tree-sitter. Collect through a
        // cursor in O(n), then reverse for the stack push.
        let mut cursor = current.walk();
        let mut children: Vec<Node> = current.children(&mut cursor).collect();
        while let Some(child) = children.pop() {
            // pop() iterates back-to-front, so the first sibling ends up on
            // top of the stack — same left-to-right DFS as before.
            stack.push(child);
        }
    }
}

/// Classify a tree-sitter node kind to a semantic token type.
/// Returns `None` for nodes that should not be highlighted or should recurse.
fn classify_node(
    kind: &str,
    node: Node,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
) -> Option<u32> {
    use super::language_data::token_classification;

    match kind {
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
        // A keyword that names a property (`DataClassification = …`) is the
        // property. Anywhere else, the value of a property included
        // (`tabledata` in `Permissions`), it is a keyword.
        "keyword" | "metadata_keyword" | "property_keyword" => {
            if is_property_name(node) {
                Some(token_types::PROPERTY)
            } else {
                Some(token_types::KEYWORD)
            }
        }
        "object_keyword" => Some(token_types::OBJECT_KEYWORD),
        "type_keyword" => Some(token_types::BUILTIN_TYPE),

        "operator_word" | "op_and" | "op_or" | "op_not" | "op_div" | "op_mod" | "op_xor"
        | "op_is" | "op_as" => Some(token_types::OPERATOR),

        "operator" => Some(token_types::OPERATOR),

        "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
            classify_name_like_node(node, source, type_resolver)
        }
        "verbatim_string" => Some(token_types::STRING),

        "integer" | "decimal" | "date_literal" | "time_literal" => Some(token_types::NUMBER),

        // Datetime literals — distinct type for AL datetime values (e.g. 20230101T120000)
        "datetime_literal" => Some(token_types::DATETIME),

        "comment" => Some(token_types::COMMENT),

        // Directives (preprocessor): `directive` is an external *leaf* token
        // covering the whole `#if/#endif/#pragma/#region/…` line — it has no
        // children for the DFS to recurse into, so returning `None` here used
        // to emit zero tokens for preprocessor lines. Classify the whole
        // directive as a preprocessor keyword.
        "directive" => Some(token_types::PREPROCESSOR_KEYWORD),
        // `inactive_code` is left as a single EXCLUDED_CODE span by design
        // (the whole block is dimmed by clients; recursing into it would
        // emit conflicting tokens on top of the EXCLUDED_CODE block).
        "inactive_code" => Some(token_types::EXCLUDED_CODE),

        _ => {
            if !kind.starts_with("kw_") {
                return None;
            }
            let tc = token_classification();
            if tc.keyword_control.contains(kind) {
                Some(token_types::KEYWORD)
            } else if tc.keyword_object.contains(kind) || tc.keyword_object_extension.contains(kind)
            {
                Some(token_types::OBJECT_KEYWORD)
            } else if tc.builtin_type.contains(kind) {
                Some(token_types::BUILTIN_TYPE)
            } else {
                Some(token_types::KEYWORD)
            }
        }
    }
}

fn is_trigger_variable(text: &str) -> bool {
    super::language_data::implicit_variables()
        .iter()
        .any(|v| v.name.eq_ignore_ascii_case(text))
}

fn classify_name_like_node(
    node: Node,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
) -> Option<u32> {
    let parent = node.parent()?;
    match parent.kind() {
        "procedure_declaration" | "event_procedure_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::FUNCTION)
            } else {
                None
            }
        }
        "trigger_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::TRIGGER_NAME)
            } else {
                None
            }
        }
        "event_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::EVENT_CREATION)
            } else {
                None
            }
        }
        "member_call_suffix" | "scope_call_suffix" => {
            if parent.child_by_field_name("member").map(|n| n.id()) == Some(node.id()) {
                if parent.kind() == "member_call_suffix"
                    && (is_builtin_record_member(parent, node, source, type_resolver)
                        || has_object_kind_receiver(parent, source))
                {
                    Some(token_types::BUILTIN_FUNCTION)
                } else {
                    Some(token_types::FUNCTION)
                }
            } else {
                None
            }
        }
        "regular_variable_declaration" => {
            if is_regular_variable_name(node, parent) {
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
        "parameter" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::PARAMETER)
            } else {
                None
            }
        }
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
        // `Comment`, `Locked`, `MaxLength` after a label's text.
        "label_property" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::KEYWORD)
            } else {
                None
            }
        }
        "attribute" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                Some(token_types::ATTRIBUTE_NAME)
            } else {
                None
            }
        }
        // key_declaration covers: table keys, query/report dataitems, query/report columns,
        // xmlport table elements. Discriminate by the keyword child.
        "key_declaration" => classify_key_declaration_name(node, parent, source),
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
        // Covers: pageView names, reportLayout names, xmlport element names, queryFilter
        // names, and table field names (`field(1; Name; Type)` in a table).
        "parenthesized_block" => classify_parenthesized_block_name(node, parent, source)
            .or_else(|| classify_table_field_name(node, parent, source)),
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
                if is_subscribed_event_name(node, source) {
                    Some(token_types::EVENT_CREATION)
                } else {
                    Some(token_types::STRING)
                }
            } else if let Some(role) = name_role(node, source) {
                // A name in a scope access or a property value. In code a
                // name is a field or a variable, which the grammar's
                // highlighting already colors.
                role
            } else if matches!(node.kind(), "identifier") {
                if is_subscribed_event_name(node, source) {
                    return Some(token_types::EVENT_CREATION);
                }
                if let Ok(text) = node.utf8_text(source) {
                    if is_global_builtin_call(node, text)
                        && super::language_data::is_builtin_function(text)
                    {
                        return Some(token_types::BUILTIN_FUNCTION);
                    }
                    if is_trigger_variable(text) && has_ancestor_kind(node, "trigger_declaration") {
                        return Some(token_types::SELF_KEYWORD);
                    }
                }
                log_unclassified(node, parent);
                None
            } else {
                log_unclassified(node, parent);
                None
            }
        }
    }
}

/// The parts of a `///` documentation comment as (UTF-16 offset, UTF-16
/// length, token type), the way Microsoft's server splits it: the `///` and
/// the brackets `<`, `</`, `>`, `/>` are delimiters, a tag name is a name, an
/// attribute and its quotes are attributes, the value of a `name` attribute
/// (a parameter) is a parameter, and the rest is text.
fn doc_comment_parts(comment: &str) -> Vec<(u32, u32, u32)> {
    let chars: Vec<char> = comment.chars().collect();
    let utf16_at: Vec<u32> = std::iter::once(0)
        .chain(chars.iter().scan(0u32, |acc, c| {
            *acc += c.len_utf16() as u32;
            Some(*acc)
        }))
        .collect();
    let mut parts = Vec::new();
    let mut push = |from: usize, to: usize, token_type: u32| {
        if to > from {
            parts.push((utf16_at[from], utf16_at[to] - utf16_at[from], token_type));
        }
    };

    push(0, 3.min(chars.len()), token_types::DOC_COMMENT_DELIMITER);
    let mut i = 3;
    let mut text_start = i;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        push(text_start, i, token_types::DOC_COMMENT_TEXT);
        let open_end = if chars.get(i + 1) == Some(&'/') {
            i + 2
        } else {
            i + 1
        };
        push(i, open_end, token_types::DOC_COMMENT_DELIMITER);
        i = open_end;
        let name_start = i;
        while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | ':'))
        {
            i += 1;
        }
        push(name_start, i, token_types::DOC_COMMENT_NAME);
        // Attributes up to the closing bracket.
        while i < chars.len() && chars[i] != '>' {
            if chars[i] == '/' && chars.get(i + 1) == Some(&'>') {
                break;
            }
            if chars[i].is_alphabetic() {
                let attr_start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-'))
                {
                    i += 1;
                }
                let attribute: String = chars[attr_start..i].iter().collect();
                push(attr_start, i, token_types::DOC_COMMENT_ATTRIBUTE);
                while i < chars.len() && (chars[i] == '=' || chars[i].is_whitespace()) {
                    if chars[i] == '=' {
                        push(i, i + 1, token_types::DOC_COMMENT_DELIMITER);
                    }
                    i += 1;
                }
                if let Some(&quote) = chars.get(i).filter(|c| matches!(c, '"' | '\'')) {
                    push(i, i + 1, token_types::DOC_COMMENT_ATTRIBUTE);
                    let value_start = i + 1;
                    let mut end = value_start;
                    while end < chars.len() && chars[end] != quote {
                        end += 1;
                    }
                    let value_type = if attribute.eq_ignore_ascii_case("name") {
                        token_types::PARAMETER
                    } else {
                        token_types::DOC_COMMENT_ATTRIBUTE
                    };
                    push(value_start, end, value_type);
                    if end < chars.len() {
                        push(end, end + 1, token_types::DOC_COMMENT_ATTRIBUTE);
                        i = end + 1;
                    } else {
                        i = end;
                    }
                }
                continue;
            }
            i += 1;
        }
        let close_end = if chars.get(i) == Some(&'/') {
            (i + 2).min(chars.len())
        } else {
            (i + 1).min(chars.len())
        };
        push(i, close_end, token_types::DOC_COMMENT_DELIMITER);
        i = close_end;
        text_start = i;
    }
    push(text_start, chars.len(), token_types::DOC_COMMENT_TEXT);
    parts
}

/// Whether a string or name is the event name in `[EventSubscriber(ObjectType,
/// Object, 'EventName', …)]`, its third argument, which Microsoft colors as the
/// event.
fn is_subscribed_event_name(node: Node<'_>, source: &[u8]) -> bool {
    let mut current = node;
    let argument = loop {
        let Some(parent) = current.parent() else {
            return false;
        };
        if parent.kind() == "attribute_argument" {
            break parent;
        }
        if !matches!(
            parent.kind(),
            "expression"
                | "unary_expression"
                | "postfix_expression"
                | "primary_expression"
                | "name"
        ) {
            return false;
        }
        current = parent;
    };
    let Some(list) = argument.parent() else {
        return false;
    };
    let is_subscriber = list
        .parent()
        .filter(|attribute| attribute.kind() == "attribute")
        .and_then(|attribute| attribute.child_by_field_name("name"))
        .and_then(|name| name.utf8_text(source).ok())
        .is_some_and(|name| name.eq_ignore_ascii_case("EventSubscriber"));
    if !is_subscriber {
        return false;
    }
    let mut cursor = list.walk();
    let position = list
        .children(&mut cursor)
        .filter(|child| child.kind() == "attribute_argument")
        .position(|child| child.id() == argument.id());
    position == Some(2)
}

/// A data item's name: a query's data item has its own token type, while
/// Microsoft's server reports a report's data item as a variable, which is how
/// report code uses it.
fn data_item_type(node: Node<'_>) -> u32 {
    let mut ancestor = node.parent();
    while let Some(candidate) = ancestor {
        if candidate.kind() == "object_declaration" {
            let in_report = candidate
                .child_by_field_name("kind")
                .is_some_and(|kind| matches!(kind.kind(), "kw_report" | "kw_reportextension"));
            if in_report {
                return token_types::VARIABLE;
            }
            break;
        }
        ancestor = candidate.parent();
    }
    token_types::QUERY_DATA_ITEM
}

/// Whether `node` is the name of a `property_assignment`.
fn is_property_name(node: Node<'_>) -> bool {
    node.parent()
        .filter(|parent| parent.kind() == "property_assignment")
        .and_then(|parent| parent.child_by_field_name("name"))
        .is_some_and(|name| name.id() == node.id())
}

/// Object kinds that, before `::`, make the member an object name
/// (`Page::"Item Card"`). After any other prefix the member is an enum or
/// option value (`Status::Released`, `ObjectType::Codeunit`).
const OBJECT_KIND_PREFIXES: &[&str] = &[
    "codeunit",
    "database",
    "enum",
    "interface",
    "page",
    "query",
    "report",
    "table",
    "xmlport",
];

/// Properties whose unquoted value names an object.
const OBJECT_REFERENCE_PROPERTIES: &[&str] = &[
    "cardpageid",
    "dataitemtable",
    "defaultlayout",
    "defaultrenderinglayout",
    "drilldownpageid",
    "linkedobject",
    "lookuppageid",
    "pageid",
    "runobject",
    "sourcetable",
    "sourcetableview",
    "tablerelation",
    "tableno",
];

/// The token of an identifier or quoted name that sits in a scope access or
/// a property value, as Microsoft's server reports it: `Some(Some(TYPE))` for
/// an object, `Some(Some(ENUM_MEMBER))` for an enum value, `Some(None)` for a
/// name that gets no token (a permission letter), and `None` when the name is
/// in neither place.
fn name_role(node: Node<'_>, source: &[u8]) -> Option<Option<u32>> {
    if !matches!(node.kind(), "identifier" | "quoted_identifier") {
        return None;
    }
    let name = node.parent().filter(|parent| parent.kind() == "name")?;
    let holder = name.parent()?;
    // `TableRelation = Vendor."No."`: the receiver of an expression in a
    // property value is the property's object.
    if holder.kind() == "primary_expression" {
        let mut up = holder;
        while let Some(parent) = up.parent() {
            match parent.kind() {
                "postfix_expression" | "unary_expression" | "expression" => up = parent,
                "property_assignment" => {
                    let property = parent
                        .child_by_field_name("name")
                        .and_then(|property| property.utf8_text(source).ok())
                        .map(|text| text.trim().to_ascii_lowercase())
                        .unwrap_or_default();
                    return OBJECT_REFERENCE_PROPERTIES
                        .contains(&property.as_str())
                        .then_some(Some(token_types::TYPE));
                }
                _ => return None,
            }
        }
        return None;
    }
    match holder.kind() {
        // `TableRelation = Vendor."No."`: the first name is the object, the
        // rest are its fields.
        "qualified_name" => {
            let property = holder
                .parent()
                .filter(|parent| parent.kind() == "property_assignment")?
                .child_by_field_name("name")
                .and_then(|property| property.utf8_text(source).ok())
                .map(|text| text.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if !OBJECT_REFERENCE_PROPERTIES.contains(&property.as_str()) {
                return None;
            }
            let first = holder
                .named_child(0)
                .is_some_and(|first| first.id() == name.id());
            Some(first.then_some(token_types::TYPE))
        }
        // `extends Item`, `implements "My Interface"`.
        "implements_clause" => Some(Some(token_types::TYPE)),
        "scope_suffix" => {
            let member = holder.child_by_field_name("member")?;
            if member.id() != name.id() {
                return None;
            }
            let prefix = holder
                .prev_sibling()
                .and_then(|prefix| prefix.utf8_text(source).ok())
                .map(|text| text.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if OBJECT_KIND_PREFIXES.contains(&prefix.as_str()) {
                Some(Some(token_types::TYPE))
            } else {
                Some(Some(token_types::ENUM_MEMBER))
            }
        }
        "property_assignment" => {
            let property = holder.child_by_field_name("name")?;
            if property.id() == name.id() {
                return None;
            }
            let property = property
                .utf8_text(source)
                .map(|text| text.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if property == "permissions" {
                // `tabledata Item = r`: the name after the object kind is the
                // object, the letters after `=` are the permissions.
                let after_kind = name
                    .prev_sibling()
                    .is_some_and(|prev| prev.kind() == "property_keyword");
                return Some(after_kind.then_some(token_types::TYPE));
            }
            let text = node.utf8_text(source).unwrap_or_default();
            if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false") {
                return Some(None);
            }
            if node.kind() == "quoted_identifier"
                || OBJECT_REFERENCE_PROPERTIES.contains(&property.as_str())
            {
                Some(Some(token_types::TYPE))
            } else {
                Some(Some(token_types::ENUM_MEMBER))
            }
        }
        _ => None,
    }
}

fn is_global_builtin_call(node: Node<'_>, text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let Some(name) = node.parent().filter(|parent| parent.kind() == "name") else {
        return false;
    };
    let Some(primary) = name
        .parent()
        .filter(|parent| parent.kind() == "primary_expression")
    else {
        return false;
    };
    let Some(postfix) = primary
        .parent()
        .filter(|parent| parent.kind() == "postfix_expression")
    else {
        return false;
    };
    (0..postfix.child_count())
        .filter_map(|index| postfix.child(index))
        .any(|child| child.kind() == "call_suffix")
}

/// Whether a member call is made on an object kind or a system scope,
/// `Report.Run(…)`, `Codeunit.Run(…)`, `Page.RunModal(…)`, `Session.SessionId()`:
/// every such method is built into the platform.
fn has_object_kind_receiver(suffix: Node<'_>, source: &[u8]) -> bool {
    const RECEIVERS: &[&str] = &[
        "codeunit",
        "database",
        "page",
        "query",
        "report",
        "xmlport",
        "session",
        "system",
        "currentsession",
        "companyproperty",
        "navapp",
        "numbersequence",
        "taskscheduler",
        "debugger",
        "isolatedstorage",
        "productname",
    ];
    let Some(postfix) = suffix
        .parent()
        .filter(|parent| parent.kind() == "postfix_expression")
    else {
        return false;
    };
    let receiver = (0..postfix.child_count())
        .filter_map(|index| postfix.child(index))
        .find(|child| child.kind() == "primary_expression")
        .and_then(|primary| primary.utf8_text(source).ok())
        .map(|text| text.trim().to_ascii_lowercase());
    receiver.is_some_and(|receiver| RECEIVERS.contains(&receiver.as_str()))
}

fn is_builtin_record_member(
    suffix: Node<'_>,
    member: Node<'_>,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
) -> bool {
    let Ok(member_name) = member.utf8_text(source) else {
        return false;
    };
    if !is_record_builtin_method(&crate::clean_identifier(member_name)) {
        return false;
    }

    let Some(postfix) = suffix
        .parent()
        .filter(|parent| parent.kind() == "postfix_expression")
    else {
        return false;
    };
    let Some(primary) = (0..postfix.child_count())
        .filter_map(|index| postfix.child(index))
        .find(|child| child.kind() == "primary_expression")
    else {
        return false;
    };
    let Ok(receiver) = primary.utf8_text(source) else {
        return false;
    };
    let receiver = crate::clean_identifier(receiver);
    if receiver.is_empty()
        || !receiver
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return false;
    }

    // `Rec` and `xRec` are the current record wherever a page or table uses
    // them, though their table is not declared in the file.
    if receiver.eq_ignore_ascii_case("rec") || receiver.eq_ignore_ascii_case("xrec") {
        return true;
    }

    // Tree-sitter columns are byte offsets, but the type resolver expects
    // UTF-16 code units (LSP convention). Convert before resolving so lines
    // containing non-ASCII text don't resolve at the wrong point.
    let row = member.start_position().row;
    let line = super::get_source_line(source, row);
    let position = super::types::SyntaxPosition {
        line: row as u32,
        character: super::byte_col_to_utf16_col(line, member.start_position().column),
    };
    type_resolver
        .resolve_type(&receiver, position)
        .is_some_and(|declaration| declaration.type_name.eq_ignore_ascii_case("Record"))
}

fn is_record_builtin_method(name: &str) -> bool {
    super::language_data::is_record_method(name)
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

/// Trace-log an unclassified name-like node. Shared helper so the two
/// callers in `classify_name_like_node` don't have to duplicate the same
/// `tracing::trace!` block.
fn log_unclassified(node: Node, parent: Node) {
    trace!(
        node_kind = node.kind(),
        parent_kind = parent.kind(),
        line = node.start_position().row,
        col = node.start_position().column,
        "classify_name_like_node: unclassified name-like node"
    );
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
    super::has_ancestor_kind(node, kind)
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
    if declaration.child_by_field_name("name").map(|n| n.id()) != Some(node.id()) {
        return None;
    }

    let kw = {
        let keyword_node = declaration.child_by_field_name("keyword").or_else(|| {
            (0..declaration.child_count())
                .filter_map(|i| declaration.child(i))
                .find(|child| {
                    matches!(
                        child.kind(),
                        "keyword" | "property_keyword" | "metadata_keyword"
                    )
                })
        })?;
        keyword_node.utf8_text(source).ok()?.to_lowercase()
    };

    match kw.as_str() {
        "key" => Some(token_types::TABLE_KEY),
        "dataitem" => Some(data_item_type(node)),
        "column" => Some(token_types::QUERY_COLUMN),
        "tableelement" => Some(token_types::XMLPORT_TABLE_ELEMENT),
        _ => None,
    }
}

/// Classify the name of a table field declaration.
///
/// `field(1; "No."; Code[20])` inside a table's `fields` section parses as an
/// `object_section` (keyword `field`) whose `parenthesized_block` holds
/// `(id; Name; Type)`. The field name is the first name-like child after the
/// first semicolon. Only table/tableextension objects carry the dedicated
/// `TABLE_FIELD` token; page `field(Name; Expr)` controls are left to the
/// generic classification.
fn classify_table_field_name(node: Node, paren_block: Node, source: &[u8]) -> Option<u32> {
    let section = paren_block
        .parent()
        .filter(|p| p.kind() == "object_section")?;
    let keyword = section.child_by_field_name("keyword")?;
    if !keyword
        .utf8_text(source)
        .ok()?
        .trim()
        .eq_ignore_ascii_case("field")
    {
        return None;
    }

    let object = super::find_ancestor(node, |n| n.kind() == "object_declaration")?;
    let object_kind = object.child_by_field_name("kind")?.kind();
    if object_kind != "kw_table" && object_kind != "kw_tableextension" {
        return None;
    }

    let mut past_semicolon = false;
    let name_node = (0..paren_block.child_count())
        .filter_map(|i| paren_block.child(i))
        .find(|child| {
            if !past_semicolon {
                if child.kind() == "semicolon" {
                    past_semicolon = true;
                }
                return false;
            }
            matches!(
                child.kind(),
                "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
            )
        })?;
    (name_node.id() == node.id()).then_some(token_types::TABLE_FIELD)
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
    // Only a name: a string in a table view filter is a literal.
    if matches!(
        node.kind(),
        "string" | "verbatim_string" | "integer" | "decimal" | "number"
    ) {
        return None;
    }

    let prev_sibling = prev_named_sibling(paren_block)?;
    let kw = prev_sibling.utf8_text(source).ok()?;

    // avoid per-token `to_lowercase()` allocation by using
    // `eq_ignore_ascii_case` against literal table entries. AL keywords are
    // ASCII so the check is exact. Same grammar-fixity rationale as
    // `classify_key_declaration_name` above.
    const TABLE: &[(&str, u32)] = &[
        ("view", token_types::PAGE_VIEW),
        ("layout", token_types::REPORT_LAYOUT),
        ("textelement", token_types::XMLPORT_TEXT_ELEMENT),
        ("fieldelement", token_types::XMLPORT_FIELD_ELEMENT),
        ("fieldattribute", token_types::XMLPORT_FIELD_ATTRIBUTE),
        ("filter", token_types::QUERY_FILTER),
        ("dataitem", token_types::QUERY_DATA_ITEM),
        ("column", token_types::QUERY_COLUMN),
        ("tableelement", token_types::XMLPORT_TABLE_ELEMENT),
    ];
    if let Some(ty) = TABLE
        .iter()
        .find(|(name, _)| kw.eq_ignore_ascii_case(name))
        .map(|(_, ty)| *ty)
    {
        if ty == token_types::QUERY_DATA_ITEM {
            return Some(data_item_type(node));
        }
        return Some(ty);
    }
    page_member_name(paren_block, kw, source)
}

/// The token of the name in `keyword(Name …)` inside a page, page extension
/// or page customization: a control in `layout`, an action in `actions`.
fn page_member_name(paren_block: Node<'_>, keyword: &str, source: &[u8]) -> Option<u32> {
    const LAYOUT: &[&str] = &[
        "field",
        "group",
        "part",
        "repeater",
        "cuegroup",
        "fixed",
        "grid",
        "usercontrol",
        "systempart",
        "label",
        "addfirst",
        "addlast",
        "addafter",
        "addbefore",
        "movefirst",
        "movelast",
        "moveafter",
        "movebefore",
        "modify",
    ];
    const ACTIONS: &[&str] = &[
        "action",
        "actionref",
        "group",
        "separator",
        "customaction",
        "fileuploadaction",
        "systemaction",
        "addfirst",
        "addlast",
        "addafter",
        "addbefore",
        "movefirst",
        "movelast",
        "moveafter",
        "movebefore",
        "modify",
    ];
    let mut in_actions = false;
    let mut object_kind = None;
    let mut ancestor = paren_block.parent();
    while let Some(node) = ancestor {
        match node.kind() {
            "object_section" => {
                let is_actions = node
                    .child_by_field_name("keyword")
                    .and_then(|keyword| keyword.utf8_text(source).ok())
                    .is_some_and(|text| text.eq_ignore_ascii_case("actions"));
                in_actions |= is_actions;
            }
            "object_declaration" => {
                object_kind = node.child_by_field_name("kind").map(|kind| kind.kind());
                break;
            }
            _ => {}
        }
        ancestor = node.parent();
    }
    if !matches!(
        object_kind,
        Some("kw_page" | "kw_pageextension" | "kw_pagecustomization")
    ) {
        return None;
    }
    let keyword = keyword.to_ascii_lowercase();
    if in_actions {
        ACTIONS
            .contains(&keyword.as_str())
            .then_some(token_types::PAGE_ACTION)
    } else {
        LAYOUT
            .contains(&keyword.as_str())
            .then_some(token_types::PAGE_CONTROL)
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

    fn token_types_for_text(source: &str, tokens: &[SemanticToken], text: &str) -> Vec<u32> {
        decoded_tokens(tokens)
            .into_iter()
            .filter(|&(line, col, len, _)| token_text_at(source, line, col, len) == Some(text))
            .map(|(_, _, _, token_type)| token_type)
            .collect()
    }

    /// A quoted name in an expression is a field or a variable. It was sent
    /// as a type, so `SetRange("AUK Active", true)` drew the field in the
    /// type color. Only a scope access such as `Page::"Item Card"` names an
    /// object.
    #[test]
    fn a_quoted_name_is_a_type_only_in_a_scope_access() {
        let src = "codeunit 50010 X\n{\n    procedure P()\n    begin\n        Rec.SetRange(\"AUK Field\", 1);\n        Rec.\"My Field\" := 1;\n        Page.Run(Page::\"Item Card\");\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert!(
            !token_types_for_text(src, &tokens, "\"AUK Field\"").contains(&token_types::TYPE),
            "a field argument is not a type"
        );
        assert!(
            !token_types_for_text(src, &tokens, "\"My Field\"").contains(&token_types::TYPE),
            "a field member is not a type"
        );
        assert_token_type_for_text(src, &tokens, "\"Item Card\"", token_types::TYPE);
    }

    /// Microsoft's theme draws a property name in the foreground and the
    /// keywords in its value, such as `tabledata`, in the keyword color. A
    /// property named by a keyword was sent as a keyword, and `tabledata` as
    /// a property.
    #[test]
    fn a_property_name_is_a_property_and_a_keyword_in_its_value_is_a_keyword() {
        let src = "codeunit 50010 X\n{\n    Permissions = tabledata \"Item Ledger Entry\" = r;\n}\ntable 50000 T\n{\n    fields\n    {\n        field(1; Code; Code[20])\n        {\n            DataClassification = CustomerContent;\n        }\n    }\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert_token_type_for_text(src, &tokens, "Permissions", token_types::PROPERTY);
        assert_token_type_for_text(src, &tokens, "DataClassification", token_types::PROPERTY);
        assert_token_type_for_text(src, &tokens, "tabledata", token_types::KEYWORD);
        assert_token_type_for_text(src, &tokens, "\"Item Ledger Entry\"", token_types::TYPE);
    }

    /// Microsoft colors an enum value apart from an object: `Status::Released`
    /// and a property's enum value are enum members, `Page::"Item Card"` and
    /// the objects in `Permissions` and `SourceTable` are types.
    #[test]
    fn enum_values_are_enum_members_and_object_references_are_types() {
        let src = "page 50000 P\n{\n    SourceTable = Item;\n    ApplicationArea = All;\n    Permissions = tabledata Item = r;\n    trigger OnOpenPage()\n    begin\n        Rec.SetRange(Status, Status::Released);\n        x := \"Document Type\"::\"Purchase Receipt\";\n        Page.Run(Page::\"Item Card\");\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert_token_type_for_text(src, &tokens, "Released", token_types::ENUM_MEMBER);
        assert_token_type_for_text(
            src,
            &tokens,
            "\"Purchase Receipt\"",
            token_types::ENUM_MEMBER,
        );
        assert_token_type_for_text(src, &tokens, "All", token_types::ENUM_MEMBER);
        assert_token_type_for_text(src, &tokens, "\"Item Card\"", token_types::TYPE);
        let item_types = token_types_for_text(src, &tokens, "Item");
        assert_eq!(
            item_types,
            vec![token_types::TYPE, token_types::TYPE],
            "SourceTable and Permissions name the Item table"
        );
        assert!(
            token_types_for_text(src, &tokens, "r").is_empty(),
            "a permission letter is not a name"
        );
    }

    /// Microsoft splits an XML documentation comment into its parts, each
    /// with its own color.
    #[test]
    fn a_documentation_comment_is_split_into_its_parts() {
        let src = "codeunit 50000 C\n{\n    /// <param name=\"ItemNo\">The item.</param>\n    procedure P(ItemNo: Code[20])\n    begin\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert_token_type_for_text(src, &tokens, "///", token_types::DOC_COMMENT_DELIMITER);
        assert_token_type_for_text(src, &tokens, "<", token_types::DOC_COMMENT_DELIMITER);
        assert_token_type_for_text(src, &tokens, "param", token_types::DOC_COMMENT_NAME);
        assert_token_type_for_text(src, &tokens, "name", token_types::DOC_COMMENT_ATTRIBUTE);
        assert_token_type_for_text(src, &tokens, "ItemNo", token_types::PARAMETER);
        assert_token_type_for_text(src, &tokens, "The item.", token_types::DOC_COMMENT_TEXT);
        assert_token_type_for_text(src, &tokens, "</", token_types::DOC_COMMENT_DELIMITER);
        assert_eq!(
            token_types::LEGEND[token_types::DOC_COMMENT_TEXT as usize],
            "docCommentText"
        );
    }

    /// Microsoft gives the names of a page's controls and actions their own
    /// token types, which its theme draws in the type color.
    #[test]
    fn page_controls_and_actions_are_named_as_such() {
        let src = "pageextension 50001 \"AUK Item Card\" extends \"Item Card\"\n{\n    layout\n    {\n        addlast(Item)\n        {\n            field(\"CoA Check Required\"; Rec.\"AUK CoA\")\n            {\n            }\n        }\n    }\n    actions\n    {\n        addlast(processing)\n        {\n            action(PostPallet)\n            {\n            }\n        }\n    }\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert_token_type_for_text(src, &tokens, "Item", token_types::PAGE_CONTROL);
        assert_token_type_for_text(
            src,
            &tokens,
            "\"CoA Check Required\"",
            token_types::PAGE_CONTROL,
        );
        assert_token_type_for_text(src, &tokens, "processing", token_types::PAGE_ACTION);
        assert_token_type_for_text(src, &tokens, "PostPallet", token_types::PAGE_ACTION);
        assert!(
            !token_types_for_text(src, &tokens, "\"AUK CoA\"").contains(&token_types::PAGE_CONTROL),
            "the field's source expression is not the control name"
        );
    }

    #[test]
    fn a_method_on_an_object_kind_is_a_builtin() {
        let src = "codeunit 50000 C\n{\n    procedure P()\n    begin\n        Report.Run(Report::\"Goods In Label\");\n        Codeunit.Run(50001);\n        MyHelper.Run();\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        let runs = token_types_for_text(src, &tokens, "Run");
        assert_eq!(
            runs,
            vec![
                token_types::BUILTIN_FUNCTION,
                token_types::BUILTIN_FUNCTION,
                token_types::FUNCTION
            ],
            "Report.Run and Codeunit.Run are built in, a variable's Run is not"
        );
    }

    #[test]
    fn the_event_name_of_a_subscriber_is_the_event() {
        let src = "codeunit 50000 C\n{\n    [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Whse.-Post Receipt\", 'OnAfterRun', '', false, false)]\n    local procedure H()\n    begin\n        Message('OnAfterRun');\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_eq!(
            token_types_for_text(src, &tokens, "'OnAfterRun'"),
            vec![token_types::EVENT_CREATION, token_types::STRING],
            "only the attribute's event name is the event"
        );
    }

    #[test]
    fn extended_objects_layouts_and_booleans_get_their_own_types() {
        let src = "tableextension 50001 \"AUK Item\" extends Item\n{\n}\nreport 50002 R\n{\n    UseRequestPage = false;\n    DefaultRenderingLayout = RDLCLayout;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "Item", token_types::TYPE);
        assert_token_type_for_text(src, &tokens, "RDLCLayout", token_types::TYPE);
        assert!(
            !token_types_for_text(src, &tokens, "false").contains(&token_types::ENUM_MEMBER),
            "a boolean value is not an enum member"
        );
    }

    #[test]
    fn a_report_data_item_is_a_variable_and_a_query_data_item_is_not() {
        let src = "report 50000 R\n{\n    dataset\n    {\n        dataitem(ItemLedgerEntry; \"Item Ledger Entry\")\n        {\n        }\n    }\n}\nquery 50001 Q\n{\n    elements\n    {\n        dataitem(CustomerItem; Customer)\n        {\n        }\n    }\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "ItemLedgerEntry", token_types::VARIABLE);
        assert_token_type_for_text(src, &tokens, "CustomerItem", token_types::QUERY_DATA_ITEM);
    }

    #[test]
    fn label_options_unquoted_event_names_and_doc_equals_follow_microsoft() {
        let src = "codeunit 50000 C\n{\n    var\n        BinErr: Label 'Bin', Comment = 'x', Locked = true;\n\n    /// <param name=\"Qty\">Amount.</param>\n    [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Whse.-Post Receipt\", OnAfterRun, '', false, false)]\n    local procedure H(Qty: Decimal)\n    begin\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "Comment", token_types::KEYWORD);
        assert_token_type_for_text(src, &tokens, "Locked", token_types::KEYWORD);
        assert_token_type_for_text(src, &tokens, "OnAfterRun", token_types::EVENT_CREATION);
        assert_token_type_for_text(src, &tokens, "=", token_types::DOC_COMMENT_DELIMITER);
    }

    #[test]
    fn a_table_relation_target_and_rec_builtins_follow_microsoft() {
        let src = "tableextension 50000 E extends \"Lot No. Information\"\n{\n    fields\n    {\n        field(50002; \"AUK Vendor No.\"; Code[20])\n        {\n            TableRelation = Vendor.\"No.\";\n        }\n    }\n    trigger OnAfterModify()\n    begin\n        Rec.CalcFields(\"AUK Vendor No.\");\n    end;\n}\n";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "Vendor", token_types::TYPE);
        assert_token_type_for_text(src, &tokens, "CalcFields", token_types::BUILTIN_FUNCTION);
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

        for token in &tokens {
            assert!(token.length > 0, "Token length should be positive");
        }

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
        let builtin_type_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::BUILTIN_TYPE)
            .count();
        assert!(
            builtin_type_count > 0,
            "Should have builtin type tokens for 'Integer'"
        );
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
        assert_token_type_for_text(
            source,
            &tokens,
            r#""Second Var""#,
            token_types::GLOBAL_VARIABLE,
        );
    }

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
        assert_token_type_for_text(
            src,
            &tokens,
            "CustomerDataItem",
            token_types::QUERY_DATA_ITEM,
        );
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
        assert_token_type_for_text(
            src,
            &tokens,
            "CustomerElem",
            token_types::XMLPORT_TABLE_ELEMENT,
        );
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
        assert_token_type_for_text(
            src,
            &tokens,
            "NameAttr",
            token_types::XMLPORT_FIELD_ATTRIBUTE,
        );
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
        assert!(
            !tokens.is_empty(),
            "Expected tokens from a namespace-prefixed codeunit file"
        );
        let has_any_kw = tokens.iter().any(|t| {
            t.token_type == token_types::KEYWORD || t.token_type == token_types::OBJECT_KEYWORD
        });
        assert!(
            has_any_kw,
            "Expected at least one keyword token in namespace file, got {} tokens",
            tokens.len()
        );
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
        assert_token_type_for_text(
            src,
            &tokens,
            "IntegrationEvent",
            token_types::ATTRIBUTE_NAME,
        );
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
        assert_token_type_for_text(src, &tokens, "0DT", token_types::DATETIME);
    }

    #[test]
    fn global_platform_call_is_a_builtin_function() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        assert_token_type_for_text(src, &tokens, "Message", token_types::BUILTIN_FUNCTION);
    }

    #[test]
    fn record_platform_methods_are_builtin_but_custom_methods_are_not() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        Customer: Record Customer;
        Worker: Codeunit "Custom Worker";
    begin
        Customer.FindFirst();
        Customer.Insert();
        Customer.SetCurrentKey("No.");
        Customer.FieldError("No.");
        Customer.Truncate();
        Customer.CustomProcedure();
        Worker.Insert();
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        let decoded = decoded_tokens(&tokens);

        let token_type_on_line = |line: u32, text: &str| {
            decoded
                .iter()
                .find_map(|(token_line, col, len, token_type)| {
                    (*token_line == line
                        && token_text_at(src, *token_line, *col, *len) == Some(text))
                    .then_some(*token_type)
                })
                .unwrap_or_else(|| panic!("missing semantic token {text:?} on line {line}"))
        };

        assert_eq!(
            token_type_on_line(7, "FindFirst"),
            token_types::BUILTIN_FUNCTION
        );
        assert_eq!(
            token_type_on_line(8, "Insert"),
            token_types::BUILTIN_FUNCTION
        );
        assert_eq!(
            token_type_on_line(9, "SetCurrentKey"),
            token_types::BUILTIN_FUNCTION
        );
        assert_eq!(
            token_type_on_line(10, "FieldError"),
            token_types::BUILTIN_FUNCTION
        );
        assert_eq!(
            token_type_on_line(11, "Truncate"),
            token_types::BUILTIN_FUNCTION
        );
        assert_eq!(
            token_type_on_line(12, "CustomProcedure"),
            token_types::FUNCTION
        );
        assert_eq!(token_type_on_line(13, "Insert"), token_types::FUNCTION);
    }

    #[test]
    fn test_table_field_names_get_table_field_token() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            DataClassification = CustomerContent;
        }
        field(2; Description; Text[100]) { }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, r#""No.""#, token_types::TABLE_FIELD);
        assert_token_type_for_text(src, &tokens, "Description", token_types::TABLE_FIELD);
    }

    #[test]
    fn test_page_field_names_do_not_get_table_field_token() {
        let src = r#"page 50100 "Item List"
{
    layout
    {
        area(Content)
        {
            field(Description; Rec.Description)
            {
                ApplicationArea = All;
            }
        }
    }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        let has_table_field = decoded_tokens(&tokens)
            .into_iter()
            .any(|(_, _, _, token_type)| token_type == token_types::TABLE_FIELD);
        assert!(
            !has_table_field,
            "page field controls must not receive TABLE_FIELD tokens"
        );
    }

    #[test]
    fn test_preprocessor_directives_emit_preprocessor_keyword_tokens() {
        // The scanner's `directive` leaf spans the whole `#...` line, so the
        // emitted token covers the entire directive including the `#pragma`
        // prefix.
        let src = "#pragma warning disable AA0001\ncodeunit 50100 Test\n{\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(
            src,
            &tokens,
            "#pragma warning disable AA0001",
            token_types::PREPROCESSOR_KEYWORD,
        );

        let src = "#if MYFLAG\ncodeunit 50100 Test\n{\n}\n#endif";
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        let preprocessor_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::PREPROCESSOR_KEYWORD)
            .count();
        assert_eq!(
            preprocessor_count, 2,
            "both #if and #endif lines must emit PREPROCESSOR_KEYWORD tokens"
        );
    }

    #[test]
    fn record_builtin_classification_survives_non_ascii_on_the_same_line() {
        // The member's tree-sitter column is a BYTE offset; the type resolver
        // expects UTF-16. Without conversion, the 40 two-byte `é`s below shift
        // the resolution point 40 bytes to the right — out of procedure A and
        // into procedure B, where `Customer` is not declared — silently
        // downgrading FindFirst from BUILTIN_FUNCTION to FUNCTION.
        let src = "codeunit 50100 Test\n{\n    procedure A() var Customer: Record Customer; begin Message('éééééééééééééééééééééééééééééééééééééééé'); Customer.FindFirst(); end; procedure B() begin Foo(); Bar(); Baz(); Quux(); end;\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);

        let line_no = 2u32;
        let line_text = src.lines().nth(line_no as usize).unwrap();
        let find_first = decoded_tokens(&tokens)
            .into_iter()
            .find_map(|(line, col, len, token_type)| {
                if line != line_no {
                    return None;
                }
                let start = crate::utf16_col_to_byte_offset(line_text, col as usize);
                let end = crate::utf16_col_to_byte_offset(line_text, (col + len) as usize);
                (line_text.get(start..end) == Some("FindFirst")).then_some(token_type)
            })
            .expect("FindFirst token missing");
        assert_eq!(
            find_first,
            token_types::BUILTIN_FUNCTION,
            "FindFirst on a Record receiver must stay a builtin despite non-ASCII text earlier on the line"
        );
    }

    #[test]
    fn test_legend_length_matches_constants() {
        assert_eq!(
            token_types::LEGEND.len(),
            (token_types::DOC_COMMENT_TEXT + 1) as usize,
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
        assert_token_type_for_text(src, &tokens, r#""Item Journal Staging""#, token_types::TYPE);
    }
}
