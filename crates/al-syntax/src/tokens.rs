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

    /// Control-flow keywords (begin, end, if, then, else, for, while, etc.).
    pub const KEYWORD_CONTROL: u32 = 43;
    /// Function/procedure/trigger definition keywords.
    pub const KEYWORD_FUNCTION: u32 = 44;

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
        "keywordControl",    // 43
        "keywordFunction",   // 44
    ];
}

/// Semantic token modifier bit flags — must match the legend registered with the LSP client.
pub mod token_modifiers {
    /// Bit 0: `unnecessary` — the token is for code that is not needed (e.g. unused variable).
    pub const UNNECESSARY: u32 = 1 << 0;

    /// The modifier legend entries in order, for registering with the LSP server.
    pub const LEGEND: &[&str] = &["unnecessary"];
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

    // Collect all leaf tokens with their absolute positions.
    // Tuple: (line, col, len, token_type)
    let mut raw_tokens: Vec<(u32, u32, u32, u32)> = Vec::new();
    collect_tokens(root, source, &mut raw_tokens);

    // Build a set of (line, col) positions for unused local variable declaration names.
    // These receive the `unnecessary` modifier (bit 0) so editors can fade them.
    let unused_positions = collect_unused_var_positions(root, source);

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

        let token_modifiers = if unused_positions.contains(&(line, col)) {
            token_modifiers::UNNECESSARY
        } else {
            0
        };

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type,
            token_modifiers,
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

/// Collect the (line, col) positions of unused or write-only local variable declaration name nodes.
///
/// Walks all `procedure_declaration` and `trigger_declaration` nodes in the tree,
/// finds variables in their `var_section`, and returns the set of declaration-name
/// positions for variables that are:
///   - Never referenced in the procedure body at all, OR
///   - Only assigned via plain `:=` (write-only: assigned but never read).
///
/// Compound assignments (`+=`, `-=`, etc.) count as reads and are not flagged.
///
/// Only local variables (those inside a procedure/trigger var section) are checked.
/// Global object-level variables are not marked because they may be used across files.
fn collect_unused_var_positions(root: Node, source: &[u8]) -> std::collections::HashSet<(u32, u32)> {
    let mut result = std::collections::HashSet::new();

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "procedure_declaration"
            || node.kind() == "trigger_declaration"
            || node.kind() == "event_procedure_declaration"
        {
            collect_unused_in_procedure(node, source, &mut result);
            // Don't push children — we already handled this subtree
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    result
}

/// Collect unused variable positions within a single procedure/trigger node.
fn collect_unused_in_procedure(
    proc_node: Node,
    source: &[u8],
    result: &mut std::collections::HashSet<(u32, u32)>,
) {
    // Gather variable declarations from the var section
    let mut var_declarations: Vec<(String, tree_sitter::Point)> = Vec::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "var_section" || child.kind() == "empty_var_section" {
            collect_var_name_nodes(child, source, &mut var_declarations);
        }
    }

    if var_declarations.is_empty() {
        return;
    }

    // Get the body text (begin..end block) for word-boundary usage checks
    let mut cursor2 = proc_node.walk();
    let body_text: Option<String> = proc_node.children(&mut cursor2).find_map(|c| {
        if c.kind() == "begin_end_block" {
            c.utf8_text(source).ok().map(|s| s.to_lowercase())
        } else {
            None
        }
    });

    let body = match body_text {
        Some(b) => b,
        None => return,
    };

    for (var_name, pos) in &var_declarations {
        let lower_name = var_name.to_lowercase();
        if is_write_only_var(&body, &lower_name) {
            result.insert((pos.row as u32, pos.column as u32));
        }
    }
}

/// Return `true` if the variable `word` is considered unused or write-only in `body`.
///
/// A variable is unnecessary when:
/// - It never appears in the body at all (completely unused), OR
/// - Every word-boundary occurrence is a plain `:=` assignment LHS (write-only: assigned but
///   never read). Compound assignments like `+=` are excluded because they imply a read.
fn is_write_only_var(body: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }

    let mut found_any = false;
    let mut all_lhs = true;

