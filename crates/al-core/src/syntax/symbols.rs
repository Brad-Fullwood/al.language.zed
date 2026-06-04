//! Document symbol extraction from tree-sitter trees.

use super::ts_range_to_syntax as ts_range_to_lsp;
use super::types::{
    SyntaxDocumentSymbol as DocumentSymbol, SyntaxRange, SyntaxSymbolKind as SymbolKind,
};
use tracing::debug;
use tree_sitter::{Node, Tree};

/// Extract document symbols for the outline view.
///
/// Produces a hierarchical symbol tree:
/// - Top-level object (table, page, codeunit, etc.)
///   - Procedures and triggers
///   - Fields (for tables)
///   - Controls and actions (for pages)
///   - Data items (for reports)
///   - Keys
///   - Enum values
pub fn extract_document_symbols(tree: &Tree, text: &str) -> Vec<DocumentSymbol> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "object_declaration" => {
                if let Some(sym) = extract_object_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "namespace_or_using_declaration" => {
                if let Some(sym) = extract_namespace_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
    }

    debug!(total = symbols.len(), "extract_document_symbols: complete");
    symbols
}

/// Convert a symbol kind string (as stored in object_types.json) to a
/// `SyntaxSymbolKind` value.
///
/// This is an infrastructure mapping — not AL language knowledge.
fn lsp_symbol_kind_from_str(s: &str) -> SymbolKind {
    match s {
        "File" => SymbolKind::File,
        "Module" => SymbolKind::Module,
        "Namespace" => SymbolKind::Namespace,
        "Class" => SymbolKind::Class,
        "Struct" => SymbolKind::Struct,
        "Interface" => SymbolKind::Interface,
        "Enum" => SymbolKind::Enum,
        _ => SymbolKind::Object,
    }
}

/// Map an AL object kind node (e.g. "kw_table") to an LSP SymbolKind.
///
/// Uses language_data::object_types() so new AL object types are picked up
/// without any code changes here.
fn object_kind_to_symbol_kind(kind: &str) -> SymbolKind {
    super::language_data::object_types()
        .iter()
        .find(|ot| ot.node_kind == kind)
        .map(|ot| lsp_symbol_kind_from_str(&ot.lsp_symbol_kind))
        .unwrap_or(SymbolKind::Object)
}

/// Extract the object kind as a human-readable (lowercase) string.
///
/// Uses language_data::object_types() so new AL object types are handled
/// without any code changes here.
fn object_kind_display(kind: &str) -> String {
    super::language_data::object_types()
        .iter()
        .find(|ot| ot.node_kind == kind)
        .map(|ot| ot.keyword.clone())
        .unwrap_or_else(|| {
            // Strip the kw_ prefix for unknown kinds as a best-effort fallback.
            kind.strip_prefix("kw_").unwrap_or(kind).to_string()
        })
}

fn extract_object_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kind_node = node.child_by_field_name("kind")?;
    let kind_str = kind_node.kind();
    let sym_kind = object_kind_to_symbol_kind(kind_str);

    // Grammar doesn't assign a field name to the object name;
    // use the shared extract_object_name helper.
    let name = super::extract_object_name(node, source).unwrap_or_else(|| "(unnamed)".to_string());
    // Also track the name node range for the selection_range below.
    let name_node_range = {
        let mut found_range = None;
        let mut obj_cursor = node.walk();
        for c in node.children(&mut obj_cursor) {
            if matches!(
                c.kind(),
                "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
            ) {
                if let Ok(n) = c.utf8_text(source) {
                    let trimmed = n.trim_matches('"').trim();
                    if !trimmed.is_empty() {
                        found_range = Some(c.range());
                        break;
                    }
                }
            }
        }
        found_range
    };

    let id_text = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let kind_display = object_kind_display(kind_str);
    let detail = if !id_text.is_empty() {
        Some(format!("{} {}", kind_display, id_text))
    } else {
        Some(kind_display)
    };

    let range = ts_range_to_lsp(&node.range(), source);

    // Selection range is the name node or kind node
    let selection_range = name_node_range
        .map(|r| ts_range_to_lsp(&r, source))
        .unwrap_or(ts_range_to_lsp(&kind_node.range(), source));

    // Extract children from the object body
    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name,
        detail,
        kind: sym_kind,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

fn extract_namespace_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unknown)")
        .to_string();

    let keyword = node
        .child_by_field_name("keyword")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("using");

    let range = ts_range_to_lsp(&node.range(), source);

    Some(DocumentSymbol {
        name,
        detail: Some(keyword.to_string()),
        kind: SymbolKind::Namespace,
        range,
        selection_range: range,
        children: None,
    })
}

/// Extract children symbols from an object body.
fn extract_body_children(body: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "procedure_declaration" | "event_procedure_declaration" => {
                if let Some(sym) = extract_procedure_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "trigger_declaration" => {
                if let Some(sym) = extract_trigger_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "event_declaration" => {
                if let Some(sym) = extract_event_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "object_section" => {
                if let Some(sym) = extract_section_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "enum_value_declaration" => {
                if let Some(sym) = extract_enum_value_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "key_declaration" => {
                if let Some(sym) = extract_key_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "object_var_section" | "var_section" => {
                extract_var_section_children(child, source, symbols);
            }
            "variable_declaration" | "label_declaration" | "object_variable_declaration" => {
                collect_var_symbols_recursive(child, source, symbols);
            }
            _ => {}
        }
    }
}

/// Shared helper: build a `DocumentSymbol` for any named node (procedure, trigger, event).
///
/// `kind` is the LSP `SymbolKind`.  `detail` is the optional detail string shown in the outline.
fn extract_named_symbol(
    node: Node,
    source: &[u8],
    kind: SymbolKind,
    detail: Option<String>,
) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail,
        kind,
        range,
        selection_range,
        children: None,
    })
}

fn extract_procedure_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"');

    if name == "(unnamed)" {
        debug!(
            node_kind = node.kind(),
            line = node.start_position().row,
            "extract_procedure_symbol: unnamed procedure (possible grammar issue)"
        );
    }

    let params = node
        .child_by_field_name("parameters")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("()");

    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source).ok());

    let detail = if let Some(rt) = return_type {
        Some(format!("{}: {}", params, rt))
    } else {
        Some(params.to_string())
    };

    extract_named_symbol(node, source, SymbolKind::Function, detail)
}

fn extract_trigger_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    extract_named_symbol(node, source, SymbolKind::Event, Some("trigger".to_string()))
}

fn extract_event_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    extract_named_symbol(node, source, SymbolKind::Event, Some("event".to_string()))
}

