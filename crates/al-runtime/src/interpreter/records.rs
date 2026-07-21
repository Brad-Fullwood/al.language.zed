//! Record and table operations for the AL interpreter.
//!
//! Wires `Value::Record` to the in-memory [`MockRecord`] table store so that
//! the common BC `Record` API executes natively in the interpreter:
//! `Init`, field get/set, `Insert`, `Modify`, `Delete`, `Get`, `SetRange`,
//! `SetFilter`, `FindSet`/`FindFirst`/`FindLast`/`Find`/`Next`, `Count`,
//! `IsEmpty`, `Reset`, `SetCurrentKey`, `DeleteAll`.
//!
//! The interpreter is BC-free, so it has **no symbol table** for field numbers
//! or primary keys. Instead, when a record table is first touched, this module
//! looks the table up in the *workspace* (via `DispatchCtx::source`) and parses
//! its `fields { field(N; Name; …) }` and `keys { key(…; F1, F2) }` sections to
//! recover the field-name→number map and primary-key field list. Tables that are
//! not defined in the workspace (e.g. base-app `Customer`) cannot be modelled
//! and return an error.
//!
//! **FlowField / `CalcFormula` evaluation** is implemented for the aggregating
//! formula classes — `Sum`, `Average`, `Min`, `Max`, `Count`, `Exist` and
//! `Lookup`. A field declared `FieldClass = FlowField` with a parseable
//! `CalcFormula` is evaluated against the referenced table's in-memory store,
//! applying the `WHERE` clause (`CONST` / `FIELD` / `FILTER`), both on
//! `CalcFields(<field>)` and on a direct read of the field. `Linked` is not an
//! aggregation and is not modelled (a read of such a field returns an error);
//! a FlowField whose formula fails to parse falls back to its buffer value.

use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::eval_stmt::arg_expr_nodes;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{ErrorInfo, RecordValue, Value};
use crate::mock::calcformula_parser::{self, CalcFormula, FormulaType, WhereValue};
use crate::mock::filter;
use crate::mock::record::{FieldNo, FlowAgg, FlowFilter, MockRecord};

/// A live record-table backing store: the `MockRecord` data plus the
/// field-name→number map recovered from the workspace table definition.
#[derive(Debug, Clone)]
pub struct RecordStore {
    pub record: MockRecord,
    /// Lowercased field name → field number, from the table's `fields` section.
    field_by_name: HashMap<String, FieldNo>,
    /// FlowField field number → its parsed `CalcFormula`. Only fields with a
    /// `FieldClass = FlowField` *and* a parseable formula appear here; reads and
    /// `CalcFields` of these are computed from the referenced table's store.
    flowfields: HashMap<FieldNo, CalcFormula>,
    /// Next synthetic field number for a field name that wasn't in the parsed
    /// schema (keeps get/set self-consistent for partially-known tables).
    next_synthetic: FieldNo,
}

impl RecordStore {
    /// Resolve a field name to its number, synthesising a stable number for any
    /// name not present in the parsed schema so repeated get/set stay consistent.
    fn resolve_field(&mut self, name: &str) -> FieldNo {
        let key = name.trim().trim_matches('"').to_ascii_lowercase();
        if let Some(&no) = self.field_by_name.get(&key) {
            return no;
        }
        let no = self.next_synthetic;
        self.next_synthetic += 1;
        self.field_by_name.insert(key, no);
        no
    }
}

/// Parsed metadata for a workspace table definition.
struct TableMeta {
    table_id: i32,
    table_name: String,
    field_by_name: HashMap<String, FieldNo>,
    flowfields: HashMap<FieldNo, CalcFormula>,
    pk_fields: Vec<FieldNo>,
}

fn err(msg: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: msg.into(),
        error_type: None,
        source: None,
    })
}

/// Lowercased table-name store key.
fn key_for(table_name: &str) -> String {
    table_name.trim().trim_matches('"').to_ascii_lowercase()
}