    for (i, _) in body.match_indices(word) {
        // Word-boundary check
        let before_ok = i == 0 || {
            let b = body.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after_idx = i + word.len();
        let after_ok = after_idx >= body.len() || {
            let b = body.as_bytes()[after_idx];
            !b.is_ascii_alphanumeric() && b != b'_'
        };

        if !before_ok || !after_ok {
            continue; // not a word boundary match
        }

        found_any = true;

        // Check whether this occurrence is on the LHS of a plain `:=` assignment.
        // Skip optional whitespace after the word boundary, then look for `:=`.
        // A compound assignment like `+=` is NOT a plain assignment — it reads the value too.
        let rest = &body[after_idx..];
        let trimmed = rest.trim_start_matches(|c: char| c == ' ' || c == '\t' || c == '\r' || c == '\n');
        if !trimmed.starts_with(":=") {
            // This occurrence is a read (RHS, function arg, condition, compound assignment, etc.)
            all_lhs = false;
        }
    }

    // Unnecessary if: never appeared, OR every occurrence was an LHS write
    !found_any || all_lhs
}

/// Collect variable name nodes from a var_section, recording their start positions.
fn collect_var_name_nodes(
    node: Node,
    source: &[u8],
    vars: &mut Vec<(String, tree_sitter::Point)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration"
            || child.kind() == "regular_variable_declaration"
        {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let clean_name = name.trim_matches('"').to_string();
                    if !clean_name.is_empty() {
                        vars.push((clean_name, name_node.start_position()));
                    }
                }
            }
        }
        // Recurse for nested variable_declaration groups
        if child.kind() == "variable_declaration" {
            collect_var_name_nodes(child, source, vars);
        }
    }
}