fn extract_section_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let keyword_node = node.child_by_field_name("keyword")?;
    let keyword = keyword_node
        .utf8_text(source)
        .ok()
        .unwrap_or("section")
        .to_string();

    // The grammar parses enum `value(N; "Name") { }` as an object_section with
    // keyword="value". Detect this and emit an ENUM_MEMBER symbol instead of a
    // generic section symbol.
    if keyword.eq_ignore_ascii_case("value") {
        return extract_enum_value_from_section(node, source);
    }

    // Report/query `dataitem(Name; "SourceTable") { }` is also an object_section
    // (the grammar's key_declaration rule only matches `key(...)` in tables).
    // The dataitem name is the first identifier in the parenthesized block.
    if keyword.eq_ignore_ascii_case("dataitem") {
        return extract_dataitem_from_section(node, source);
    }

    // Table `field(ID; "Name"; Type) { }` — extract the field name from the
    // parenthesized block.  The name is the first identifier/string that appears
    // after a semicolon (i.e. position 2 in the triplet: ID ; Name ; Type).
    if keyword.eq_ignore_ascii_case("field") {
        if let Some(paren) = find_parenthesized_block(node) {
            let name = extract_field_name_from_paren(paren, source);
            let range = ts_range_to_lsp(&node.range(), source);
            let selection_range = ts_range_to_lsp(&paren.range(), source);
            let mut children = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                extract_section_body_children(body, source, &mut children);
            }
            return Some(DocumentSymbol {
                name,
                detail: Some("field".to_string()),
                kind: SymbolKind::Field,
                range,
                selection_range,
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
            });
        }
    }

    // AL object section-keyword → outline symbol-kind mapping.
    //
    // These are top-level grammar fixtures (sections inside `table`, `page`,
    // `report`, etc.). They are stable AL grammar forms — the set is fixed by
    // the AL grammar revision Microsoft ships, not BC release-to-release. The
    // grammar JSON corpus does not currently have a `section_keywords.json`
    // file; when one is added, replace this with
    // `language_data::section_kind_by_keyword(keyword)`. A new section that
    // doesn't yet appear here falls through to `Namespace` which is the
    // correct generic outline kind — symbols are still extracted, just under
    // a generic label.
    let sym_kind = match keyword.to_lowercase().as_str() {
        "fields" => SymbolKind::Struct,
        "keys" => SymbolKind::Key,
        "actions" => SymbolKind::Namespace,
        "layout" => SymbolKind::Namespace,
        "views" => SymbolKind::Namespace,
        "dataset" => SymbolKind::Namespace,
        "requestpage" => SymbolKind::Class,
        "rendering" => SymbolKind::Namespace,
        "fieldgroups" => SymbolKind::Struct,
        _ => SymbolKind::Namespace,
    };

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = ts_range_to_lsp(&keyword_node.range(), source);

    // Extract children from section body
    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        extract_section_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name: keyword,
        detail: None,
        kind: sym_kind,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Extract an enum value symbol from an `object_section` node whose keyword is "value".
///
/// The grammar parses `value(N; "Name") { ... }` as:
///   object_section
///     keyword("value")
///     parenthesized_block("(N; "Name")")
///     braced_block("{ ... }")
///
/// The name is the last identifier/quoted_identifier in the parenthesized block.
/// The ordinal is the integer before the semicolon.
fn extract_enum_value_from_section(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let range = ts_range_to_lsp(&node.range(), source);

    // Walk the node's children to find the parenthesized_block
    let mut paren_node = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parenthesized_block" {
            paren_node = Some(child);
            break;
        }
    }

    let paren = paren_node?;

    // Walk paren children: expect integer, semicolon, then name
    let mut ordinal = String::new();
    let mut name = String::new();
    let mut name_node_range = paren.range();
    let mut paren_cursor = paren.walk();
    for child in paren.children(&mut paren_cursor) {
        match child.kind() {
            "integer" if ordinal.is_empty() => {
                ordinal = child.utf8_text(source).unwrap_or("").to_string();
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Ok(text) = child.utf8_text(source) {
                    let trimmed = text.trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        name = trimmed;
                        name_node_range = child.range();
                    }
                }
            }
            _ => {}
        }
    }

    if name.is_empty() {
        return None;
    }

    let detail = if ordinal.is_empty() {
        Some("value".to_string())
    } else {
        Some(format!("value({})", ordinal))
    };

    Some(DocumentSymbol {
        name,
        detail,
        kind: SymbolKind::EnumMember,
        range,
        selection_range: ts_range_to_lsp(&name_node_range, source),
        children: None,
    })
}