/// Ensure a backing store exists for `table_name`, building it from the
/// workspace table definition on first use. Returns the store key on success.
fn ensure_store(ctx: &mut DispatchCtx, table_name: &str) -> Result<String, String> {
    let key = key_for(table_name);
    if ctx.records.contains_key(&key) {
        return Ok(key);
    }
    let source = Arc::clone(&ctx.source);
    let meta = load_table_meta(&*source, table_name).ok_or_else(|| {
        format!(
            "record table '{}' not found in workspace (native record ops require a workspace \
             table definition; base-app tables are not modelled)",
            table_name.trim().trim_matches('"')
        )
    })?;
    let mut pk = meta.pk_fields;
    if pk.is_empty() {
        // No usable key parsed — fall back to the lowest field number so Insert
        // / Get still have a deterministic key.
        if let Some(min) = meta.field_by_name.values().min().copied() {
            pk.push(min);
        }
    }
    let store = RecordStore {
        record: MockRecord::new(meta.table_id, meta.table_name, pk),
        field_by_name: meta.field_by_name,
        flowfields: meta.flowfields,
        next_synthetic: 1_000_000,
    };
    ctx.records.insert(key.clone(), store);
    Ok(key)
}

/// Locate and parse a workspace table object's metadata by name.
fn load_table_meta(source: &dyn al_types::ProcedureSource, table_name: &str) -> Option<TableMeta> {
    let want = table_name.trim().trim_matches('"');
    let path = source.find_by_object_name(want)?;
    let (text, tree) = source.get_cached_parse(&path)?;
    let bytes = text.as_bytes();
    parse_table_meta(tree.root_node(), bytes, want)
}

/// Parse a table `object_declaration` (matching `want`) into [`TableMeta`].
fn parse_table_meta(root: Node<'_>, source: &[u8], want: &str) -> Option<TableMeta> {
    let obj = find_table_object(root, source, want)?;

    let table_id = obj
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .and_then(|t| t.trim().parse::<i32>().ok())
        .unwrap_or(0);
    let table_name = object_name_of(obj, source).unwrap_or_else(|| want.to_string());

    let body = obj.child_by_field_name("body")?;

    let mut field_by_name: HashMap<String, FieldNo> = HashMap::new();
    let mut flowfields: HashMap<FieldNo, CalcFormula> = HashMap::new();
    let mut pk_field_names: Vec<String> = Vec::new();

    if let Some(fields_body) = section_body(body, "fields", source) {
        for fdef in sections_with_keyword(fields_body, "field", source) {
            if let Some((no, name, calc)) = parse_field_def(fdef, source) {
                field_by_name.insert(name.to_ascii_lowercase(), no);
                if let Some(formula) = calc {
                    flowfields.insert(no, formula);
                }
            }
        }
    }

    if let Some(keys_body) = section_body(body, "keys", source) {
        // The first `key(...)` is the primary key.
        if let Some(key_def) = sections_with_keyword(keys_body, "key", source)
            .into_iter()
            .next()
        {
            pk_field_names = parse_key_fields(key_def, source);
        }
    }

    let pk_fields: Vec<FieldNo> = pk_field_names
        .iter()
        .filter_map(|n| field_by_name.get(&n.to_ascii_lowercase()).copied())
        .collect();

    Some(TableMeta {
        table_id,
        table_name,
        field_by_name,
        flowfields,
        pk_fields,
    })
}

/// Find the `object_declaration` for a `table` whose name matches `want`.
fn find_table_object<'a>(root: Node<'a>, source: &[u8], want: &str) -> Option<Node<'a>> {
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
fn object_name_of(obj: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = obj.walk();
    for child in obj.named_children(&mut cursor) {
        if matches!(child.kind(), "quoted_identifier" | "identifier") {
            return child
                .utf8_text(source)
                .ok()
                .map(|t| t.trim_matches('"').to_string());
        }
    }
    None
}

/// Get the `body` object_body of the `object_section` whose keyword equals `kw`.
fn section_body<'a>(body: Node<'a>, kw: &str, source: &[u8]) -> Option<Node<'a>> {
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
fn sections_with_keyword<'a>(body: Node<'a>, kw: &str, source: &[u8]) -> Vec<Node<'a>> {
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

