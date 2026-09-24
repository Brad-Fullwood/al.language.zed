//! Symbol entries (fields, properties, methods, parameters) read from a
//! parse tree.

use super::*;

/// Extract `FieldSymbol` data from table/tableextension field sections.
///
/// Fields parse as `object_section` nodes with keyword `field` and a
/// parenthesized `(ID; Name; Type)` triplet. Used by the workspace
/// enrichment pass so scaffolding (`generate page --table`) works against
/// the user's own tables.
pub(super) fn extract_fields_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::FieldSymbol> {
    let mut fields = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_section" {
            if let Some(kw) = node.child_by_field_name("keyword") {
                if kw
                    .utf8_text(source)
                    .is_ok_and(|t| t.eq_ignore_ascii_case("field"))
                {
                    if let Some(f) = field_symbol_from_section(node, source) {
                        fields.push(f);
                    }
                    // Field bodies hold properties/triggers, not nested fields.
                    continue;
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    // The explicit stack visits siblings in reverse order; sort for stable,
    // declaration-order output.
    fields.sort_by_key(|f| f.id);
    fields
}

/// Extract the `extends` target from an object declaration, if any.
///
/// The grammar emits `extends X` either as `object_modifier` (with
/// `modifier`/`target` fields) or — what real headers actually produce —
/// as `implements_clause` (positional `metadata_keyword` + `name` children,
/// shared between `implements` and `extends`). Needed so workspace extension
/// objects participate in composition (`composed table <base>`) once
/// registered in the SymbolIndex.
/// Object-level `property_assignment` entries (`SourceTable`, `PageType`,
/// `Permissions`, …) for one workspace object.
///
/// Table-impact analysis reads `SourceTable` off the symbol entry, and without
/// this pass workspace pages carried none, so a table that a workspace page is
/// built on reported no consumers at all.
pub(super) fn extract_object_properties_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::PropertyValue> {
    let mut properties = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "object_body" => stack.push(child),
                "property_assignment" => {
                    let (Some(name), Some(value)) = (
                        child
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(source).ok()),
                        child
                            .child_by_field_name("value")
                            .and_then(|n| n.utf8_text(source).ok()),
                    ) else {
                        continue;
                    };
                    properties.push(al_symbols::PropertyValue {
                        name: name.trim().to_string(),
                        value: value.trim().trim_end_matches(';').trim().to_string(),
                    });
                }
                // Only the object's own header and body level matter; a
                // property inside a page control or a table field belongs to
                // that member, not to the object.
                "source_file" | "object_declaration" => stack.push(child),
                _ => {}
            }
        }
    }
    properties
}

pub(super) fn info_extends_from_tree(root: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "object_modifier" | "implements_clause") {
            let mut kw_cursor = node.walk();
            let keyword_node = node.child_by_field_name("modifier").or_else(|| {
                node.children(&mut kw_cursor)
                    .find(|c| c.kind() == "metadata_keyword")
            });
            let keyword = keyword_node
                .and_then(|m| m.utf8_text(source).ok())
                .unwrap_or("");
            if keyword.trim().eq_ignore_ascii_case("extends") {
                let mut tgt_cursor = node.walk();
                let target_node = node
                    .child_by_field_name("target")
                    .or_else(|| node.children(&mut tgt_cursor).find(|c| c.kind() == "name"));
                return target_node
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(|t| t.unquote_identifier().into_owned());
            }
        }
        // The extends clause lives in the object header — don't descend into
        // object bodies (procedure code can't contain these clause nodes).
        if node.kind() != "object_body" {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

/// Extract the interface names from an object's `implements` clause.
///
/// The grammar emits only the *first* interface inside `implements_clause`
/// (`metadata_keyword` + `name`); any further comma-separated interfaces appear
/// as sibling `identifier`/`quoted_identifier` tokens of the
/// `object_declaration`. We collect both. Names are returned unquoted.
///
/// `object_name` selects the matching object when a file declares more than one
/// (falls back to the first object). Mirrors [`info_extends_from_tree`]; kept
/// separate because `extends` and `implements` share the `implements_clause`
/// node but carry different keywords.
pub(super) fn info_implements_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
    object_name: &str,
) -> Vec<String> {
    let want = object_name.unquote_identifier().to_lowercase();
    let mut cursor = root.walk();
    let objects: Vec<tree_sitter::Node> = root
        .children(&mut cursor)
        .filter(|c| c.kind() == "object_declaration")
        .collect();

    let chosen = objects
        .iter()
        .find(|o| {
            object_decl_name(**o, source)
                .map(|n| n.to_lowercase() == want)
                .unwrap_or(false)
        })
        .or_else(|| objects.first());

    match chosen {
        Some(obj) => collect_implements_from_object(*obj, source),
        None => Vec::new(),
    }
}

