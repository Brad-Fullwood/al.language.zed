//! Parses a workspace table object's `fields` and `keys` sections into
//! [`TableMeta`]: the field-name/number map, FlowField formulas, typed field
//! defaults and lengths, and the primary-key field list. [`load_table_meta`]
//! is how [`super::store::ensure_store`] and `Validate`'s `TableRelation`
//! check build a table's schema from the workspace source.

use std::collections::HashMap;

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::value::Value;
use crate::mock::calcformula_parser::{self, CalcFormula};
use crate::mock::record::FieldNo;

use super::helpers::unquote_subtype;

/// Parsed metadata for a workspace table definition.
pub(super) struct TableMeta {
    pub(super) table_id: i32,
    pub(super) table_name: String,
    pub(super) field_by_name: HashMap<String, FieldNo>,
    pub(super) flowfields: HashMap<FieldNo, CalcFormula>,
    pub(super) field_defaults: HashMap<FieldNo, Value>,
    pub(super) field_lengths: HashMap<FieldNo, usize>,
    pub(super) pk_fields: Vec<FieldNo>,
}

/// Locate and parse a workspace table object's metadata by name.
pub(super) fn load_table_meta(
    source: &dyn al_types::ProcedureSource,
    table_name: &str,
) -> Result<TableMeta, String> {
    let want = table_name.unquote_identifier();
    let path = source
        .find_object_of_kind(&want, &["table"])
        .ok_or_else(|| {
            format!(
                "record table '{want}' not found in workspace (native record ops require a \
                 workspace table definition; base-app tables are not modelled)"
            )
        })?;
    let (text, tree) = source.get_cached_parse(&path).ok_or_else(|| {
        format!(
            "record table '{want}' at {} has no cached syntax tree",
            path.display()
        )
    })?;
    if tree.root_node().has_error() {
        return Err(format!(
            "record table '{want}' at {} contains syntax errors",
            path.display()
        ));
    }
    let bytes = text.as_bytes();
    parse_table_meta(tree.root_node(), bytes, &want, source)
        .map_err(|reason| format!("invalid metadata for record table '{want}': {reason}"))
}

/// Parse a table `object_declaration` (matching `want`) into [`TableMeta`].
fn parse_table_meta(
    root: Node<'_>,
    source: &[u8],
    want: &str,
    workspace: &dyn al_types::ProcedureSource,
) -> Result<TableMeta, String> {
    let obj = find_table_object(root, source, want)
        .ok_or_else(|| "matching table object declaration was not found".to_string())?;

    let id_text = obj
        .child_by_field_name("id")
        .ok_or_else(|| "table object ID is missing".to_string())?
        .utf8_text(source)
        .map_err(|error| format!("table object ID is not UTF-8: {error}"))?;
    let table_id = id_text
        .trim()
        .parse::<i32>()
        .map_err(|error| format!("table object ID '{id_text}' is invalid: {error}"))?;
    if table_id <= 0 {
        return Err(format!("table object ID must be positive, got {table_id}"));
    }
    let table_name =
        object_name_of(obj, source).ok_or_else(|| "table object name is missing".to_string())?;

    let body = obj
        .child_by_field_name("body")
        .ok_or_else(|| "table object body is missing".to_string())?;

    let mut field_by_name: HashMap<String, FieldNo> = HashMap::new();
    let mut flowfields: HashMap<FieldNo, CalcFormula> = HashMap::new();
    let mut field_defaults: HashMap<FieldNo, Value> = HashMap::new();
    let mut field_lengths: HashMap<FieldNo, usize> = HashMap::new();

    let fields_body = section_body(body, "fields", source)
        .ok_or_else(|| "fields section is missing".to_string())?;
    let mut field_numbers = std::collections::HashSet::new();
    for fdef in sections_with_keyword(fields_body, "field", source) {
        let (no, name, type_text, calc) = parse_field_def(fdef, source)?;
        let option_members = parse_option_members(fdef, source);
        if no <= 0 {
            return Err(format!("field '{name}' has non-positive number {no}"));
        }
        if !field_numbers.insert(no) {
            return Err(format!("duplicate field number {no}"));
        }
        let normalized_name = name.to_ascii_lowercase();
        if field_by_name.insert(normalized_name, no).is_some() {
            return Err(format!("duplicate field name '{name}'"));
        }
        if let Some(type_text) = type_text {
            // Strip any length suffix (`Text[20]`, `Code[10]`) to the base name.
            let base = type_text
                .split(['[', ' '])
                .next()
                .unwrap_or(&type_text)
                .trim();
            if let Some(length) = crate::interpreter::dispatch::declared_text_length(&type_text) {
                field_lengths.insert(no, length);
            }
            if let Some(default) = Value::default_for(base) {
                field_defaults.insert(no, default);
            } else if let Some(default) =
                option_field_default(base, &type_text, option_members.as_deref(), workspace)
            {
                field_defaults.insert(no, default);
            }
        }
        if let Some(formula) = calc {
            if flowfields.insert(no, formula).is_some() {
                return Err(format!("duplicate FlowField metadata for field '{name}'"));
            }
        }
    }
    if field_by_name.is_empty() {
        return Err("fields section contains no usable field definitions".to_string());
    }

    // Post grammar bump `keys { key(...) {} }` parses as a dedicated
    // `key_section` (not a generic `object_section`) whose `body:` holds
    // `key_declaration` nodes; the first is the primary key.
    let key_section =
        find_key_section(body).ok_or_else(|| "keys section is missing".to_string())?;
    let keys_body = key_section
        .child_by_field_name("body")
        .ok_or_else(|| "keys section has no body".to_string())?;
    let key_def = first_key_declaration(keys_body)
        .ok_or_else(|| "keys section contains no primary key".to_string())?;
    let pk_field_names = parse_key_fields(key_def, source)?;
    if pk_field_names.is_empty() {
        return Err("primary key contains no fields".to_string());
    }

    let pk_fields: Vec<FieldNo> = pk_field_names
        .iter()
        .map(|name| {
            field_by_name
                .get(&name.to_ascii_lowercase())
                .copied()
                .ok_or_else(|| format!("primary-key field '{name}' is not declared"))
        })
        .collect::<Result<_, _>>()?;

    Ok(TableMeta {
        table_id,
        table_name,
        field_by_name,
        flowfields,
        field_defaults,
        field_lengths,
        pk_fields,
    })
}