/// Parse `field(N; Name; Type) { ... }` → (field_no, name, flow_calc_formula).
/// The third element is `Some(formula)` only when the field is a FlowField with
/// a parseable `CalcFormula`.
fn parse_field_def(
    section: Node<'_>,
    source: &[u8],
) -> Option<(FieldNo, String, Option<CalcFormula>)> {
    let mut cursor = section.walk();
    let pblock = section
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parenthesized_block")?;

    let mut number: Option<FieldNo> = None;
    let mut name: Option<String> = None;
    let mut bc = pblock.walk();
    for child in pblock.named_children(&mut bc) {
        match child.kind() {
            "integer" if number.is_none() => {
                number = child
                    .utf8_text(source)
                    .ok()
                    .and_then(|t| t.trim().parse().ok());
            }
            "identifier" | "quoted_identifier" if number.is_some() && name.is_none() => {
                name = child
                    .utf8_text(source)
                    .ok()
                    .map(|t| t.trim_matches('"').to_string());
            }
            _ => {}
        }
    }

    let calc = parse_field_calcformula(section, source);
    Some((number?, name?, calc))
}

/// If a field's body declares `FieldClass = FlowField;` *and* a parseable
/// `CalcFormula = …;`, return the parsed [`CalcFormula`]. Returns `None` for a
/// non-FlowField, a FlowField without a formula, or an unparseable formula
/// (which then falls back to a plain buffer read).
fn parse_field_calcformula(section: Node<'_>, source: &[u8]) -> Option<CalcFormula> {
    let body = section.child_by_field_name("body")?;
    let mut is_flow = false;
    let mut formula_text: Option<String> = None;
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("");
        if name.eq_ignore_ascii_case("FieldClass") {
            let val = child
                .child_by_field_name("value")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("");
            if val.trim().eq_ignore_ascii_case("FlowField") {
                is_flow = true;
            }
        } else if name.eq_ignore_ascii_case("CalcFormula") {
            formula_text = property_value_text(child, source);
        }
    }
    if !is_flow {
        return None;
    }
    calcformula_parser::parse(&formula_text?).ok()
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

/// Parse `key(Name; F1, F2, …)` → the field names (excluding the key name).
fn parse_key_fields(section: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut cursor = section.walk();
    let Some(pblock) = section
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parenthesized_block")
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    let mut bc = pblock.walk();
    for child in pblock.named_children(&mut bc) {
        if matches!(child.kind(), "identifier" | "quoted_identifier") {
            if let Ok(t) = child.utf8_text(source) {
                names.push(t.trim_matches('"').to_string());
            }
        }
    }
    // The first name is the key's own name (e.g. `PK`); the rest are fields.
    if names.is_empty() {
        names
    } else {
        names[1..].to_vec()
    }
}

/// True if `method` is a record API method handled by [`dispatch_record_method`].
pub(crate) fn is_record_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "init"
            | "insert"
            | "modify"
            | "delete"
            | "get"
            | "setrange"
            | "setfilter"
            | "findset"
            | "findfirst"
            | "findlast"
            | "find"
            | "next"
            | "count"
            | "countapprox"
            | "isempty"
            | "reset"
            | "setcurrentkey"
            | "deleteall"
            | "calcfields"
    )
}

