//! The record and object variables a procedure declares, by name.

use super::*;

/// Extract a mapping of `lowercase_variable_name -> table_name` for all
/// `Record "X"` variables in a procedure's `var` section and parameters.
///
/// Returns only `Record`-typed variables since those are what trigger table events.
pub fn extract_procedure_var_types(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> HashMap<String, String> {
    let source_bytes = source.as_bytes();
    let mut result = HashMap::new();

    let proc_node = find_procedure_node(tree, source_bytes, procedure_name);
    let Some(proc_node) = proc_node else {
        return result;
    };

    collect_record_vars_from_procedure_node(proc_node, source_bytes, &mut result);

    result
}

/// [`extract_procedure_var_types`] for a declaration node the caller already
/// holds.
///
/// AL repeats declaration names constantly — every field has its own
/// `trigger OnValidate()`, every page action its own `trigger OnAction()`. A
/// name-keyed lookup answers for the first one only, so a caller that must
/// visit every declaration walks the declarations itself and passes each node
/// here.
pub fn procedure_var_types_in_node(
    proc_node: tree_sitter::Node<'_>,
    source: &str,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    collect_record_vars_from_procedure_node(proc_node, source.as_bytes(), &mut result);
    result
}

/// Every `procedure_declaration`, `trigger_declaration` and
/// `event_procedure_declaration` node in the tree, in no particular order.
///
/// Unlike a name-keyed lookup this returns repeated names separately, so a
/// write inside the second `OnValidate` of a table is visible.
pub fn collect_declaration_nodes<'a>(tree: &'a tree_sitter::Tree) -> Vec<tree_sitter::Node<'a>> {
    let mut declarations = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration" => {
                declarations.push(node);
                // AL has no nested procedures, so the body holds no more.
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
    }
    declarations
}

pub(super) fn find_procedure_node<'a>(
    tree: &'a tree_sitter::Tree,
    source: &[u8],
    procedure_name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let proc_name_lower = procedure_name.to_lowercase();
    let root = tree.root_node();

    find_procedure_in_node(root, source, &proc_name_lower)
}

pub(super) fn find_procedure_in_node<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    proc_name_lower: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "procedure_declaration" || kind == "trigger_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let clean = name_text.unquote_identifier();
                    if clean.to_lowercase() == proc_name_lower {
                        return Some(node);
                    }
                }
            }
            // Do not descend further into this procedure's body
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    None
}

pub(super) fn collect_record_vars_from_procedure_node(
    proc_node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        match child.kind() {
            "var_section" => {
                collect_record_vars_from_var_section(child, source, result);
            }
            "parameter_list" => {
                collect_record_vars_from_parameter_list(child, source, result);
            }
            _ => {}
        }
    }
}

pub(super) fn collect_record_vars_from_var_section(
    section: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = section.walk();
    for child in section.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            collect_record_from_variable_declaration(child, source, result);
        }
    }
}

pub(super) fn collect_record_vars_from_parameter_list(
    param_list: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() == "parameter" {
            collect_record_from_parameter(child, source, result);
        }
    }
}

pub(super) fn collect_record_from_variable_declaration(
    container: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = container.walk();
    for child in container.children(&mut cursor) {
        if child.kind() == "regular_variable_declaration" {
            collect_record_from_regular_var_decl(child, source, result);
        }
    }
    if container.kind() == "regular_variable_declaration" {
        collect_record_from_regular_var_decl(container, source, result);
    }
}

pub(super) fn collect_record_from_regular_var_decl(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };

    let name = match name_node.utf8_text(source).ok() {
        Some(t) => t.unquote_identifier().into_owned(),
        None => return,
    };

    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    if type_kw.eq_ignore_ascii_case("record") {
        if let Some(table_name) = subtype {
            result.insert(name.to_lowercase(), table_name);
        }
    }
}

pub(super) fn collect_record_from_parameter(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };

    let name = match name_node.utf8_text(source).ok() {
        Some(t) => t.unquote_identifier().into_owned(),
        None => return,
    };

    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    if type_kw.eq_ignore_ascii_case("record") {
        if let Some(table_name) = subtype {
            result.insert(name.to_lowercase(), table_name);
        }
    }
}

