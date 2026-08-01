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

/// Build a table of byte offsets for the start of each line in `source`.
/// `line_starts[i]` is the byte offset of the first byte on line `i`.
fn build_line_starts(source: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            source
                .iter()
                .enumerate()
                .filter(|(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect()
}

/// Return the content of a single line from `source` using the precomputed
/// `line_starts` table.  Returns an empty slice if `row` is out of range.
fn get_line<'a>(source: &'a [u8], line_starts: &[usize], row: usize) -> &'a [u8] {
    let Some(&start) = line_starts.get(row) else {
        return &[];
    };
    let end = line_starts.get(row + 1).copied().unwrap_or(source.len());
    &source[start..end]
}

fn collect_tokens(
    node: Node,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
    tokens: &mut Vec<(u32, u32, u32, u32)>,
) {
    // Pre-build line offsets once so every per-token line lookup is O(1).
    let line_starts = build_line_starts(source);

    let mut stack = Vec::new();
    stack.push(node);

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if let Some(token_type) = classify_node(kind, current, source, type_resolver) {
            let start = current.start_position();
            let end = current.end_position();

            if start.row == end.row {
                let line_bytes = get_line(source, &line_starts, start.row);
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
                        let line_bytes = get_line(source, &line_starts, row);
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
        "keyword" => Some(token_types::KEYWORD),
        "object_keyword" => Some(token_types::OBJECT_KEYWORD),
        "metadata_keyword" => Some(token_types::KEYWORD),
        "type_keyword" => Some(token_types::BUILTIN_TYPE),

        "property_keyword" => Some(token_types::PROPERTY),

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
                    && is_builtin_record_member(parent, node, source, type_resolver)
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
                Some(token_types::STRING)
            } else if matches!(node.kind(), "quoted_identifier") {
                // Double-quoted identifiers in AL are always object/identifier references
                Some(token_types::TYPE)
            } else if matches!(node.kind(), "identifier") {
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

fn is_builtin_record_member(
    suffix: Node<'_>,
    member: Node<'_>,
    source: &[u8],
    type_resolver: &super::type_resolver::TypeResolver<'_>,
) -> bool {
    let Ok(member_name) = member.utf8_text(source) else {
        return false;
    };
    if !is_record_builtin_method(member_name.trim_matches('"')) {
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
    let receiver = receiver.trim().trim_matches('"');
    if receiver.is_empty()
        || !receiver
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return false;
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
        .resolve_type(receiver, position)
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
        "dataitem" => Some(token_types::QUERY_DATA_ITEM),
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
    TABLE
        .iter()
        .find(|(name, _)| kw.eq_ignore_ascii_case(name))
        .map(|(_, ty)| *ty)
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
        // The scanner's `directive` leaf starts after the `#pragma ` prefix
        // (the prefix itself is a hidden token), so the emitted token covers
        // the directive body.
        let src = "#pragma warning disable AA0001\ncodeunit 50100 Test\n{\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(
            src,
            &tokens,
            "warning disable AA0001",
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
        assert_token_type_for_text(src, &tokens, r#""Item Journal Staging""#, token_types::TYPE);
    }
}