/// Execute a record-API method call (`Rec.Method(args)`).
///
/// `table_name` is the declared subtype of the receiver record variable.
/// `args_node` is the call's `argument_list` (so field-reference arguments —
/// e.g. the first arg of `SetRange` — can be read as field names rather than
/// evaluated as variables).
pub(crate) fn dispatch_record_method(
    table_name: &str,
    method: &str,
    args_node: Option<Node<'_>>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let nodes: Vec<Node> = args_node.map(arg_expr_nodes).unwrap_or_default();
    let lower = method.to_ascii_lowercase();

    // CalcFields takes field-name args (not value expressions): evaluate each
    // named FlowField's CalcFormula and write the result into the buffer so a
    // subsequent plain read sees it. Handled before the value-eval loop so the
    // field-name nodes are never evaluated as variables.
    if lower == "calcfields" {
        return dispatch_calcfields(table_name, &nodes, source, ctx);
    }

    // Field-reference methods: arg 0 is a field name (raw text), the rest values.
    let field_methods = matches!(lower.as_str(), "setrange" | "setfilter" | "setcurrentkey");

    // Evaluate the value-bearing argument nodes up front (records store is
    // touched afterwards, so the two mutable borrows of ctx don't overlap).
    let value_start = if field_methods { 1 } else { 0 };
    let mut values: Vec<Value> = Vec::new();
    for n in nodes.iter().skip(value_start) {
        match eval_expr(*n, source, stack, ctx) {
            Eval::Normal(v) => values.push(v),
            Eval::Error(e) => return Eval::Error(e),
            Eval::Exit(v) => return Eval::Exit(v),
            // Expressions can't legally produce break/continue statements.
            cf @ (Eval::Break | Eval::Continue) => return cf,
        }
    }

    let key = match ensure_store(ctx, table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };

    // setcurrentkey takes any number of field-name args (no value args).
    if lower == "setcurrentkey" {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let mut field_nos = Vec::new();
        for n in &nodes {
            let fname = node_text(*n, source);
            field_nos.push(store.resolve_field(&fname));
        }
        store.record.set_current_key(field_nos);
        return Eval::Normal(Value::Empty);
    }

    // Resolve the field-name argument for field-reference methods.
    let field_no = if field_methods {
        let fname = nodes
            .first()
            .map(|n| node_text(*n, source))
            .unwrap_or_default();
        if fname.is_empty() {
            return err(format!("{method}: missing field name argument"));
        }
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        Some(store.resolve_field(&fname))
    } else {
        None
    };

    let store = ctx.records.get_mut(&key).expect("store just ensured");

    match lower.as_str() {
        "init" => {
            store.record.init();
            Eval::Normal(Value::Empty)
        }
        "reset" => {
            store.record.reset();
            Eval::Normal(Value::Empty)
        }
        "insert" => match store
            .record
            .insert(values.first().map(truthy).unwrap_or(false))
        {
            Ok(()) => Eval::Normal(Value::Boolean(true)),
            Err(e) => err(format!("Insert: {e}")),
        },
        "modify" => match store
            .record
            .modify(values.first().map(truthy).unwrap_or(false))
        {
            Ok(()) => Eval::Normal(Value::Boolean(true)),
            Err(e) => err(format!("Modify: {e}")),
        },
        "delete" => match store
            .record
            .delete(values.first().map(truthy).unwrap_or(false))
        {
            Ok(()) => Eval::Normal(Value::Boolean(true)),
            Err(e) => err(format!("Delete: {e}")),
        },
        "get" => {
            if values.is_empty() {
                return err("Get: requires at least one primary-key value");
            }
            match store.record.get(values.clone()) {
                Ok(()) => Eval::Normal(Value::Boolean(true)),
                Err(_) => Eval::Normal(Value::Boolean(false)),
            }
        }
        "setrange" => {
            let f = field_no.unwrap();
            match values.len() {
                0 => {
                    store.record.clear_filter(f);
                    Eval::Normal(Value::Empty)
                }
                1 => {
                    store
                        .record
                        .set_range(f, values[0].clone(), values[0].clone());
                    Eval::Normal(Value::Empty)
                }
                _ => {
                    store
                        .record
                        .set_range(f, values[0].clone(), values[1].clone());
                    Eval::Normal(Value::Empty)
                }
            }
        }
        "setfilter" => {
            let f = field_no.unwrap();
            let expr = match values.first() {
                Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
                Some(v) => render_simple(v),
                None => return err("SetFilter: missing filter expression"),
            };
            match store.record.set_filter(f, &expr) {
                Ok(()) => Eval::Normal(Value::Empty),
                Err(e) => err(format!("SetFilter: {e}")),
            }
        }
        "findset" | "findfirst" => match store.record.find_first() {
            Ok(found) => Eval::Normal(Value::Boolean(found)),
            Err(e) => err(format!("{method}: {e}")),
        },
        "findlast" => match store.record.find_last() {
            Ok(found) => Eval::Normal(Value::Boolean(found)),
            Err(e) => err(format!("FindLast: {e}")),
        },
        "find" => {
            let dir = match values.first() {
                Some(Value::Text(s)) | Some(Value::Code(s)) => s.chars().next().unwrap_or('-'),
                _ => '-',
            };
            match store.record.find(dir) {
                Ok(found) => Eval::Normal(Value::Boolean(found)),
                Err(e) => err(format!("Find: {e}")),
            }
        }
        "next" => {
            let steps = match values.first() {
                Some(Value::Integer(n)) => *n as i32,
                _ => 1,
            };
            match store.record.next(steps) {
                Ok(moved) => Eval::Normal(Value::Integer(moved as i64)),
                Err(e) => err(format!("Next: {e}")),
            }
        }
        "count" | "countapprox" => Eval::Normal(Value::Integer(store.record.count() as i64)),
        "isempty" => Eval::Normal(Value::Boolean(store.record.is_empty())),
        "deleteall" => {
            // Delete every row matching the current filters.
            while store.record.find_first().unwrap_or(false) {
                if store.record.delete(false).is_err() {
                    break;
                }
            }
            Eval::Normal(Value::Empty)
        }
        other => err(format!("unsupported record method: {other}")),
    }
}