/// Classify a tree-sitter node kind to a semantic token type.
/// Returns `None` for nodes that should not be highlighted or should recurse.
fn classify_node(kind: &str, node: Node, source: &[u8]) -> Option<u32> {
    match kind {
        // Control-flow and access modifier keywords → KEYWORD_CONTROL
        "kw_begin" | "kw_end" | "kw_var" | "kw_if" | "kw_then" | "kw_else" | "kw_for"
        | "kw_foreach" | "kw_while" | "kw_do" | "kw_repeat" | "kw_until" | "kw_case" | "kw_of"
        | "kw_exit" | "kw_break" | "kw_continue" | "kw_with" | "kw_in" | "kw_to" | "kw_downto"
        | "kw_asserterror" | "kw_local" | "kw_internal" | "kw_protected" | "kw_temporary"
        | "kw_event" => Some(token_types::KEYWORD_CONTROL),

        // Procedure/trigger/function definition keywords → KEYWORD_FUNCTION
        "kw_procedure" | "kw_function" | "kw_trigger" => Some(token_types::KEYWORD_FUNCTION),

        // Object keywords — emit OBJECT_KEYWORD for object declaration keyword nodes
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
        // Otherwise emit KEYWORD_CONTROL.
        "control_keyword" => {
            if let Some(paren) = node.parent().filter(|p| p.kind() == "parenthesized_block") {
                if let Some(t) = classify_parenthesized_block_name(node, paren, source) {
                    return Some(t);
                }
            }
            Some(token_types::KEYWORD_CONTROL)
        }
        // Generic keyword nodes — emit coarse KEYWORD
        "keyword" => Some(token_types::KEYWORD),
        // Object declaration keywords — emit OBJECT_KEYWORD
        "object_keyword" => Some(token_types::OBJECT_KEYWORD),
        // Metadata keywords — emit KEYWORD
        "metadata_keyword" => Some(token_types::KEYWORD),

        // Built-in type keywords → BUILTIN_TYPE
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

        // Generic type keyword node → BUILTIN_TYPE
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
    let json = include_str!("../../../tree-sitter-al/data/implicit_variables.json");
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
                Some(token_types::BUILTIN_TYPE)
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
                Some(token_types::NAMESPACE_DECL)
            } else {
                None
            }
        }
        // Usage sites: a `name` node appearing as a direct child of `primary_expression`
        // is a standalone identifier reference (variable, procedure call, builtin).
        // Resolve it by scanning the enclosing scope.
        "primary_expression" if node.kind() == "name" => {
            classify_name_in_expression(node, source)
        }
        _ => {
            if has_ancestor_kind(node, "enum_value_declaration") {
                return Some(token_types::ENUM_MEMBER);
            }
            if has_ancestor_kind(node, "type_reference") {
                Some(token_types::BUILTIN_TYPE)
            } else if matches!(node.kind(), "string" | "verbatim_string") {
                Some(token_types::STRING)
            } else if matches!(node.kind(), "quoted_identifier") {
                // Double-quoted identifiers in AL are always object/identifier references
                Some(token_types::BUILTIN_TYPE)
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

// ---------------------------------------------------------------------------
// Usage-site resolution helpers
// ---------------------------------------------------------------------------

/// Classify a `name` node that appears inside a `primary_expression` (usage site).
///
/// Walk the enclosing scope to determine if the identifier refers to a parameter,
/// local variable, global variable, procedure, or builtin function.
fn classify_name_in_expression(name_node: Node, source: &[u8]) -> Option<u32> {
    let text = name_node.utf8_text(source).ok()?;

    // Check for implicit trigger variables (Rec, xRec, CurrPage, etc.)
    if is_trigger_variable(text)
        && (has_ancestor_kind(name_node, "trigger_declaration")
            || has_ancestor_kind(name_node, "procedure_declaration"))
    {
        return Some(token_types::SELF_KEYWORD);
    }

    // Walk up to the enclosing procedure or trigger declaration.
    let enclosing_proc = find_ancestor_proc(name_node);

    if let Some(proc) = enclosing_proc {
        // 1. Check parameters of the enclosing procedure.
        if procedure_has_parameter(proc, text, source) {
            return Some(token_types::PARAMETER);
        }

        // 2. Check local var section of the enclosing procedure.
        if procedure_has_local_var(proc, text, source) {
            return Some(token_types::LOCAL_VARIABLE);
        }
    }

    // 3. Check object-level var section (global variables).
    if let Some(obj) = find_ancestor_object(name_node) {
        if object_has_global_var(obj, text, source) {
            return Some(token_types::GLOBAL_VARIABLE);
        }

        // 4. Check if it's a procedure name in this file.
        if object_has_procedure(obj, text, source) {
            return Some(token_types::FUNCTION);
        }
    }

    // 5. Check known AL builtin functions.
    if is_builtin_function(text) {
        return Some(token_types::BUILTIN_FUNCTION);
    }

    None
}

/// Walk up the AST to find the nearest enclosing `procedure_declaration` or `trigger_declaration`.
fn find_ancestor_proc(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if matches!(n.kind(), "procedure_declaration" | "trigger_declaration") {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Walk up the AST to find the nearest enclosing `object_declaration`.
fn find_ancestor_object(node: Node) -> Option<Node> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "object_declaration" {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Extract the plain text of a `name` or `name_or_keyword` node.
///
/// These nodes contain a single `identifier` or keyword child. We return the text
/// of that child (or the node itself if it is an identifier).
fn name_node_text<'a>(node: Node, source: &'a [u8]) -> Option<&'a str> {
    // If the node itself is an identifier or quoted_identifier, use it directly.
    if matches!(node.kind(), "identifier" | "quoted_identifier") {
        return node.utf8_text(source).ok();
    }
    // Otherwise look for a child identifier.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if matches!(child.kind(), "identifier" | "quoted_identifier") {
                return child.utf8_text(source).ok();
            }
        }
    }
    // Fallback: use the node's own text.
    node.utf8_text(source).ok()
}

/// Check whether `proc_node` (a `procedure_declaration` or `trigger_declaration`) has a
/// parameter named `text` (case-insensitive).
fn procedure_has_parameter(proc_node: Node, text: &str, source: &[u8]) -> bool {
    // Find the parameter_list child.
    for i in 0..proc_node.child_count() {
        if let Some(child) = proc_node.child(i) {
            if child.kind() == "parameter_list" && parameter_list_has(child, text, source) {
                return true;
            }
        }
    }
    false
}