/// Find the `object_declaration` for a `table` whose name matches `want`.
pub(crate) fn find_table_object<'a>(root: Node<'a>, source: &[u8], want: &str) -> Option<Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_declaration" {
            let is_table = node
                .child_by_field_name("kind")
                .map(|k| k.kind() == "kw_table")
                .unwrap_or(false);
            if is_table {
                let name = object_name_of(node, source);
                if name
                    .as_deref()
                    .map(|n| n.eq_ignore_ascii_case(want))
                    .unwrap_or(false)
                {
                    return Some(node);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// The object name (quoted or bare identifier) declared on an `object_declaration`.
///
/// The name lives on the `name:` field as
/// `(name_or_keyword (name (identifier | quoted_identifier)))`. Reading the
/// field text and stripping any surrounding quotes recovers the identifier.
pub(crate) fn object_name_of(obj: Node<'_>, source: &[u8]) -> Option<String> {
    obj.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.unquote_identifier().into_owned())
}

/// Get the `body` object_body of the `object_section` whose keyword equals `kw`.
pub(crate) fn section_body<'a>(body: Node<'a>, kw: &str, source: &[u8]) -> Option<Node<'a>> {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() != "object_section" {
            continue;
        }
        if section_keyword(child, source).is_some_and(|k| k.eq_ignore_ascii_case(kw)) {
            return child.child_by_field_name("body");
        }
    }
    None
}

/// All `object_section` children of `body` whose keyword equals `kw`.
pub(crate) fn sections_with_keyword<'a>(body: Node<'a>, kw: &str, source: &[u8]) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() == "object_section"
            && section_keyword(child, source).is_some_and(|k| k.eq_ignore_ascii_case(kw))
        {
            out.push(child);
        }
    }
    out
}

/// The table's `key_section` (the `keys { }` block) among an object body's
/// children. Post grammar bump `keys` is a dedicated `key_section` node rather
/// than a generic `object_section`, so it is matched by kind, not keyword text.
fn find_key_section<'a>(body: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = body.walk();
    let found = body
        .named_children(&mut cursor)
        .find(|c| c.kind() == "key_section");
    found
}

/// The first `key_declaration` (the primary key) inside a `key_section` body.
fn first_key_declaration<'a>(keys_body: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = keys_body.walk();
    let found = keys_body
        .named_children(&mut cursor)
        .find(|c| c.kind() == "key_declaration");
    found
}