/// Read a record field by name: `x := Rec."Field"`.
///
/// A FlowField with a parseable `CalcFormula` is computed on read (auto-calc);
/// any other field returns its buffer value.
pub(crate) fn field_get(table_name: &str, field_name: &str, ctx: &mut DispatchCtx) -> Eval {
    let key = match ensure_store(ctx, table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    let (f, formula) = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let f = store.resolve_field(field_name);
        (f, store.flowfields.get(&f).cloned())
    };
    if let Some(formula) = formula {
        return eval_flowfield(ctx, &key, &formula);
    }
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    match store.record.field_get(f) {
        Some(v) => Eval::Normal(v.clone()),
        None => Eval::Normal(Value::Empty),
    }
}

/// `Rec.CalcFields(F1, F2, …)` — evaluate each named FlowField and store the
/// result into the current buffer. Non-FlowField (or unparseable) args are
/// ignored, matching BC's tolerance of explicitly-listed normal fields.
fn dispatch_calcfields(
    table_name: &str,
    nodes: &[Node<'_>],
    source: &[u8],
    ctx: &mut DispatchCtx,
) -> Eval {
    let key = match ensure_store(ctx, table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    // Resolve each field name to its number + formula up front (one borrow).
    let targets: Vec<(FieldNo, Option<CalcFormula>)> = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        nodes
            .iter()
            .map(|n| {
                let f = store.resolve_field(&node_text(*n, source));
                (f, store.flowfields.get(&f).cloned())
            })
            .collect()
    };
    for (field_no, formula) in targets {
        let Some(formula) = formula else { continue };
        match eval_flowfield(ctx, &key, &formula) {
            Eval::Normal(v) => {
                let store = ctx.records.get_mut(&key).expect("store just ensured");
                store.record.field_set(field_no, v);
            }
            other => return other,
        }
    }
    Eval::Normal(Value::Empty)
}

/// Evaluate a FlowField `CalcFormula` for the record whose store is `current_key`.
///
/// Resolves the `WHERE` clause (`CONST`/`FIELD`/`FILTER`), then aggregates over
/// the referenced table's store via [`MockRecord::calc_flow`]. `FIELD(...)`
/// references read the *calculating* record's current buffer; the aggregation
/// and the constrained fields are resolved against the *referenced* table.
fn eval_flowfield(ctx: &mut DispatchCtx, current_key: &str, formula: &CalcFormula) -> Eval {
    // 1. Resolve any FIELD(...) references against the calculating record's
    //    buffer first, before borrowing the referenced store (they may be the
    //    same store for a self-referencing FlowField).
    let field_values: Vec<Option<Value>> = {
        let store = ctx
            .records
            .get_mut(current_key)
            .expect("store just ensured");
        formula
            .where_clause
            .iter()
            .map(|cond| match &cond.value {
                WhereValue::Field(name) => {
                    let f = store.resolve_field(name);
                    Some(store.record.field_get(f).cloned().unwrap_or(Value::Empty))
                }
                _ => None,
            })
            .collect()
    };

    // 2. Ensure the referenced table's store exists.
    let ref_key = match ensure_store(ctx, &formula.table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };

    let agg = match formula.formula_type {
        FormulaType::Sum => FlowAgg::Sum,
        FormulaType::Average => FlowAgg::Average,
        FormulaType::Min => FlowAgg::Min,
        FormulaType::Max => FlowAgg::Max,
        FormulaType::Count => FlowAgg::Count,
        FormulaType::Exist => FlowAgg::Exist,
        FormulaType::Lookup => FlowAgg::Lookup,
        FormulaType::Linked => {
            return err(
                "FlowField CalcFormula 'Linked' is a record relationship, not an aggregation, \
                 and is not modelled by the BC-free interpreter",
            );
        }
    };

    // 3. Resolve the target + condition fields against the referenced table and
    //    build the resolved conditions.
    let store = ctx.records.get_mut(&ref_key).expect("store just ensured");
    let target = formula
        .field_name
        .as_deref()
        .map(|n| store.resolve_field(n));

    let mut conditions: Vec<(FieldNo, FlowFilter)> = Vec::new();
    for (i, cond) in formula.where_clause.iter().enumerate() {
        let field_no = store.resolve_field(&cond.field);
        let filt = match &cond.value {
            WhereValue::Const(s) => FlowFilter::Eq(parse_scalar(s)),
            WhereValue::Field(_) => FlowFilter::Eq(field_values[i].clone().unwrap_or(Value::Empty)),
            WhereValue::Filter(expr) => match filter::parse(expr) {
                Ok(parsed) => FlowFilter::Expr(parsed),
                Err(e) => return err(format!("FlowField filter '{expr}': {e}")),
            },
        };
        conditions.push((field_no, filt));
    }

    Eval::Normal(store.record.calc_flow(&conditions, target, agg))
}

/// Parse a `CONST(...)` literal into the most specific scalar `Value`. Matching
/// is type-tolerant downstream (see `FlowFilter::Eq`), so `Text` is a safe
/// fallback even when the referenced field is `Code`/`Option`.
fn parse_scalar(s: &str) -> Value {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Value::Integer(n);
    }
    if let Ok(d) = t.parse::<crate::interpreter::value::Decimal>() {
        return Value::Decimal(d);
    }
    Value::Text(t.to_string())
}