/// Extract a report/query dataitem symbol from an `object_section` node whose keyword is "dataitem".
///
/// Grammar shape: `dataitem(StagingRec; "Item Journal Staging") { ... }`
///
/// The grammar parses this as:
///   object_section
///     metadata_keyword("dataitem")
///     parenthesized_block("(StagingRec; \"Item Journal Staging\")")
///     object_body("{ ... }")
///
/// Children of the parenthesized_block: `(`, identifier(name), `;`, quoted_identifier(source), `)`.
/// We take the first identifier/quoted_identifier as the dataitem name and recurse into the
/// body for nested triggers/sections.
fn extract_dataitem_from_section(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let range = ts_range_to_lsp(&node.range(), source);

    let mut paren_node = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parenthesized_block" {
            paren_node = Some(child);
            break;
        }
    }

    let paren = paren_node?;

    let mut name = String::new();
    let mut name_node_range = paren.range();
    let mut paren_cursor = paren.walk();
    for child in paren.children(&mut paren_cursor) {
        if matches!(
            child.kind(),
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
        ) {
            if let Ok(text) = child.utf8_text(source) {
                name = text.trim_matches('"').trim().to_string();
                name_node_range = child.range();
                break;
            }
        }
    }

    if name.is_empty() {
        return None;
    }

    let mut children = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        // F-OPEN-102: extract_section_body_children now folds in the
        // raw-trigger pass inline (was a second full sweep).
        extract_section_body_children(body, source, &mut children);
    }

    Some(DocumentSymbol {
        name,
        detail: Some("dataitem".to_string()),
        kind: SymbolKind::Class,
        range,
        selection_range: ts_range_to_lsp(&name_node_range, source),
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Map a page control keyword's `lsp_symbol_kind` string (from page_controls.json) to a
/// tower-lsp [`SymbolKind`].
fn control_keyword_to_symbol_kind(keyword: &str) -> SymbolKind {
    match super::language_data::page_control_by_keyword(keyword)
        .map(|e| e.lsp_symbol_kind.as_str())
        .unwrap_or("Namespace")
    {
        "Field" => SymbolKind::Field,
        "Event" => SymbolKind::Event,
        "Struct" => SymbolKind::Struct,
        "Class" => SymbolKind::Class,
        "Constant" => SymbolKind::Constant,
        _ => SymbolKind::Namespace,
    }
}

/// Extract children from braced blocks within sections (fields, keys, etc.)
///
/// Page controls in the grammar appear as sibling sequences:
///   metadata_keyword ("area") + parenthesized_block ("(Content)") + braced_block ("{ ... }")
/// Uses next_sibling() for zero-allocation look-ahead instead of collecting all children.
fn extract_section_body_children(body: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let mut cursor = body.walk();
    if !cursor.goto_first_child() {
        return;
    }

    loop {
        let child = cursor.node();
        match child.kind() {
            "object_section" => {
                if let Some(sym) = extract_section_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "procedure_declaration" | "event_procedure_declaration" => {
                if let Some(sym) = extract_procedure_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "trigger_declaration" => {
                if let Some(sym) = extract_trigger_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "key_declaration" => {
                // key_declaration covers both table keys (keyword="key") and
                // report/query dataitems (keyword="dataitem"). Dataitems need
                // their body braced_block scanned for raw trigger tokens.
                if is_dataitem_key_declaration(child, source) {
                    if let Some(sym) = extract_dataitem_symbol(child, source) {
                        symbols.push(sym);
                    }
                } else if let Some(sym) = extract_key_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "enum_value_declaration" => {
                if let Some(sym) = extract_enum_value_symbol(child, source) {
                    symbols.push(sym);
                }
            }
            "metadata_keyword" => {
                if let Some(sym) = try_extract_page_control(child, source) {
                    // Skip siblings consumed by the page control (paren + braced_block)
                    loop {
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                        if cursor.node().kind() == "braced_block" {
                            // consumed the body — advance past it
                            break;
                        }
                    }
                    symbols.push(sym);
                }
            }
            "braced_block" => {
                extract_section_body_children(child, source, symbols);
            }
            // F-OPEN-102: fold the trigger-extraction pass into this walk
            // instead of running `extract_triggers_from_braced_block` as a
            // second full sweep. Raw `trigger OnFoo()` patterns inside
            // dataitem/action bodies have a `control_keyword` parent (text
            // "trigger") followed by an identifier + parenthesized_block.
            // Handle inline here; the standalone function is retained for
            // callers that walk a single braced_block in isolation
            // (extract_dataitem_symbol body scan).
            "control_keyword" => {
                if let Some(sym) = try_extract_inline_trigger(child, source) {
                    symbols.push(sym);
                }
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Try to extract a "trigger Name()" symbol starting at a `control_keyword`
/// node. Returns None when the node isn't the "trigger" keyword or the
/// expected siblings aren't present. Closes F-OPEN-102.
fn try_extract_inline_trigger(kw_node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let text = kw_node.utf8_text(source).ok()?;
    if !text.eq_ignore_ascii_case("trigger") {
        return None;
    }
    let name_node = kw_node.next_sibling()?;
    if !matches!(
        name_node.kind(),
        "identifier" | "name" | "name_or_keyword" | "quoted_identifier"
    ) {
        return None;
    }
    let name_text = name_node.utf8_text(source).ok()?;
    let name = name_text.trim_matches('"').to_string();
    if name.is_empty() {
        return None;
    }
    let trigger_kw_range = kw_node.range();
    let range = ts_range_to_lsp(
        &tree_sitter::Range {
            start_byte: trigger_kw_range.start_byte,
            end_byte: name_node.range().end_byte,
            start_point: trigger_kw_range.start_point,
            end_point: name_node.range().end_point,
        },
        source,
    );
    let selection_range = ts_range_to_lsp(&name_node.range(), source);
    Some(DocumentSymbol {
        name,
        detail: Some("trigger".to_string()),
        kind: SymbolKind::Event,
        range,
        selection_range,
        children: None,
    })
}

/// Scan a `braced_block` for trigger declarations that the grammar parses as raw tokens.
///
/// Inside dataitem bodies and some action blocks, `trigger OnPreDataItem()` is not
/// parsed as a `trigger_declaration` node — it appears as:
///   `control_keyword("trigger")` + `identifier("OnPreDataItem")` + `parenthesized_block("()")`
///
/// This function walks the block's children looking for that pattern.
fn extract_triggers_from_braced_block(
    block: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    let mut cursor = block.walk();
    if !cursor.goto_first_child() {
        return;
    }

    loop {
        let child = cursor.node();
        // Look for control_keyword with text "trigger"
        if child.kind() == "control_keyword" {
            if let Ok(text) = child.utf8_text(source) {
                if text.eq_ignore_ascii_case("trigger") {
                    // Next non-punctuation sibling should be the trigger name
                    let trigger_kw_range = child.range();
                    if let Some(name_node) = child.next_sibling() {
                        let name_kind = name_node.kind();
                        // `keyword` is rejected: bare keyword nodes are produced
                        // for AL reserved words used as identifier-like tokens
                        // (e.g. `var`), but they are NOT trigger names — matching
                        // them produced bogus DocumentSymbols. Restrict to true
                        // identifier-like node kinds.
                        if matches!(
                            name_kind,
                            "identifier" | "name" | "name_or_keyword" | "quoted_identifier"
                        ) {
                            if let Ok(name_text) = name_node.utf8_text(source) {
                                let name = name_text.trim_matches('"').to_string();
                                if !name.is_empty() {
                                    let range = ts_range_to_lsp(
                                        &tree_sitter::Range {
                                            start_byte: trigger_kw_range.start_byte,
                                            end_byte: name_node.range().end_byte,
                                            start_point: trigger_kw_range.start_point,
                                            end_point: name_node.range().end_point,
                                        },
                                        source,
                                    );
                                    let selection_range =
                                        ts_range_to_lsp(&name_node.range(), source);
                                    symbols.push(DocumentSymbol {
                                        name,
                                        detail: Some("trigger".to_string()),
                                        kind: SymbolKind::Event,
                                        range,
                                        selection_range,
                                        children: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Try to extract a page control symbol from a metadata_keyword node.
/// Looks ahead at next_sibling() for parenthesized_block and braced_block.
fn try_extract_page_control(kw_node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let kw_text = kw_node.utf8_text(source).ok()?;

    if !super::language_data::is_page_control_keyword(kw_text) {
        return None;
    }

    // Walk next siblings to find parenthesized_block and braced_block
    let mut paren_node = None;
    let mut body_node = None;
    let mut sibling = kw_node.next_sibling();
    while let Some(sib) = sibling {
        match sib.kind() {
            "parenthesized_block" if paren_node.is_none() => paren_node = Some(sib),
            "braced_block" => {
                body_node = Some(sib);
                break;
            }
            "semicolon" => {}
            _ => break,
        }
        sibling = sib.next_sibling();
    }

    // Extract the control name from the parenthesized_block
    let name = if let Some(paren) = paren_node {
        extract_control_name(paren, source)
    } else {
        kw_text.to_string()
    };

    let sym_kind = control_keyword_to_symbol_kind(kw_text);

    // Compute range from keyword start to body end (or paren end if no body)
    let end_node = body_node.or(paren_node).unwrap_or(kw_node);
    let range = ts_range_to_lsp(
        &tree_sitter::Range {
            start_byte: kw_node.start_byte(),
            end_byte: end_node.end_byte(),
            start_point: kw_node.start_position(),
            end_point: end_node.end_position(),
        },
        source,
    );

    let selection_range = paren_node
        .map(|p| ts_range_to_lsp(&p.range(), source))
        .unwrap_or(ts_range_to_lsp(&kw_node.range(), source));

    // Extract children from the body braced_block. F-OPEN-102:
    // extract_section_body_children folds in the raw-trigger pass inline
    // (recognises `trigger OnFoo()` patterns parsed as control_keyword +
    // identifier rather than a trigger_declaration node).
    let mut nested = Vec::new();
    if let Some(body) = body_node {
        extract_section_body_children(body, source, &mut nested);
    }

    Some(DocumentSymbol {
        name,
        detail: Some(kw_text.to_string()),
        kind: sym_kind,
        range,
        selection_range,
        children: if nested.is_empty() {
            None
        } else {
            Some(nested)
        },
    })
}

/// Extract the control name from a parenthesized block like (Content), (Records), ("Entry No."; Rec."Entry No.").
fn extract_control_name(paren: Node, source: &[u8]) -> String {
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Ok(text) = child.utf8_text(source) {
                    return text.trim_matches('"').to_string();
                }
            }
            _ => {}
        }
    }
    // Fallback: show the full paren text without parens
    paren
        .utf8_text(source)
        .map(|t| t.trim_matches(|c| c == '(' || c == ')').trim().to_string())
        .unwrap_or_default()
}

fn extract_enum_value_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let id = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: Some(format!("value({})", id)),
        kind: SymbolKind::EnumMember,
        range,
        selection_range,
        children: None,
    })
}

fn extract_key_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unnamed)")
        .trim_matches('"')
        .to_string();

    let fields = node
        .child_by_field_name("fields")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = node
        .child_by_field_name("name")
        .map(|n| ts_range_to_lsp(&n.range(), source))
        .unwrap_or(range);

    Some(DocumentSymbol {
        name,
        detail: if fields.is_empty() {
            None
        } else {
            Some(fields.to_string())
        },
        kind: SymbolKind::Key,
        range,
        selection_range,
        children: None,
    })
}

/// Return true if this `key_declaration` node represents a report/query dataitem
/// (i.e. its first keyword child has text "dataitem") rather than a table key.
fn is_dataitem_key_declaration(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "keyword" | "metadata_keyword" | "control_keyword"
        ) {
            return child
                .utf8_text(source)
                .map(|t| t.eq_ignore_ascii_case("dataitem"))
                .unwrap_or(false);
        }
    }
    false
}

/// Extract a report/query dataitem symbol from a `key_declaration` node whose keyword is
/// "dataitem".
///
/// The grammar reuses `key_declaration` for dataitems:
///   key_declaration
///     keyword("dataitem")
///     name_or_keyword("StagingRec")
///     semicolon
///     name_or_keyword("\"Item Journal Staging\"")
///     braced_block { ... }
///
/// Unlike table keys, the body braced_block may contain raw trigger tokens
/// (`control_keyword("trigger") identifier("OnPreDataItem") ...`) that are not
/// parsed as `trigger_declaration` nodes.
fn extract_dataitem_symbol(node: Node, source: &[u8]) -> Option<DocumentSymbol> {
    // The dataitem name is the first name_or_keyword / identifier / quoted_identifier
    // child (before the semicolon).
    let mut name = "(unnamed)".to_string();
    let mut name_node_range = node.range();
    let mut seen_semicolon = false;
    {
        let mut c = node.walk();
        for child in node.children(&mut c) {
            match child.kind() {
                "keyword" | "metadata_keyword" | "control_keyword" => {
                    // skip the "dataitem" keyword itself
                }
                "semicolon" => {
                    seen_semicolon = true;
                }
                "(" | ")" => {}
                _ if !seen_semicolon => {
                    // First non-keyword token before the semicolon is the name.
                    // Use index-based child access to avoid iterator borrow issues.
                    let raw = child.utf8_text(source).unwrap_or("");
                    let mut resolved = raw.to_string();
                    // F-OPEN-101: also capture the inner identifier's RANGE
                    // so the selection_range points at just the name, not the
                    // enclosing wrapper (e.g. parenthesized_block). Without
                    // this the outline's "go to definition" target was the
                    // whole `(Name; ...)` block.
                    let mut resolved_range = child.range();
                    for ci in 0..child.child_count() {
                        if let Some(inner) = child.child(ci) {
                            if matches!(inner.kind(), "identifier" | "quoted_identifier" | "name") {
                                if let Ok(t) = inner.utf8_text(source) {
                                    resolved = t.to_string();
                                    resolved_range = inner.range();
                                    break;
                                }
                            }
                        }
                    }
                    let trimmed = resolved.trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        name = trimmed;
                        name_node_range = resolved_range;
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    let range = ts_range_to_lsp(&node.range(), source);
    let selection_range = ts_range_to_lsp(&name_node_range, source);

    // Scan the body braced_block for raw trigger tokens
    let mut children: Vec<DocumentSymbol> = Vec::new();
    {
        let mut c = node.walk();
        for child in node.children(&mut c) {
            if child.kind() == "braced_block" {
                extract_triggers_from_braced_block(child, source, &mut children);
                break;
            }
        }
    }

    Some(DocumentSymbol {
        name,
        detail: Some("dataitem".to_string()),
        kind: SymbolKind::Struct,
        range,
        selection_range,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    })
}

/// Extract variable symbols from a var section.
fn extract_var_section_children(node: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    collect_var_symbols_recursive(node, source, symbols);
    collect_label_symbols_from_text(node, source, symbols);
}

fn collect_var_symbols_recursive(root: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    // Iterative DFS using an explicit stack — avoids stack overflow on deeply nested AL.
    // Start by pushing the children of root (mirroring the original behaviour of iterating
    // root's children and recursing only into unknown-kind nodes).
    let mut stack: Vec<Node> = {
        let mut cursor = root.walk();
        root.children(&mut cursor).collect()
    };
    // Reverse so that popping yields left-to-right order
    stack.reverse();

    while let Some(child) = stack.pop() {
        match child.kind() {
            "regular_variable_declaration" => {
                let detail = extract_node_text(child.child_by_field_name("type"), source);
                let range = ts_range_to_lsp(&child.range(), source);
                for (name, selection_range) in extract_regular_variable_names(child, source) {
                    symbols.push(DocumentSymbol {
                        name,
                        detail: detail.clone(),
                        kind: SymbolKind::Variable,
                        range,
                        selection_range,
                        children: None,
                    });
                }
                // Do not descend into regular_variable_declaration children
            }
            "label_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Some(name) = clean_node_text(name_node, source) {
                        let detail = extract_node_text(child.child_by_field_name("type"), source);
                        symbols.push(DocumentSymbol {
                            name,
                            detail,
                            kind: SymbolKind::Variable,
                            range: ts_range_to_lsp(&child.range(), source),
                            selection_range: ts_range_to_lsp(&name_node.range(), source),
                            children: None,
                        });
                    }
                }
                // Do not descend into label_declaration children
            }
            _ => {
                // Descend into container nodes (variable_declaration wrappers, etc.)
                let mut cursor = child.walk();
                let grandchildren: Vec<Node> = child.children(&mut cursor).collect();
                for gc in grandchildren.into_iter().rev() {
                    stack.push(gc);
                }
            }
        }
    }
}

fn extract_regular_variable_names(node: Node, source: &[u8]) -> Vec<(String, SyntaxRange)> {
    let Some(sep_start) = node.child_by_field_name("sep").map(|sep| sep.start_byte()) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    collect_variable_name_nodes(node, sep_start, source, &mut names);

    if names.is_empty() {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Some(name) = clean_node_text(name_node, source) {
                names.push((name, ts_range_to_lsp(&name_node.range(), source)));
            }
        }
    }

    names
}

fn collect_variable_name_nodes(
    node: Node,
    sep_start: usize,
    source: &[u8],
    names: &mut Vec<(String, SyntaxRange)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.start_byte() >= sep_start {
            continue;
        }

        if current.child_count() == 0 && is_variable_name_node(current.kind()) {
            if let Some(name) = clean_node_text(current, source) {
                names.push((name, ts_range_to_lsp(&current.range(), source)));
            }
            continue;
        }

        // Push children in reverse order so left-to-right children are
        // popped (and thus processed) in their original left-to-right order.
        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
}

fn is_variable_name_node(kind: &str) -> bool {
    // F-OPEN-103: accept any `kw_*` node as an identifier fallback, not just
    // the single `kw_function` we hardcoded before. AL grammar sometimes
    // tokenises identifier-positioned reserved words (e.g. `record`, `query`,
    // `trigger`) as their dedicated `kw_*` nodes when they appear inside
    // `var FunctionRef: Codeunit ...`-style declarations. The pre-fix list
    // silently dropped outline entries for those positions whenever a new
    // grammar release added a `kw_*` not in the literal match list.
    matches!(
        kind,
        "identifier"
            | "quoted_identifier"
            | "keyword"
            | "object_keyword"
            | "metadata_keyword"
            | "property_keyword"
    ) || kind.starts_with("kw_")
}

fn extract_node_text(node: Option<Node>, source: &[u8]) -> Option<String> {
    super::node_text_clean(node?, source)
}

fn clean_node_text(node: Node, source: &[u8]) -> Option<String> {
    super::node_text_clean(node, source)
}

fn collect_label_symbols_from_text(node: Node, source: &[u8], symbols: &mut Vec<DocumentSymbol>) {
    let Ok(section_text) = node.utf8_text(source) else {
        return;
    };

    let mut seen: std::collections::HashSet<String> = symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| symbol.name.to_lowercase())
        .collect();

    for (offset, line) in section_text.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.ends_with(';') {
            continue;
        }
        let Some((name_part, rest)) = trimmed.split_once(':') else {
            continue;
        };
        if !rest.trim_start().starts_with("Label ") && !rest.trim_start().starts_with("Label\t") {
            continue;
        }

        let name = name_part.trim().trim_matches('"').to_string();
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }

        let line_no = node.start_position().row as u32 + offset as u32;
        // F-OPEN-syntax-symbols-1: skip this entry if we can't locate the
        // identifier within the source line. `unwrap_or_default()` was
        // producing a (0, 0) range at column 0 — visually wrong for the
        // outline. Real ASCII identifiers always match; this guard handles
        // the edge case of whitespace-collapsing surprises.
        let Some(start_byte) = line.find(name_part) else {
            continue;
        };
        let start_col = super::byte_col_to_utf16_col(line, start_byte);
        let end_col = super::byte_col_to_utf16_col(line, start_byte + name_part.len());
        symbols.push(DocumentSymbol {
            name,
            detail: Some("Label".to_string()),
            kind: SymbolKind::Variable,
            range: ts_range_to_lsp(&node.range(), source),
            selection_range: SyntaxRange {
                start: super::types::SyntaxPosition {
                    line: line_no,
                    character: start_col,
                },
                end: super::types::SyntaxPosition {
                    line: line_no,
                    character: end_col,
                },
            },
            children: None,
        });
    }
}

/// Find the first `parenthesized_block` child of a node (non-body, non-named-field).
fn find_parenthesized_block(node: Node) -> Option<Node> {
    // `cursor` is bound separately because the lifetime of the borrowed cursor
    // doesn't extend through `find` when chained inline.
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .find(|child| child.kind() == "parenthesized_block");
    result
}

/// Extract a table field name from `(ID; "Name"; Type)`.
///
/// The name is the first identifier/quoted_identifier/string/name that appears
/// after the first semicolon in the parenthesized block.  For a page field like
/// `("Caption"; ...)` (no integer ID before the semicolon) the first identifier
/// is used directly.
fn extract_field_name_from_paren(paren: Node, source: &[u8]) -> String {
    let mut past_first_semicolon = false;
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "semicolon" if !past_first_semicolon => {
                past_first_semicolon = true;
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if past_first_semicolon =>
            {
                if let Ok(text) = child.utf8_text(source) {
                    let trimmed = text.trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        return trimmed;
                    }
                }
            }
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if !past_first_semicolon =>
            {
                // Page field: `field("Caption"; ...)` — no integer before semicolon.
                if let Ok(text) = child.utf8_text(source) {
                    let trimmed = text.trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        return trimmed;
                    }
                }
            }
            _ => {}
        }
    }
    // Fallback: use the full paren text
    paren
        .utf8_text(source)
        .map(|t| {
            t.trim_matches(|c: char| c == '(' || c == ')')
                .trim()
                .to_string()
        })
        .unwrap_or_else(|_| "field".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;

    #[test]
    fn test_extract_symbols_codeunit() {
        let src = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
    end;

    procedure DoAnother(x: Integer): Boolean
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1, "Should have one top-level object");
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Codeunit");
        assert_eq!(obj.kind, SymbolKind::Class);
        let children = obj.children.as_ref().expect("Should have children");
        assert!(
            children.len() >= 2,
            "Should have at least 2 procedures, got {}",
            children.len()
        );
    }

    #[test]
    fn test_extract_symbols_enum() {
        let src = r#"enum 50100 "My Enum"
{
    value(0; "First") { }
    value(1; "Second") { }
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1);
        let obj = &symbols[0];
        assert_eq!(obj.kind, SymbolKind::Enum);
        let children = obj.children.as_ref().expect("Should have enum values");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].kind, SymbolKind::EnumMember);
    }

    #[test]
    fn test_extract_symbols_table() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
    }

    trigger OnInsert()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        assert_eq!(symbols.len(), 1);
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Table");
        assert_eq!(obj.kind, SymbolKind::Class);
    }

    #[test]
    fn test_extract_symbols_global_variables_keep_names_and_types() {
        let src = r#"codeunit 50100 Test
{
    var
        FirstVar, "Second Var": Record "Sales Header" temporary;
        CaptionLbl: Label 'Caption';

    procedure DoIt()
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);
        let obj = &symbols[0];
        let children = obj.children.as_ref().expect("Should have object children");

        let vars: Vec<&DocumentSymbol> = children
            .iter()
            .filter(|child| child.kind == SymbolKind::Variable)
            .collect();

        assert_eq!(vars.len(), 3, "Expected one symbol per declared variable");
        assert_eq!(vars[0].name, "FirstVar");
        assert_eq!(
            vars[0].detail.as_deref(),
            Some(r#"Record "Sales Header" temporary"#)
        );
        assert_eq!(vars[1].name, "Second Var");
        assert_eq!(
            vars[1].detail.as_deref(),
            Some(r#"Record "Sales Header" temporary"#)
        );
        assert_eq!(vars[2].name, "CaptionLbl");
        assert_eq!(vars[2].detail.as_deref(), Some("Label"));
        assert!(vars.iter().all(|var| var.name != "(unnamed)"));
    }

    #[test]
    fn test_extract_symbols_report_dataitem_trigger() {
        let src = r#"report 50200 "IJL Process Staging"
{
    dataset
    {
        dataitem(StagingRec; "Item Journal Staging")
        {
            RequestFilterFields = "Entry No.", Status;

            trigger OnPreDataItem()
            begin
                // body
            end;
        }
    }

    trigger OnPostReport()
    begin
    end;

    procedure SetAction(NewAction: Enum "IJL Process Action")
    begin
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let symbols = extract_document_symbols(&result.tree, src);

        // Collect all names recursively
        fn collect_names(syms: &[DocumentSymbol]) -> Vec<String> {
            let mut names = Vec::new();
            for sym in syms {
                names.push(sym.name.clone());
                if let Some(children) = &sym.children {
                    names.extend(collect_names(children));
                }
            }
            names
        }

        let all_names = collect_names(&symbols);
        assert!(
            all_names.iter().any(|n| n == "OnPreDataItem"),
            "OnPreDataItem trigger should be in document symbols. Got: {:?}",
            all_names
        );
        assert!(
            all_names.iter().any(|n| n == "OnPostReport"),
            "OnPostReport trigger should be in document symbols. Got: {:?}",
            all_names
        );
        assert!(
            all_names.iter().any(|n| n == "StagingRec"),
            "StagingRec dataitem should be in document symbols. Got: {:?}",
            all_names
        );
    }

    // ---- shared helpers for the tests below ----

    /// Recursively collect every symbol name in the tree (depth-first).
    fn collect_names_rec(syms: &[DocumentSymbol]) -> Vec<String> {
        let mut names = Vec::new();
        for sym in syms {
            names.push(sym.name.clone());
            if let Some(children) = &sym.children {
                names.extend(collect_names_rec(children));
            }
        }
        names
    }

    /// Find the first symbol (anywhere in the tree) matching `name`.
    fn find_sym<'a>(syms: &'a [DocumentSymbol], name: &str) -> Option<&'a DocumentSymbol> {
        for sym in syms {
            if sym.name == name {
                return Some(sym);
            }
            if let Some(children) = &sym.children {
                if let Some(found) = find_sym(children, name) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn parse_symbols(src: &str) -> Vec<DocumentSymbol> {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        extract_document_symbols(&result.tree, src)
    }

    #[test]
    fn test_namespace_and_using_declarations() {
        let src = r#"namespace MyApp.Sales;
using System.Utilities;

codeunit 50100 Test { }"#;
        let symbols = parse_symbols(src);

        // namespace + using + the codeunit
        let ns = find_sym(&symbols, "MyApp.Sales").expect("namespace symbol");
        assert_eq!(ns.kind, SymbolKind::Namespace);
        assert_eq!(ns.detail.as_deref(), Some("namespace"));
        // selection_range mirrors range for namespaces (no separate name node range)
        assert_eq!(ns.selection_range, ns.range);
        assert!(ns.children.is_none());

        let using = find_sym(&symbols, "System.Utilities").expect("using symbol");
        assert_eq!(using.kind, SymbolKind::Namespace);
        assert_eq!(using.detail.as_deref(), Some("using"));
    }

    #[test]
    fn test_object_detail_includes_id() {
        let src = r#"codeunit 50100 "My Codeunit" { }"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        // detail is "<keyword> <id>"
        assert_eq!(obj.detail.as_deref(), Some("codeunit 50100"));
        // The selection range must point at the name, not the whole object.
        assert!(obj.selection_range.start.character >= obj.range.start.character);
    }

    #[test]
    fn test_object_without_id_detail_is_keyword_only() {
        // An interface has no numeric ID — detail should be just the keyword.
        let src = r#"interface "My Interface"
{
    procedure Foo()
}"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        assert_eq!(obj.name, "My Interface");
        assert_eq!(obj.kind, SymbolKind::Interface);
        // No numeric id present → detail is the keyword alone.
        assert_eq!(obj.detail.as_deref(), Some("interface"));
    }

    #[test]
    fn test_table_fields_keys_sections_and_field_names() {
        let src = r#"table 50101 "My Table2"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Description; Text[100]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}"#;
        let symbols = parse_symbols(src);
        let obj = &symbols[0];
        let children = obj.children.as_ref().expect("table children");

        let fields_section = children
            .iter()
            .find(|c| c.name == "fields")
            .expect("fields section");
        assert_eq!(fields_section.kind, SymbolKind::Struct);

        let keys_section = children
            .iter()
            .find(|c| c.name == "keys")
            .expect("keys section");
        assert_eq!(keys_section.kind, SymbolKind::Key);

        // Field names extracted from the parenthesized triplet (after first ';').
        let no_field = find_sym(&symbols, "No.").expect("No. field");
        assert_eq!(no_field.kind, SymbolKind::Field);
        assert_eq!(no_field.detail.as_deref(), Some("field"));

        let desc_field = find_sym(&symbols, "Description").expect("Description field");
        assert_eq!(desc_field.kind, SymbolKind::Field);
    }

    #[test]
    fn test_page_field_quoted_name_and_nested_trigger() {
        let src = r#"page 50100 "My Page"
{
    layout
    {
        area(Content)
        {
            field("Customer Name"; Rec."Customer Name")
            {
                ApplicationArea = All;
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(Post)
            {
                trigger OnAction()
                begin
                end;
            }
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let all = collect_names_rec(&symbols);

        // Quoted page-field name is unquoted in the outline.
        assert!(
            all.iter().any(|n| n == "Customer Name"),
            "page field name should be unquoted. Got: {:?}",
            all
        );
        let field = find_sym(&symbols, "Customer Name").unwrap();
        assert_eq!(field.kind, SymbolKind::Field);

        // The OnAction trigger nested inside the action body is captured.
        let trig = find_sym(&symbols, "OnAction").expect("OnAction trigger");
        assert_eq!(trig.kind, SymbolKind::Event);
        assert_eq!(trig.detail.as_deref(), Some("trigger"));
    }

    #[test]
    fn test_procedure_detail_params_and_return_type() {
        let src = r#"codeunit 50100 Test
{
    procedure NoReturn(x: Integer)
    begin
    end;

    procedure WithReturn(a: Integer; b: Code[20]): Boolean
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let no_return = find_sym(&symbols, "NoReturn").expect("NoReturn proc");
        assert_eq!(no_return.kind, SymbolKind::Function);
        // Detail is just the parameter list (no return type).
        let detail = no_return.detail.as_deref().unwrap();
        assert!(detail.contains("Integer"), "got detail {:?}", detail);
        assert!(!detail.contains(':') || detail.starts_with('('));

        let with_return = find_sym(&symbols, "WithReturn").expect("WithReturn proc");
        let detail = with_return.detail.as_deref().unwrap();
        // Return type is appended after ": ".
        assert!(
            detail.ends_with(": Boolean"),
            "return type should be in detail, got {:?}",
            detail
        );
    }

    #[test]
    fn test_enum_value_ordinal_in_detail() {
        let src = r#"enum 50100 "My Enum"
{
    value(0; First) { }
    value(7; "Last One") { }
}"#;
        let symbols = parse_symbols(src);
        let first = find_sym(&symbols, "First").expect("First value");
        assert_eq!(first.kind, SymbolKind::EnumMember);
        assert_eq!(first.detail.as_deref(), Some("value(0)"));

        let last = find_sym(&symbols, "Last One").expect("Last One value");
        assert_eq!(last.detail.as_deref(), Some("value(7)"));
    }

    #[test]
    fn test_report_dataitem_is_class_with_dataitem_detail() {
        let src = r#"report 50200 "My Report"
{
    dataset
    {
        dataitem(MyItem; "Customer")
        {
            column(Name; "Name") { }

            trigger OnAfterGetRecord()
            begin
            end;
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let item = find_sym(&symbols, "MyItem").expect("dataitem symbol");
        assert_eq!(item.detail.as_deref(), Some("dataitem"));
        // Nested trigger captured under the dataitem.
        let all = collect_names_rec(&symbols);
        assert!(
            all.iter().any(|n| n == "OnAfterGetRecord"),
            "dataitem trigger should be captured. Got: {:?}",
            all
        );
    }

    #[test]
    fn test_event_procedure_symbol() {
        // [IntegrationEvent] decorated procedures parse as procedure_declaration;
        // ensure they still surface as a Function symbol with param detail.
        let src = r#"codeunit 50100 Test
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeFoo(var Handled: Boolean)
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let ev = find_sym(&symbols, "OnBeforeFoo").expect("event procedure");
        assert!(matches!(ev.kind, SymbolKind::Function | SymbolKind::Event));
        assert!(ev.detail.as_deref().unwrap().contains("Handled"));
    }

    #[test]
    fn test_label_symbols_extracted_from_var_section() {
        let src = r#"codeunit 50100 Test
{
    var
        ErrMsg: Label 'Something went wrong';
        InfoTxt: Label 'All good';
        RecVar: Record Customer;
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"ErrMsg"), "got {:?}", names);
        assert!(names.contains(&"InfoTxt"), "got {:?}", names);
        assert!(names.contains(&"RecVar"), "got {:?}", names);

        let err = children.iter().find(|c| c.name == "ErrMsg").unwrap();
        assert_eq!(err.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_empty_source_yields_no_symbols() {
        let symbols = parse_symbols("");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_malformed_object_does_not_panic() {
        // Truncated / malformed object: must not panic and must degrade gracefully.
        let src = "codeunit 50100";
        let symbols = parse_symbols(src);
        // Either zero symbols or a best-effort object symbol — never a panic.
        for s in &symbols {
            assert!(!s.name.is_empty() || s.name == "(unnamed)");
        }
    }

    #[test]
    fn test_unnamed_object_fallback_name() {
        // Object with no name token at all → "(unnamed)" fallback.
        let src = "codeunit 50100 { }";
        let symbols = parse_symbols(src);
        if let Some(obj) = symbols.first() {
            // When the parser can't recover a name, the helper substitutes "(unnamed)".
            assert!(!obj.name.is_empty());
        }
    }

    #[test]
    fn test_object_kind_to_symbol_kind_unknown_falls_back_to_object() {
        // A node kind that is not a known AL object type maps to Object.
        assert_eq!(
            object_kind_to_symbol_kind("kw_not_a_real_object"),
            SymbolKind::Object
        );
    }

    #[test]
    fn test_object_kind_display_strips_kw_prefix_for_unknown() {
        // Unknown kinds fall back to stripping the "kw_" prefix.
        assert_eq!(object_kind_display("kw_widget"), "widget");
        // A kind without the prefix is returned unchanged.
        assert_eq!(object_kind_display("widget"), "widget");
    }

    #[test]
    fn test_lsp_symbol_kind_from_str_mapping() {
        assert_eq!(lsp_symbol_kind_from_str("Enum"), SymbolKind::Enum);
        assert_eq!(lsp_symbol_kind_from_str("Interface"), SymbolKind::Interface);
        assert_eq!(lsp_symbol_kind_from_str("Class"), SymbolKind::Class);
        // Unknown strings fall back to Object.
        assert_eq!(lsp_symbol_kind_from_str("Nonsense"), SymbolKind::Object);
    }

    #[test]
    fn test_is_variable_name_node_accepts_kw_prefix() {
        assert!(is_variable_name_node("identifier"));
        assert!(is_variable_name_node("quoted_identifier"));
        // F-OPEN-103: any kw_* node counts as an identifier fallback.
        assert!(is_variable_name_node("kw_record"));
        assert!(is_variable_name_node("kw_anything_at_all"));
        // Non-identifier punctuation does not.
        assert!(!is_variable_name_node("semicolon"));
        assert!(!is_variable_name_node(";"));
    }

    #[test]
    fn test_multiple_top_level_objects_in_one_file() {
        let src = r#"codeunit 50100 First { }

table 50101 Second { fields { } }

enum 50102 Third { value(0; A) { } }"#;
        let symbols = parse_symbols(src);
        assert_eq!(symbols.len(), 3, "three top-level objects");
        assert_eq!(symbols[0].name, "First");
        assert_eq!(symbols[1].name, "Second");
        assert_eq!(symbols[2].name, "Third");
        assert_eq!(symbols[2].kind, SymbolKind::Enum);
    }

    #[test]
    fn test_section_keyword_kind_mapping() {
        // Exercise the section-keyword -> SymbolKind map (extract_section_symbol).
        // Each of these keywords parses as an `object_section` whose outline
        // kind is fixed by the keyword, not by AL release.
        let src = r#"page 50100 "P"
{
    layout { area(Content) { } }
    views { view(MyView) { } }
    actions { area(Processing) { } }
}
table 50101 "T2" { fieldgroups { fieldgroup(DropDown; "No.") { } } }
report 50102 "R2" { rendering { layout(L) { } } requestpage { layout { } } dataset { } }"#;
        let symbols = parse_symbols(src);

        // Helper that returns the kind of the first section named `name`.
        fn kind_of(syms: &[DocumentSymbol], name: &str) -> Option<SymbolKind> {
            for s in syms {
                if s.name == name {
                    return Some(s.kind);
                }
                if let Some(children) = &s.children {
                    if let Some(k) = kind_of(children, name) {
                        return Some(k);
                    }
                }
            }
            None
        }

        assert_eq!(kind_of(&symbols, "layout"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "views"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "actions"), Some(SymbolKind::Namespace));
        assert_eq!(kind_of(&symbols, "fieldgroups"), Some(SymbolKind::Struct));
        assert_eq!(kind_of(&symbols, "rendering"), Some(SymbolKind::Namespace));
        // requestpage maps to Class — a distinct mapping arm, not the generic
        // Namespace fallback, so this guards the specific branch.
        assert_eq!(kind_of(&symbols, "requestpage"), Some(SymbolKind::Class));
        assert_eq!(kind_of(&symbols, "dataset"), Some(SymbolKind::Namespace));
    }

    #[test]
    fn test_table_field_nested_trigger_is_child_event() {
        // A `trigger OnValidate()` inside a table field's braced body is parsed
        // as a raw `control_keyword("trigger")` + identifier, surfaced via
        // try_extract_inline_trigger and nested under the field symbol.
        let src = r#"table 50100 "T"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
            begin
            end;
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let field = find_sym(&symbols, "No.").expect("No. field");
        assert_eq!(field.kind, SymbolKind::Field);
        let field_children = field
            .children
            .as_ref()
            .expect("field should have its nested trigger as a child");
        let trig = field_children
            .iter()
            .find(|c| c.name == "OnValidate")
            .expect("OnValidate trigger nested under field");
        assert_eq!(trig.kind, SymbolKind::Event);
        assert_eq!(trig.detail.as_deref(), Some("trigger"));
        // The selection range must point at the name token, which starts after
        // the keyword column — proves try_extract_inline_trigger picked the
        // name node (not the whole keyword span) for selection.
        assert!(trig.selection_range.start.character > trig.range.start.character);
    }

    #[test]
    fn test_page_field_without_integer_id_uses_first_identifier() {
        // A page field `field("Caption"; Rec.Foo)` has no integer ID before the
        // first semicolon. extract_field_name_from_paren must fall through to
        // the "no-semicolon-yet" identifier branch and use the first token.
        let src = r#"page 50100 "P"
{
    layout
    {
        area(Content)
        {
            field("Caption"; Rec.Foo) { }
        }
    }
}"#;
        let symbols = parse_symbols(src);
        let field = find_sym(&symbols, "Caption").expect("Caption field");
        assert_eq!(field.kind, SymbolKind::Field);
        assert_eq!(field.detail.as_deref(), Some("field"));
    }

    #[test]
    fn test_control_keyword_to_symbol_kind_mapping() {
        // Direct unit test of the page-control keyword -> SymbolKind map.
        // Keywords come from page_controls.json (loaded at runtime), so this
        // exercises the real lookup + each match arm.
        assert_eq!(control_keyword_to_symbol_kind("area"), SymbolKind::Struct);
        assert_eq!(control_keyword_to_symbol_kind("field"), SymbolKind::Field);
        assert_eq!(control_keyword_to_symbol_kind("part"), SymbolKind::Class);
        assert_eq!(control_keyword_to_symbol_kind("action"), SymbolKind::Event);
        assert_eq!(
            control_keyword_to_symbol_kind("label"),
            SymbolKind::Constant
        );
        // An unknown keyword falls back to Namespace.
        assert_eq!(
            control_keyword_to_symbol_kind("definitely_not_a_control"),
            SymbolKind::Namespace
        );
    }

    #[test]
    fn test_event_decorated_procedure_surfaces_with_param_detail() {
        // A [BusinessEvent]-decorated procedure still surfaces as a symbol with
        // parameter detail (procedure / event extraction path).
        let src = r#"codeunit 50100 "Pub"
{
    [BusinessEvent(false)]
    procedure OnSomethingHappened(Sender: Integer)
    begin
    end;
}"#;
        let symbols = parse_symbols(src);
        let ev = find_sym(&symbols, "OnSomethingHappened").expect("event procedure");
        assert!(matches!(ev.kind, SymbolKind::Function | SymbolKind::Event));
        assert!(ev.detail.as_deref().unwrap().contains("Sender"));
    }

    #[test]
    fn test_multiple_vars_share_type_each_get_own_symbol() {
        // `Alpha, Beta, Gamma: Integer;` must yield three Variable symbols, each
        // carrying the shared type as detail (multi-name var extraction path).
        let src = r#"codeunit 50100 Test
{
    var
        Alpha, Beta, Gamma: Integer;
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let vars: Vec<&DocumentSymbol> = children
            .iter()
            .filter(|c| c.kind == SymbolKind::Variable)
            .collect();
        assert_eq!(
            vars.len(),
            3,
            "one symbol per declared name, got {:?}",
            vars
        );
        for v in &vars {
            assert_eq!(v.detail.as_deref(), Some("Integer"));
        }
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"Alpha"));
        assert!(names.contains(&"Beta"));
        assert!(names.contains(&"Gamma"));
    }

    #[test]
    fn test_label_not_duplicated_when_already_a_variable() {
        // collect_label_symbols_from_text dedupes against existing Variable
        // symbols (case-insensitive). A label parsed by the grammar must not be
        // emitted a second time by the text-scan fallback.
        let src = r#"codeunit 50100 Test
{
    var
        GreetingLbl: Label 'Hello';
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let count = children.iter().filter(|c| c.name == "GreetingLbl").count();
        assert_eq!(count, 1, "label must appear exactly once, not duplicated");
    }
}