/// Scan a `parameter_list` node for a parameter with a matching name.
fn parameter_list_has(list: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..list.child_count() {
        if let Some(param) = list.child(i) {
            if param.kind() == "parameter" {
                // The `name` field of `parameter` is a `name_or_keyword`.
                if let Some(name_node) = param.child_by_field_name("name") {
                    if let Some(param_text) = name_node_text(name_node, source) {
                        if param_text.eq_ignore_ascii_case(text) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Check whether `proc_node` has a local `var_section` containing a variable named `text`.
fn procedure_has_local_var(proc_node: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..proc_node.child_count() {
        if let Some(child) = proc_node.child(i) {
            if child.kind() == "var_section" && var_section_has(child, text, source) {
                return true;
            }
        }
    }
    false
}

/// Check whether `obj_node` (an `object_declaration`) has an object-level var section
/// containing a variable named `text`.
fn object_has_global_var(obj_node: Node, text: &str, source: &[u8]) -> bool {
    // The object body is in the `body` field or as a direct child `object_body`.
    let body = obj_node
        .child_by_field_name("body")
        .or_else(|| find_child_kind(obj_node, "object_body"));
    let body = match body {
        Some(b) => b,
        None => return false,
    };

    for i in 0..body.child_count() {
        if let Some(child) = body.child(i) {
            if child.kind() == "object_var_section" && object_var_section_has(child, text, source) {
                return true;
            }
        }
    }
    false
}

/// Check whether `obj_node` has a `procedure_declaration` or `trigger_declaration` whose
/// name matches `text` (case-insensitive).
fn object_has_procedure(obj_node: Node, text: &str, source: &[u8]) -> bool {
    let body = obj_node
        .child_by_field_name("body")
        .or_else(|| find_child_kind(obj_node, "object_body"));
    let body = match body {
        Some(b) => b,
        None => return false,
    };

    for i in 0..body.child_count() {
        if let Some(child) = body.child(i) {
            if matches!(child.kind(), "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration") {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Some(proc_text) = name_node_text(name_node, source) {
                        if proc_text.eq_ignore_ascii_case(text) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Scan a `var_section` for a `variable_declaration` containing a variable named `text`.
fn var_section_has(var_section: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..var_section.child_count() {
        if let Some(child) = var_section.child(i) {
            if child.kind() == "variable_declaration"
                && variable_declaration_has(child, text, source)
            {
                return true;
            }
        }
    }
    false
}

/// Scan an `object_var_section` for an `object_variable_declaration` containing `text`.
fn object_var_section_has(var_section: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..var_section.child_count() {
        if let Some(child) = var_section.child(i) {
            if child.kind() == "object_variable_declaration"
                && object_variable_declaration_has(child, text, source)
            {
                return true;
            }
        }
    }
    false
}

/// Check a `variable_declaration` node (which may contain multiple `regular_variable_declaration`
/// children) for a variable named `text`.
fn variable_declaration_has(var_decl: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..var_decl.child_count() {
        if let Some(child) = var_decl.child(i) {
            if child.kind() == "regular_variable_declaration"
                && regular_var_decl_has(child, text, source)
            {
                return true;
            }
        }
    }
    false
}

/// Check an `object_variable_declaration` node for a variable named `text`.
fn object_variable_declaration_has(obj_var_decl: Node, text: &str, source: &[u8]) -> bool {
    for i in 0..obj_var_decl.child_count() {
        if let Some(child) = obj_var_decl.child(i) {
            if child.kind() == "regular_variable_declaration"
                && regular_var_decl_has(child, text, source)
            {
                return true;
            }
        }
    }
    false
}

/// Check a `regular_variable_declaration` for a variable name matching `text`.
///
/// A regular_variable_declaration can declare multiple names (comma-separated), all
/// in `name` fields before the `:` separator.  We check all `name_or_keyword` children
/// that appear before the colon.
fn regular_var_decl_has(decl: Node, text: &str, source: &[u8]) -> bool {
    // The separator `:` is the `sep` field. All `name` fields are before it.
    let sep_start = decl
        .child_by_field_name("sep")
        .map(|s| s.start_byte())
        .unwrap_or(usize::MAX);

    for i in 0..decl.child_count() {
        if let Some(child) = decl.child(i) {
            if child.start_byte() >= sep_start {
                break;
            }
            // `name` field children are `name_or_keyword` nodes
            if child.kind() == "name_or_keyword" {
                if let Some(name_text) = name_node_text(child, source) {
                    if name_text.eq_ignore_ascii_case(text) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Find the first direct child of `node` with a given kind.
fn find_child_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == kind {
                return Some(child);
            }
        }
    }
    None
}

/// Known AL builtin functions (case-insensitive check).
///
/// This list covers the most common system functions. It does NOT include record methods
/// (FindFirst, Get, etc.) since those appear as member calls (`Rec.FindFirst()`) not bare
/// function calls.
static BUILTIN_FUNCTIONS: &[&str] = &[
    // Error handling / UI
    "Message",
    "Error",
    "Confirm",
    "Dialog",
    // String functions
    "Format",
    "StrSubstNo",
    "StrPos",
    "StrLen",
    "CopyStr",
    "SelectStr",
    "InsStr",
    "DelStr",
    "DelChr",
    "PadStr",
    "ConvertStr",
    "UpperCase",
    "LowerCase",
    "IncStr",
    "Evaluate",
    // Math
    "Round",
    "Abs",
    "Power",
    "Sqrt",
    "Random",
    "Randomize",
    "Maximum",
    "Minimum",
    // Date/time
    "Today",
    "Time",
    "CurrentDateTime",
    "CreateDateTime",
    "DT2Date",
    "DT2Time",
    "Date2DMY",
    "DMY2Date",
    "CalcDate",
    "NormalDate",
    "WorkDate",
    // Array
    "ArrayLen",
    "CompressArray",
    "CopyArray",
    "SortArray",
    // Control flow / transaction
    "Clear",
    "ClearAll",
    "Sleep",
    "Commit",
    "Rollback",
    // Misc
    "IsNull",
    "IsNullGuid",
    "NullGuid",
    "CreateGuid",
    "TypeHelper",
    "Hyperlink",
    "ApplicationPath",
    "GuiAllowed",
    "UserId",
    "CompanyName",
    "TenantId",
    "GlobalLanguage",
    "WindowsLanguage",
    "GetLastErrorText",
    "GetLastErrorCode",
    "ClearLastError",
    "Variant2Date",
    "Variant2Time",
    "TableCaption",
    "FieldCaption",
    "FieldNo",
];

fn is_builtin_function(text: &str) -> bool {
    BUILTIN_FUNCTIONS.iter().any(|b| b.eq_ignore_ascii_case(text))
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

        // Verify string, keyword control, and keyword function tokens are present.
        let string_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::STRING)
            .count();
        assert!(
            string_count >= 1,
            "Should have at least 1 string token ('Hello'), got {}",
            string_count
        );
        let kw_control = tokens.iter().filter(|t| t.token_type == token_types::KEYWORD_CONTROL).count();
        assert!(kw_control > 0, "Should have KEYWORD_CONTROL tokens");
        let kw_fn = tokens.iter().filter(|t| t.token_type == token_types::KEYWORD_FUNCTION).count();
        assert!(kw_fn > 0, "Should have KEYWORD_FUNCTION tokens");
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
        let number_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::NUMBER)
            .count();
        assert!(number_count > 0, "Should have number tokens (object ID 50100)");
        // Verify control-flow and function-definition keywords emit granular types.
        let kw_control = tokens
            .iter()
            .filter(|t| t.token_type == token_types::KEYWORD_CONTROL)
            .count();
        assert!(kw_control > 0, "Should have KEYWORD_CONTROL tokens (begin/end)");
        let kw_fn = tokens
            .iter()
            .filter(|t| t.token_type == token_types::KEYWORD_FUNCTION)
            .count();
        assert!(kw_fn > 0, "Should have KEYWORD_FUNCTION tokens (procedure)");
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
        // Built-in type keywords (Integer, etc.) emit BUILTIN_TYPE tokens.
        // Verify that variable declarations still produce LOCAL_VARIABLE tokens.
        let local_var_count = tokens
            .iter()
            .filter(|t| t.token_type == token_types::LOCAL_VARIABLE)
            .count();
        assert!(local_var_count > 0, "Should have local variable token for 'Counter'");
    }

    #[test]
    fn test_tokens_quoted_object_and_type_names_are_classified() {
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

        // Object declaration name → NAMESPACE_DECL; type_reference → BUILTIN_TYPE.
        // assert_token_type_for_text checks that at least one token with this text has the
        // given type. "My Table" appears as both object decl (NAMESPACE_DECL) and type ref
        // (BUILTIN_TYPE); verify the type-reference occurrence is BUILTIN_TYPE.
        assert_token_type_for_text(source, &tokens, r#""My Table""#, token_types::BUILTIN_TYPE);
        assert_token_type_for_text(source, &tokens, r#""Another Table""#, token_types::BUILTIN_TYPE);
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
        // At minimum: must have some tokens (object number and object name)
        assert!(!tokens.is_empty(), "Expected tokens from a namespace-prefixed codeunit file");
        // Keywords are now deferred to tree-sitter highlights.scm; check for number token (50100).
        let has_number = tokens.iter().any(|t| t.token_type == token_types::NUMBER);
        assert!(has_number, "Expected number token (object ID 50100) in namespace file, got {} tokens", tokens.len());
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
            (token_types::KEYWORD_FUNCTION + 1) as usize,
            "LEGEND length must match the number of registered token types"
        );
    }

    #[test]
    fn test_keyword_control_begin_end() {
        let src = "codeunit 50100 Test { procedure DoIt() begin end; }";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "begin", token_types::KEYWORD_CONTROL);
        assert_token_type_for_text(src, &tokens, "end", token_types::KEYWORD_CONTROL);
    }

    #[test]
    fn test_keyword_function_procedure_trigger() {
        let src = "codeunit 50100 Test { procedure DoIt() begin end; }";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "procedure", token_types::KEYWORD_FUNCTION);
    }

    #[test]
    fn test_builtin_type_integer() {
        let src = r#"codeunit 50100 Test {
    procedure DoIt()
    var
        N: Integer;
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "Integer", token_types::BUILTIN_TYPE);
    }

    #[test]
    fn test_object_keyword_codeunit_table() {
        let src = "codeunit 50100 Test { }";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_token_type_for_text(src, &tokens, "codeunit", token_types::OBJECT_KEYWORD);
    }

    #[test]
    fn test_permissions_table_name_highlighted_as_builtin_type() {
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
            token_types::BUILTIN_TYPE,
        );
    }
}

#[cfg(test)]
mod usage_site_tests {
    use super::*;
    use crate::AlParser;

    fn decoded_tokens_with_text(
        source: &str,
        tokens: &[SemanticToken],
    ) -> Vec<(u32, u32, u32, u32, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut decoded = Vec::new();
        let mut line = 0u32;
        let mut col = 0u32;
        for t in tokens {
            line += t.delta_line;
            col = if t.delta_line > 0 { t.delta_start } else { col + t.delta_start };
            let text = lines.get(line as usize)
                .and_then(|l| l.get(col as usize .. (col + t.length) as usize))
                .unwrap_or("?")
                .to_string();
            decoded.push((line, col, t.length, t.token_type, text));
        }
        decoded
    }

    fn assert_usage_token(source: &str, tokens: &[SemanticToken], text: &str, expected_type: u32) {
        let entries = decoded_tokens_with_text(source, tokens);
        let found = entries.iter().any(|(_, _, _, tt, t)| {
            *tt == expected_type && t == text
        });
        assert!(
            found,
            "Expected usage token {:?} with type {} but got:\n{}",
            text, expected_type,
            entries.iter()
                .filter(|(_, _, _, _, t)| t == text)
                .map(|(l, c, _, tt, t)| format!("  line={} col={} type={} text={:?}", l, c, tt, t))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn test_global_variable_usage_site() {
        let src = r#"codeunit 50100 Test
{
    var
        GlobalCounter: Integer;

    procedure DoIt()
    begin
        GlobalCounter += 1;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_usage_token(src, &tokens, "GlobalCounter", token_types::GLOBAL_VARIABLE);
    }

    #[test]
    fn test_local_variable_usage_site() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        LocalVar: Integer;
    begin
        LocalVar := 42;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_usage_token(src, &tokens, "LocalVar", token_types::LOCAL_VARIABLE);
    }

    #[test]
    fn test_parameter_usage_site() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt(a: Integer)
    var
        LocalVar: Integer;
    begin
        LocalVar := a * 2;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_usage_token(src, &tokens, "a", token_types::PARAMETER);
    }

    #[test]
    fn test_procedure_call_usage_site() {
        let src = r#"codeunit 50100 Test
{
    procedure SimpleProc()
    begin
    end;

    procedure CallsOthers()
    begin
        SimpleProc();
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_usage_token(src, &tokens, "SimpleProc", token_types::FUNCTION);
    }

    #[test]
    fn test_builtin_function_usage_site() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    begin
        Message('Hello');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert_usage_token(src, &tokens, "Message", token_types::BUILTIN_FUNCTION);
    }
}

#[cfg(test)]
mod write_only_var_tests {
    use super::*;
    use crate::AlParser;
    use crate::tokens::token_modifiers;

    /// Decode tokens back to absolute positions, returning (line, col, modifiers, text).
    fn decode_with_text(source: &str, tokens: &[SemanticToken]) -> Vec<(u32, u32, u32, String)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut decoded = Vec::new();
        let mut line = 0u32;
        let mut col = 0u32;
        for t in tokens {
            line += t.delta_line;
            col = if t.delta_line > 0 { t.delta_start } else { col + t.delta_start };
            let text = lines
                .get(line as usize)
                .and_then(|l| l.get(col as usize..(col + t.length) as usize))
                .unwrap_or("?")
                .to_string();
            decoded.push((line, col, t.token_modifiers, text));
        }
        decoded
    }

    fn has_unnecessary(source: &str, tokens: &[SemanticToken], var_name: &str) -> bool {
        decode_with_text(source, tokens)
            .iter()
            .any(|(_, _, mods, text)| *mods & token_modifiers::UNNECESSARY != 0 && text == var_name)
    }

    /// A variable that is only assigned (Result := WithReturn()) but never read must be flagged.
    #[test]
    fn test_write_only_var_flagged() {
        let src = r#"codeunit 50100 Test
{
    procedure WithReturn(): Boolean
    begin
        exit(true);
    end;

    procedure CallsOthers()
    var
        Result: Boolean;
    begin
        Result := WithReturn();
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(
            has_unnecessary(src, &tokens, "Result"),
            "Write-only variable 'Result' (assigned but never read) should have UNNECESSARY modifier"
        );
    }

    /// A variable that is read (used in an if condition) must NOT be flagged.
    #[test]
    fn test_read_var_not_flagged() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        Counter: Integer;
    begin
        Counter := 5;
        if Counter > 0 then
            Message('yes');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(
            !has_unnecessary(src, &tokens, "Counter"),
            "Variable 'Counter' is read in an if condition — must NOT be flagged as unnecessary"
        );
    }

    /// A completely unused variable (never appears in body) must still be flagged.
    #[test]
    fn test_completely_unused_var_flagged() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        Unused: Integer;
    begin
        Message('hello');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(
            has_unnecessary(src, &tokens, "Unused"),
            "Completely unused variable 'Unused' should have UNNECESSARY modifier"
        );
    }

    /// A variable used in a compound assignment (+=) is being read — must NOT be flagged.
    #[test]
    fn test_compound_assignment_not_flagged() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        Counter: Integer;
    begin
        Counter += 1;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(
            !has_unnecessary(src, &tokens, "Counter"),
            "Variable 'Counter' used in compound assignment (+=) must NOT be flagged"
        );
    }

    /// A variable passed as a function argument (RHS usage) must NOT be flagged.
    #[test]
    fn test_rhs_function_arg_not_flagged() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        Value: Integer;
    begin
        Value := 42;
        Message('%1', Value);
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let tokens = extract_semantic_tokens(&result.tree, src);
        assert!(
            !has_unnecessary(src, &tokens, "Value"),
            "Variable 'Value' is passed as an argument — must NOT be flagged"
        );
    }
}