/// If `lhs_node` is a record field access (`Rec."Field"`) whose receiver is a
/// bound `Value::Record`, set the field to `rhs_val` and return `Some(result)`.
/// Returns `None` when the LHS is not a record field access.
pub(crate) fn try_field_assign(
    lhs_node: Node<'_>,
    source: &[u8],
    rhs_val: &Value,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let (recv, field_name) = record_field_access(lhs_node, source)?;
    let table_name = match stack.lookup(&recv) {
        Some(Value::Record(rv)) => rv.table_name.clone(),
        _ => return None,
    };
    let key = match ensure_store(ctx, &table_name) {
        Ok(k) => k,
        Err(e) => return Some(err(e)),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let f = store.resolve_field(&field_name);
    store.record.field_set(f, rhs_val.clone());
    Some(Eval::Normal(Value::Empty))
}

/// If `node` is a `postfix_expression` of the shape `primary . member` with a
/// `member_suffix` (field access, no call), return `(receiver_name, field_name)`.
pub(crate) fn record_field_access(node: Node<'_>, source: &[u8]) -> Option<(String, String)> {
    // Descend through transparent wrappers to the postfix_expression.
    let pf = descend_to_postfix(node)?;
    if pf.kind() != "postfix_expression" {
        return None;
    }
    let mut cursor = pf.walk();
    let children: Vec<Node> = pf.children(&mut cursor).collect();
    // Must have exactly a member_suffix (not member_call_suffix / scope).
    let suffix = children.iter().rev().find(|c| {
        matches!(
            c.kind(),
            "member_suffix" | "member_call_suffix" | "scope_call_suffix" | "scope_suffix"
        )
    })?;
    if suffix.kind() != "member_suffix" {
        return None;
    }
    let recv = pf
        .child(0)
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.trim_matches('"').to_string())?;
    let member_node = suffix.child_by_field_name("member").or_else(|| {
        let mut c = suffix.walk();
        let found = suffix
            .named_children(&mut c)
            .find(|n| matches!(n.kind(), "identifier" | "quoted_identifier" | "name"));
        found
    });
    let field = member_node
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.trim_matches('"').to_string())?;
    Some((recv, field))
}