/// Extract object-typed (codeunit / page / report / xmlport / query /
/// interface) variable declarations from a procedure's `var` section and
/// parameters. Returns a map of lowercase var-name → object name.
///
/// Companion to `extract_procedure_var_types` (which handles only `Record`);
/// used by member-call resolution to translate `MyVar.Method()` →
/// `<ObjectName>.Method()` when the variable's declared type is an object
/// reference. Excludes `Record` because those don't act
/// as method-call receivers in the same sense (their methods live on the
/// table object, but the call-graph already routes those via the
/// `RecordOp` trigger path).
pub fn extract_procedure_object_var_types(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> HashMap<String, String> {
    let source_bytes = source.as_bytes();
    let mut result = HashMap::new();

    let Some(proc_node) = find_procedure_node(tree, source_bytes, procedure_name) else {
        return result;
    };
    result.extend(procedure_object_var_types_in_node(proc_node, source));
    result
}

/// [`extract_procedure_object_var_types`] for a declaration node the caller
/// already holds.
pub fn procedure_object_var_types_in_node(
    proc_node: tree_sitter::Node<'_>,
    source: &str,
) -> HashMap<String, String> {
    let source_bytes = source.as_bytes();
    let mut result = HashMap::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        match child.kind() {
            "var_section" => collect_object_vars_from_var_section(child, source_bytes, &mut result),
            "parameter_list" => {
                collect_object_vars_from_parameter_list(child, source_bytes, &mut result)
            }
            _ => {}
        }
    }
    result
}

pub(super) fn collect_object_vars_from_var_section(
    section: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = section.walk();
    for child in section.children(&mut cursor) {
        if child.kind() == "variable_declaration" {
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                if inner.kind() == "regular_variable_declaration" {
                    collect_object_var_from_regular_decl(inner, source, result);
                }
            }
            if child.kind() == "regular_variable_declaration" {
                collect_object_var_from_regular_decl(child, source, result);
            }
        }
    }
}

pub(super) fn collect_object_vars_from_parameter_list(
    param_list: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let mut cursor = param_list.walk();
    for child in param_list.children(&mut cursor) {
        if child.kind() == "parameter" {
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let Some(type_node) = child.child_by_field_name("type") else {
                continue;
            };
            extract_object_subtype(name_node, type_node, source, result);
        }
    }
}

pub(super) fn collect_object_var_from_regular_decl(
    node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    extract_object_subtype(name_node, type_node, source, result);
}

pub(super) fn extract_object_subtype(
    name_node: tree_sitter::Node,
    type_node: tree_sitter::Node,
    source: &[u8],
    result: &mut HashMap<String, String>,
) {
    let Some(name) = name_node
        .utf8_text(source)
        .ok()
        .map(|t| t.unquote_identifier().into_owned())
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let (type_kw, subtype) = parse_type_reference_for_record(type_node, source);
    // Lowercased so the match handles "Codeunit"/"codeunit"/"CODEUNIT".
    let kw_lower = type_kw.to_ascii_lowercase();
    let is_object_var = matches!(
        kw_lower.as_str(),
        "codeunit" | "page" | "report" | "xmlport" | "query" | "interface"
    );
    if is_object_var {
        if let Some(obj_name) = subtype {
            result.insert(name.to_lowercase(), obj_name);
        }
    }
}

pub(super) fn parse_type_reference_for_record(
    node: tree_sitter::Node,
    source: &[u8],
) -> (String, Option<String>) {
    let mut type_keyword = String::new();
    let mut subtype = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if type_keyword.is_empty() && kind.starts_with("kw_") {
            if let Ok(text) = child.utf8_text(source) {
                type_keyword = text.to_string();
            }
        } else if type_keyword.is_empty()
            && (kind == "identifier" || kind == "name" || kind == "name_or_keyword")
        {
            if let Ok(text) = child.utf8_text(source) {
                type_keyword = text.unquote_identifier().into_owned();
            }
        } else if !type_keyword.is_empty()
            && matches!(
                kind,
                "name_or_keyword" | "name" | "quoted_identifier" | "identifier" | "string"
            )
        {
            if let Ok(text) = child.utf8_text(source) {
                let clean = text.trim_matches('"').trim_matches('\'').to_string();
                if !clean.is_empty() {
                    subtype = Some(clean);
                }
            }
        }
    }

    (type_keyword, subtype)
}