/// Best-effort name of an `object_declaration` node (unquoted).
pub(super) fn object_decl_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // The object name is reliably on the `name:` field
    // `(name_or_keyword (name (identifier | quoted_identifier)))` for every
    // object kind, so read it directly.
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.unquote_identifier().into_owned())
}

pub(super) fn collect_implements_from_object(
    node: tree_sitter::Node,
    source: &[u8],
) -> Vec<String> {
    let mut result = Vec::new();
    let mut cursor = node.walk();
    let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();

    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        if child.kind() == "implements_clause" {
            let mut kc = child.walk();
            let keyword = child
                .children(&mut kc)
                .find(|c| c.kind() == "metadata_keyword")
                .and_then(|m| m.utf8_text(source).ok())
                .unwrap_or("");
            if keyword.trim().eq_ignore_ascii_case("implements") {
                // The single name inside the clause...
                let mut nc = child.walk();
                if let Some(name_node) = child.children(&mut nc).find(|c| {
                    matches!(
                        c.kind(),
                        "name" | "name_or_keyword" | "quoted_identifier" | "identifier"
                    )
                }) {
                    if let Ok(t) = name_node.utf8_text(source) {
                        push_interface(&mut result, t);
                    }
                }
                // Include any trailing `, IBar` siblings the grammar leaves at
                // the object_declaration level.
                let mut j = i + 1;
                while j < children.len() {
                    match children[j].kind() {
                        "comma" => j += 1,
                        "identifier" | "quoted_identifier" | "name" | "name_or_keyword" => {
                            if let Ok(t) = children[j].utf8_text(source) {
                                push_interface(&mut result, t);
                            }
                            j += 1;
                        }
                        _ => break,
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    result
}

pub(super) fn push_interface(result: &mut Vec<String>, raw: &str) {
    let clean = raw.unquote_identifier().into_owned();
    if !clean.is_empty() {
        result.push(clean);
    }
}

/// Parse one `field(ID; Name; Type)` section header into a `FieldSymbol`.
pub(super) fn field_symbol_from_section(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::FieldSymbol> {
    let mut cursor = node.walk();
    let paren = node
        .children(&mut cursor)
        .find(|c| c.kind() == "parenthesized_block")?;
    let text = paren.utf8_text(source).ok()?;
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.splitn(3, ';');
    let id: i32 = parts.next()?.trim().parse().ok()?;
    let name = parts.next()?.unquote_identifier().into_owned();
    let type_name = parts.next()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(al_symbols::FieldSymbol {
        id,
        name,
        type_name,
        properties: Vec::new(),
    })
}

pub(super) fn extract_methods_from_tree(
    root: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::MethodSymbol> {
    let mut methods = Vec::new();
    collect_methods_recursive(root, source, &mut methods);
    methods
}

pub(super) fn collect_methods_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    methods: &mut Vec<al_symbols::MethodSymbol>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_declaration" | "trigger_declaration" => {
                if let Some(method) = extract_method_symbol(node, source) {
                    methods.push(method);
                }
                // Do not recurse into procedure body
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
    }
}

pub(super) fn extract_method_symbol(
    proc_node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::MethodSymbol> {
    let name_node = proc_node.child_by_field_name("name")?;
    let proc_name = name_node
        .utf8_text(source)
        .ok()?
        .unquote_identifier()
        .to_string();
    if proc_name.is_empty() {
        return None;
    }

    let is_local = has_local_modifier(proc_node, source);
    let attributes = collect_procedure_attributes(proc_node, source);
    let al_attrs: Vec<al_symbols::AttributeSymbol> = attributes
        .iter()
        .map(|(name, args_text)| {
            let arguments = parse_attr_args_from_text(args_text);
            al_symbols::AttributeSymbol {
                name: name.clone(),
                arguments,
            }
        })
        .collect();

    let parameters = extract_parameters_from_proc(proc_node, source);

    let return_type = extract_return_type(proc_node, source);

    Some(al_symbols::MethodSymbol {
        name: proc_name,
        parameters,
        return_type,
        attributes: al_attrs,
        is_local,
    })
}

pub(super) fn extract_parameters_from_proc(
    proc_node: tree_sitter::Node,
    source: &[u8],
) -> Vec<al_symbols::ParameterSymbol> {
    let mut params = Vec::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "parameter_list" {
            let mut inner = child.walk();
            for param_node in child.children(&mut inner) {
                if param_node.kind() == "parameter" {
                    if let Some(p) = extract_single_parameter(param_node, source) {
                        params.push(p);
                    }
                }
            }
            break;
        }
    }
    params
}

pub(super) fn extract_single_parameter(
    param_node: tree_sitter::Node,
    source: &[u8],
) -> Option<al_symbols::ParameterSymbol> {
    let mut name: Option<String> = None;
    let mut type_name = String::new();
    let mut is_var = false;

    let mut cursor = param_node.walk();
    for child in param_node.children(&mut cursor) {
        let kind = child.kind();
        if kind.starts_with("kw_var") || kind == "kw_var" {
            is_var = true;
        } else if (kind == "name" || kind == "name_or_keyword") && name.is_none() {
            name = child
                .utf8_text(source)
                .ok()
                .map(|s| s.unquote_identifier().into_owned());
        } else if kind == "type_reference" {
            type_name = child.utf8_text(source).ok().unwrap_or("").to_string();
        }
    }

    Some(al_symbols::ParameterSymbol {
        name: name?,
        type_name,
        is_var,
    })
}

/// Extract return type from a procedure declaration.
///
/// Relies on the AL grammar putting the parameter list ahead of the return
/// type as named children — by the time a `type_reference` named child
/// appears, parameter `type_reference` nodes have already been consumed via
/// the `parameter_list` parent. The previous comment ("Check if preceded by
/// `:`") was aspirational and not implemented; the grammar's child ordering
/// makes that check unnecessary in practice.
pub(super) fn extract_return_type(proc_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // `return_type` is a *field* on procedure_declaration, not a node kind; its
    // value is a `type_reference`. The direct-child scan stays as a fallback
    // for a procedure the parser recovered without the field, and is safe
    // because parameter type references are nested under `parameter_list`.
    if let Some(field) = proc_node.child_by_field_name("return_type") {
        let text = field.utf8_text(source).ok()?.trim().to_string();
        if !text.is_empty() {
            return Some(text);
        }
    }
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "type_reference" {
            let text = child.utf8_text(source).ok()?.trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Parse attribute arguments from the raw attribute text.
/// Input: `[IntegrationEvent(false, false)]` → `["false", "false"]`
pub(super) fn parse_attr_args_from_text(attr_text: &str) -> Vec<String> {
    let start = match attr_text.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let end = match attr_text.rfind(')') {
        Some(i) => i,
        None => return Vec::new(),
    };
    if start >= end {
        return Vec::new();
    }
    let inner = &attr_text[start..end];
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