fn section_keyword(section: Node<'_>, source: &[u8]) -> Option<String> {
    let keyword = section.child_by_field_name("keyword").or_else(|| {
        let mut c = section.walk();
        let found = section
            .named_children(&mut c)
            .find(|n| n.kind() == "metadata_keyword");
        found
    });
    keyword
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.trim().to_string())
}

/// Parse `field(N; Name; Type) { ... }` →
/// (field_no, name, declared_type_text, flow_calc_formula).
/// The last element is `Some(formula)` only when the field is a FlowField.
/// Malformed FlowField metadata is an error.
pub(crate) fn parse_field_def(
    section: Node<'_>,
    source: &[u8],
) -> Result<(FieldNo, String, Option<String>, Option<CalcFormula>), String> {
    let mut cursor = section.walk();
    let pblock = section
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parenthesized_block")
        .ok_or_else(|| "field definition is missing its parameter list".to_string())?;

    let mut number: Option<FieldNo> = None;
    let mut name: Option<String> = None;
    let mut type_text: Option<String> = None;
    let mut type_start: Option<usize> = None;
    let mut segment = 0_u8;
    let mut bc = pblock.walk();
    for child in pblock.children(&mut bc) {
        if child.kind() == "semicolon" {
            segment = segment.saturating_add(1);
            continue;
        }
        if !child.is_named() {
            continue;
        }
        match segment {
            0 if child.kind() == "integer" && number.is_none() => {
                let text = child
                    .utf8_text(source)
                    .map_err(|error| format!("field number is not UTF-8: {error}"))?;
                number = Some(
                    text.trim()
                        .parse()
                        .map_err(|error| format!("field number '{text}' is invalid: {error}"))?,
                );
            }
            1 if name.is_none() => {
                name = Some(
                    child
                        .utf8_text(source)
                        .map_err(|error| format!("field name is not UTF-8: {error}"))?
                        .unquote_identifier()
                        .to_string(),
                );
            }
            // The declared type can span several children of the generic
            // parenthesized block (`Code` + `[20]`), so slice the source from
            // the first to the last rather than taking only the first.
            2 => {
                let end = child.end_byte();
                let start = type_start.get_or_insert(child.start_byte());
                type_text = std::str::from_utf8(source.get(*start..end).unwrap_or_default())
                    .ok()
                    .map(|text| text.trim().to_string());
            }
            _ => {}
        }
    }

    let number = number.ok_or_else(|| "field number is missing".to_string())?;
    let name = name.ok_or_else(|| format!("field {number} name is missing"))?;
    let calc = parse_field_calcformula(section, source, &name)?;
    Ok((number, name, type_text, calc))
}

/// If a field's body declares `FieldClass = FlowField;` *and* a parseable
/// `CalcFormula = …;`, return the parsed [`CalcFormula`]. Returns `None` for a
/// non-FlowField; missing or unparseable FlowField formulas are errors.
fn parse_field_calcformula(
    section: Node<'_>,
    source: &[u8],
    field_name: &str,
) -> Result<Option<CalcFormula>, String> {
    let Some(body) = section.child_by_field_name("body") else {
        return Ok(None);
    };
    let mut is_flow = false;
    let mut formula_text: Option<String> = None;
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .ok_or_else(|| format!("property on field '{field_name}' has no name"))?
            .utf8_text(source)
            .map_err(|error| {
                format!("property name on field '{field_name}' is invalid: {error}")
            })?;
        if name.eq_ignore_ascii_case("FieldClass") {
            let val = child
                .child_by_field_name("value")
                .ok_or_else(|| format!("FieldClass on field '{field_name}' has no value"))?
                .utf8_text(source)
                .map_err(|error| {
                    format!("FieldClass value on field '{field_name}' is invalid: {error}")
                })?;
            if val.trim().eq_ignore_ascii_case("FlowField") {
                is_flow = true;
            }
        } else if name.eq_ignore_ascii_case("CalcFormula") {
            formula_text = property_value_text(child, source);
        }
    }
    if !is_flow {
        return Ok(None);
    }
    let formula_text =
        formula_text.ok_or_else(|| format!("FlowField '{field_name}' has no CalcFormula"))?;
    calcformula_parser::parse(&formula_text)
        .map(Some)
        .map_err(|error| format!("FlowField '{field_name}' CalcFormula is invalid: {error}"))
}