fn descend_to_postfix(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "postfix_expression" => Some(node),
        "expression" | "unary_expression" | "primary_expression" => {
            if node.named_child_count() == 1 {
                descend_to_postfix(node.named_child(0)?)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// True if `method` is a `List of [T]` method handled by [`dispatch_list_method`].
pub(crate) fn is_list_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "add" | "get" | "count" | "contains" | "indexof" | "remove" | "removeat" | "set"
    )
}

/// Execute a `List of [T]` method call on the list bound to `recv`.
pub(crate) fn dispatch_list_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
) -> Eval {
    let lower = method.to_ascii_lowercase();
    let Some(slot) = stack.lookup_mut(recv) else {
        return err(format!("list variable '{recv}' is not bound"));
    };
    let Value::List(items) = slot else {
        return err(format!("'{recv}' is not a List"));
    };
    match lower.as_str() {
        "add" => {
            for v in args {
                items.push(v);
            }
            Eval::Normal(Value::Boolean(true))
        }
        "count" => Eval::Normal(Value::Integer(items.len() as i64)),
        "get" => {
            // 1-based index.
            match args.first() {
                Some(Value::Integer(i)) if *i >= 1 && (*i as usize) <= items.len() => {
                    Eval::Normal(items[(*i as usize) - 1].clone())
                }
                Some(Value::Integer(i)) => err(format!(
                    "List.Get: index {i} out of range 1..{}",
                    items.len()
                )),
                _ => err("List.Get expects an Integer index"),
            }
        }
        "contains" => {
            let needle = args.first();
            let found = needle.is_some_and(|n| items.iter().any(|it| it == n));
            Eval::Normal(Value::Boolean(found))
        }
        "indexof" => {
            let needle = args.first();
            let idx = needle
                .and_then(|n| items.iter().position(|it| it == n))
                .map(|p| p as i64 + 1)
                .unwrap_or(0);
            Eval::Normal(Value::Integer(idx))
        }
        "removeat" => match args.first() {
            Some(Value::Integer(i)) if *i >= 1 && (*i as usize) <= items.len() => {
                items.remove((*i as usize) - 1);
                Eval::Normal(Value::Boolean(true))
            }
            _ => err("List.RemoveAt expects a valid 1-based Integer index"),
        },
        "remove" => {
            if let Some(n) = args.first() {
                if let Some(pos) = items.iter().position(|it| it == n) {
                    items.remove(pos);
                    return Eval::Normal(Value::Boolean(true));
                }
            }
            Eval::Normal(Value::Boolean(false))
        }
        "set" => match (args.first(), args.get(1)) {
            (Some(Value::Integer(i)), Some(v)) if *i >= 1 && (*i as usize) <= items.len() => {
                items[(*i as usize) - 1] = v.clone();
                Eval::Normal(Value::Boolean(true))
            }
            _ => err("List.Set expects (Integer index, value)"),
        },
        other => err(format!("unsupported List method: {other}")),
    }
}

/// Build a default `Value` for a structured local variable type the scalar
/// `Value::default_for` does not cover: `Record <Subtype>`, `Codeunit <Subtype>`,
/// and `List of [T]`. Returns `None` for anything else.
pub(crate) fn default_for_structured(type_text: &str) -> Option<Value> {
    let trimmed = type_text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("record") {
        // `Record "My Item"` / `Record Item` — grab the subtype from the
        // original (case-preserving) text after the `Record` keyword.
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let subtype = subtype_after_keyword(trimmed, "record");
            return Some(Value::Record(RecordValue {
                table_name: subtype,
                table_id: 0,
                handle: None,
            }));
        }
    }
    if let Some(rest) = lower.strip_prefix("codeunit") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let object_name = subtype_after_keyword(trimmed, "codeunit");
            if !object_name.is_empty() {
                return Some(Value::Codeunit { object_name });
            }
        }
    }
    if lower.starts_with("list of") {
        return Some(Value::List(Vec::new()));
    }
    None
}

/// Extract the subtype name following a leading keyword, stripping quotes.
fn subtype_after_keyword(type_text: &str, keyword: &str) -> String {
    let rest = type_text[keyword.len()..].trim();
    rest.trim_matches('"').trim().to_string()
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .to_string()
}

fn truthy(v: &Value) -> bool {
    matches!(v, Value::Boolean(true))
}

fn render_simple(v: &Value) -> String {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(d) => d.normalize().to_string(),
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Boolean(b) => b.to_string(),
        _ => String::new(),
    }
}