/// The `OptionMembers` property of a field, verbatim (`Low,High` or
/// `" ",Low,High`). `None` when the field does not declare one.
fn parse_option_members(section: Node<'_>, source: &[u8]) -> Option<String> {
    let body = section.child_by_field_name("body")?;
    let mut cursor = body.walk();
    let found = body
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "property_assignment")
        .find(|child| {
            child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .is_some_and(|n| n.trim().eq_ignore_ascii_case("OptionMembers"))
        });
    found.and_then(|prop| property_value_text(prop, source))
}

/// The typed zero of an `Option` or `Enum "X"` field: ordinal 0, named after
/// whichever member carries that ordinal.
///
/// BC zero-initialises every field, and an option or enum field holds an
/// integer ordinal, so an unassigned one reads as its ordinal-0 member rather
/// than as an absent value. An `Option` field names its members inline; an
/// `Enum "X"` field takes them from the workspace enum object, and when that
/// object is not in the workspace the member name is left empty so the ordinal
/// still compares.
fn option_field_default(
    base_type: &str,
    type_text: &str,
    option_members: Option<&str>,
    workspace: &dyn al_types::ProcedureSource,
) -> Option<Value> {
    if base_type.eq_ignore_ascii_case("option") {
        let member = option_members
            .and_then(|members| members.split(',').next())
            .map(|m| m.unquote_identifier().into_owned())
            .unwrap_or_default();
        return Some(Value::Option {
            type_name: String::new(),
            member,
            ordinal: 0,
        });
    }
    if !base_type.eq_ignore_ascii_case("enum") {
        return None;
    }
    let type_name = unquote_subtype(&type_text[base_type.len()..]);
    Some(Value::Option {
        member: enum_member_with_ordinal_zero(workspace, &type_name).unwrap_or_default(),
        type_name,
        ordinal: 0,
    })
}

/// The name of the `value(0; …)` member of a workspace enum object.
fn enum_member_with_ordinal_zero(
    workspace: &dyn al_types::ProcedureSource,
    type_name: &str,
) -> Option<String> {
    let path = workspace.find_object_of_kind(type_name, &["enum"])?;
    let (text, tree) = workspace.get_cached_parse(&path)?;
    let bytes = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_value_declaration" {
            let ordinal = node
                .child_by_field_name("id")
                .and_then(|id| id.utf8_text(bytes).ok())
                .and_then(|id| id.trim().parse::<i64>().ok());
            if ordinal == Some(0) {
                return node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(bytes).ok())
                    .map(|name| name.unquote_identifier().into_owned());
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

/// Reconstruct the full text of a `property_assignment`'s value. The grammar
/// models the value as `repeat1(choice(...))`, so a `CalcFormula` like
/// `Sum("X".Amount WHERE (…))` spans several `value` children (`Sum` + a
/// `parenthesized_block`). Slice the source from the first to the last to
/// recover the verbatim formula string.
fn property_value_text(prop: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = prop.walk();
    let vals: Vec<Node> = prop.children_by_field_name("value", &mut cursor).collect();
    let start = vals.first()?.start_byte();
    let end = vals.last()?.end_byte();
    std::str::from_utf8(source.get(start..end)?)
        .ok()
        .map(|s| s.to_string())
}

/// Parse a `key(Name; F1, F2, …)` `key_declaration` → the field names
/// (excluding the key's own name).
///
/// The field references live on the grammar's `fields:` `key_field_list`; the
/// key name is a separate `name:` field, so the list already excludes it. The
/// list's named children are the field references interleaved with `comma`
/// nodes.
fn parse_key_fields(key: Node<'_>, source: &[u8]) -> Result<Vec<String>, String> {
    let list = key
        .child_by_field_name("fields")
        .ok_or_else(|| "primary key definition is missing its field list".to_string())?;
    let mut names: Vec<String> = Vec::new();
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if child.kind() == "comma" {
            continue;
        }
        let text = child
            .utf8_text(source)
            .map_err(|error| format!("primary-key field name is not UTF-8: {error}"))?;
        names.push(text.unquote_identifier().into_owned());
    }
    Ok(names)
}

/// Whether workspace table `table` declares a field named `field`, read from
/// the table's source as the store reads it. The test router asks this for a
/// member written without parentheses (`R.LockTable;`), so it and the runtime
/// agree on which members are field reads.
pub fn declares_field_in(source: &dyn al_types::ProcedureSource, table: &str, field: &str) -> bool {
    load_table_meta(source, table).is_ok_and(|meta| {
        meta.field_by_name
            .contains_key(&field.unquote_identifier().to_ascii_lowercase())
    })
}
