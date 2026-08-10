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
//! aggregation and is not modelled (a read of such a field returns an error).
//! Invalid table metadata or FlowField formulas fail explicitly instead of
//! fabricating field IDs, primary keys, or plain-buffer fallbacks.

use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::dispatch::DispatchMode;
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::eval_stmt::arg_expr_nodes;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{ErrorInfo, RecordValue, Value};
use crate::mock::calcformula_parser::{self, CalcFormula, FormulaType, WhereValue};
use crate::mock::filter;
use crate::mock::record::{FieldNo, FlowAgg, FlowFilter, MockRecord, RecordView};

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
    /// Field number → the typed default value of the declared field type
    /// (`0` / `''` / `false` / `0D` …). Reads of never-assigned fields return
    /// this default (BC zero-initialises every field); fields with types the
    /// interpreter cannot default are absent.
    field_defaults: HashMap<FieldNo, Value>,
    /// Per-record-variable view state (filters/cursor/buffer), keyed by the
    /// variable's handle. BC gives each record variable independent state over
    /// the shared physical table.
    views: HashMap<u64, RecordView>,
}

impl RecordStore {
    /// Resolve a field name through the parsed workspace schema.
    fn resolve_field(&self, name: &str) -> Result<FieldNo, String> {
        let key = name.trim().trim_matches('"').to_ascii_lowercase();
        self.field_by_name.get(&key).copied().ok_or_else(|| {
            format!(
                "field '{}' is not declared on workspace table '{}'",
                name.trim().trim_matches('"'),
                self.record.table_name
            )
        })
    }

    /// Take the view for `handle` out of the store, creating a fresh one for a
    /// first use. The caller MUST put it back with [`RecordStore::put_view`]
    /// on every path.
    fn take_view(&mut self, handle: u64) -> RecordView {
        self.views
            .remove(&handle)
            .unwrap_or_else(|| self.record.new_view())
    }

    fn put_view(&mut self, handle: u64, view: RecordView) {
        self.views.insert(handle, view);
    }

    /// Coerce a value being written into `field` to the field's declared type
    /// (Code caselessness, integer width, Decimal promotion). Unknown field
    /// types store the value as-is.
    fn coerce_to_field(&self, field: FieldNo, value: Value) -> Result<Value, String> {
        match self.field_defaults.get(&field) {
            Some(default) => Value::coerce_into_slot(default, value),
            None => Ok(value),
        }
    }
}

/// Parsed metadata for a workspace table definition.
struct TableMeta {
    table_id: i32,
    table_name: String,
    field_by_name: HashMap<String, FieldNo>,
    flowfields: HashMap<FieldNo, CalcFormula>,
    field_defaults: HashMap<FieldNo, Value>,
    pk_fields: Vec<FieldNo>,
}

fn err(msg: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: msg.into(),
        error_type: None,
        source: None,
    })
}

fn records_enabled(ctx: &DispatchCtx) -> bool {
    matches!(ctx.mode, DispatchMode::WithRecords)
}

fn records_disabled_error() -> Eval {
    err("record access is unavailable in pure-logic interpreter mode")
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
    let meta = load_table_meta(&*source, table_name)?;
    let store = RecordStore {
        record: MockRecord::new(meta.table_id, meta.table_name, meta.pk_fields),
        field_by_name: meta.field_by_name,
        flowfields: meta.flowfields,
        field_defaults: meta.field_defaults,
        views: HashMap::new(),
    };
    ctx.records.insert(key.clone(), store);
    Ok(key)
}

/// Resolve the record variable named `recv` to its `(table_name, handle)`
/// pair, allocating a fresh per-variable view handle on first use and writing
/// it back onto the variable's `RecordValue`. Returns `None` when `recv` is
/// not a bound record variable.
pub(crate) fn record_binding(
    recv: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<(String, u64)> {
    let table_name = match stack.lookup(recv) {
        Some(Value::Record(rv)) => rv.table_name.clone(),
        _ => return None,
    };
    let existing = match stack.lookup(recv) {
        Some(Value::Record(rv)) => rv.handle,
        _ => None,
    };
    let handle = match existing {
        Some(handle) => handle,
        None => {
            ctx.next_record_handle += 1;
            let handle = ctx.next_record_handle;
            if let Some(Value::Record(rv)) = stack.lookup_mut(recv) {
                rv.handle = Some(handle);
            }
            handle
        }
    };
    Some((table_name, handle))
}

/// Give a by-value record argument its own view, seeded from the caller's
/// buffer.
///
/// BC passes a record by value as a *copy*: the callee sees the caller's
/// current row buffer (and `xRec`) — including field assignments made without
/// an `Insert` — but gets independent filters, sort key and iteration cursor,
/// so a `SetRange`/`Next` inside the procedure cannot disturb the caller.
/// Simply clearing the handle produced the independence but lost the buffer;
/// simply keeping it shared the caller's filters. This mints a fresh handle
/// and copies only the buffers across.
///
/// `var` (by-reference) parameters must keep the caller's handle and never
/// reach this function.
pub(crate) fn fork_record_for_by_value(ctx: &mut DispatchCtx, rv: &mut RecordValue) {
    // No handle means the caller never touched the record: there is no buffer
    // to copy and the callee will mint its own view on first access.
    let Some(caller_handle) = rv.handle.take() else {
        return;
    };
    let key = key_for(&rv.table_name);
    ctx.next_record_handle += 1;
    let handle = ctx.next_record_handle;
    let Some(store) = ctx.records.get_mut(&key) else {
        // The table was never materialized, so there is no view state to copy.
        // Leave the handle cleared; the callee allocates its own on demand.
        return;
    };
    let mut view = store.record.new_view();
    if let Some(caller_view) = store.views.get(&caller_handle) {
        view.copy_buffers_from(caller_view);
    }
    store.put_view(handle, view);
    rv.handle = Some(handle);
}

/// Locate and parse a workspace table object's metadata by name.
fn load_table_meta(
    source: &dyn al_types::ProcedureSource,
    table_name: &str,
) -> Result<TableMeta, String> {
    let want = table_name.trim().trim_matches('"');
    let path = source.find_by_object_name(want).ok_or_else(|| {
        format!(
            "record table '{want}' not found in workspace (native record ops require a workspace \
             table definition; base-app tables are not modelled)"
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
    parse_table_meta(tree.root_node(), bytes, want)
        .map_err(|reason| format!("invalid metadata for record table '{want}': {reason}"))
}

/// Parse a table `object_declaration` (matching `want`) into [`TableMeta`].
fn parse_table_meta(root: Node<'_>, source: &[u8], want: &str) -> Result<TableMeta, String> {
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

    let fields_body = section_body(body, "fields", source)
        .ok_or_else(|| "fields section is missing".to_string())?;
    let mut field_numbers = std::collections::HashSet::new();
    for fdef in sections_with_keyword(fields_body, "field", source) {
        let (no, name, type_text, calc) = parse_field_def(fdef, source)?;
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
            if let Some(default) = Value::default_for(base) {
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

    let keys_body =
        section_body(body, "keys", source).ok_or_else(|| "keys section is missing".to_string())?;
    let key_def = sections_with_keyword(keys_body, "key", source)
        .into_iter()
        .next()
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

/// Parse `field(N; Name; Type) { ... }` →
/// (field_no, name, declared_type_text, flow_calc_formula).
/// The last element is `Some(formula)` only when the field is a FlowField.
/// Malformed FlowField metadata is an error.
#[allow(clippy::type_complexity)]
fn parse_field_def(
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
                        .trim_matches('"')
                        .to_string(),
                );
            }
            2 if type_text.is_none() => {
                type_text = child
                    .utf8_text(source)
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
fn parse_key_fields(section: Node<'_>, source: &[u8]) -> Result<Vec<String>, String> {
    let mut cursor = section.walk();
    let pblock = section
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parenthesized_block")
        .ok_or_else(|| "primary key definition is missing its parameter list".to_string())?;
    let mut names: Vec<String> = Vec::new();
    let mut after_key_name = false;
    let mut bc = pblock.walk();
    for child in pblock.children(&mut bc) {
        if child.kind() == "semicolon" {
            after_key_name = true;
            continue;
        }
        if after_key_name && child.is_named() && child.kind() != "comma" {
            let text = child
                .utf8_text(source)
                .map_err(|error| format!("primary-key name is not UTF-8: {error}"))?;
            names.push(text.trim_matches('"').to_string());
        }
    }
    Ok(names)
}

/// True if `method` is a record API method implemented by the local runtime.
///
/// The test router consumes this same capability predicate so classification
/// cannot drift from execution support.
pub fn supports_record_method(method: &str) -> bool {
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
/// `table_name` is the declared subtype of the receiver record variable and
/// `handle` identifies that variable's own view (filters/cursor/buffer) over
/// the shared table store. `args_node` is the call's `argument_list` (so
/// field-reference arguments — e.g. the first arg of `SetRange` — can be read
/// as field names rather than evaluated as variables).
pub(crate) fn dispatch_record_method(
    table_name: &str,
    handle: u64,
    method: &str,
    args_node: Option<Node<'_>>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Consume the statement-position marker before any nested evaluation:
    // a statement-position `Get`/`Find*` miss is a runtime error in BC, an
    // expression-position one returns false.
    let stmt_position = std::mem::take(&mut ctx.stmt_position);
    if !records_enabled(ctx) {
        return records_disabled_error();
    }
    let nodes: Vec<Node> = args_node.map(arg_expr_nodes).unwrap_or_default();
    let lower = method.to_ascii_lowercase();

    // CalcFields takes field-name args (not value expressions): evaluate each
    // named FlowField's CalcFormula and write the result into the buffer so a
    // subsequent plain read sees it. Handled before the value-eval loop so the
    // field-name nodes are never evaluated as variables.
    if lower == "calcfields" {
        return dispatch_calcfields(table_name, handle, &nodes, source, ctx);
    }

    // SetCurrentKey takes only field references. Handle it before the general
    // expression loop so later field arguments are not evaluated as variables.
    if lower == "setcurrentkey" {
        if nodes.is_empty() {
            return err("SetCurrentKey: requires at least one field");
        }
        let key = match ensure_store(ctx, table_name) {
            Ok(key) => key,
            Err(error) => return err(error),
        };
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let mut field_nos = Vec::with_capacity(nodes.len());
        for node in &nodes {
            let name = node_text(*node, source);
            if name.is_empty() {
                return err("SetCurrentKey: field name is empty");
            }
            match store.resolve_field(&name) {
                Ok(field_no) => field_nos.push(field_no),
                Err(error) => return err(format!("SetCurrentKey: {error}")),
            }
        }
        let mut view = store.take_view(handle);
        store.record.set_current_key_in(&mut view, field_nos);
        store.put_view(handle, view);
        return Eval::Normal(Value::Boolean(true));
    }

    // Field-reference methods: arg 0 is a field name (raw text), the rest values.
    let field_methods = matches!(lower.as_str(), "setrange" | "setfilter");

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
        match store.resolve_field(&fname) {
            Ok(field_no) => Some(field_no),
            Err(error) => return err(format!("{method}: {error}")),
        }
    } else {
        None
    };

    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let mut view = store.take_view(handle);
    let result = run_record_method(
        store,
        &mut view,
        &lower,
        method,
        field_no,
        values,
        stmt_position,
    );
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    store.put_view(handle, view);
    result
}

/// The record-method body proper, operating on a taken-out view so early
/// returns cannot lose the view (the caller reinserts it unconditionally).
fn run_record_method(
    store: &mut RecordStore,
    view: &mut RecordView,
    lower: &str,
    method: &str,
    field_no: Option<FieldNo>,
    values: Vec<Value>,
    stmt_position: bool,
) -> Eval {
    use crate::mock::record::RecordError;

    // Map a mutation failure with BC statement/expression semantics:
    // a statement-position failure raises; `if Rec.Insert() then` takes the
    // false branch. Capability errors (trigger execution) always raise.
    let mutation_result = |method: &str, result: Result<(), RecordError>| match result {
        Ok(()) => Eval::Normal(Value::Boolean(true)),
        Err(error @ RecordError::TriggerExecutionUnsupported(_)) => {
            err(format!("{method}: {error}"))
        }
        Err(error) if stmt_position => err(format!("{method}: {error}")),
        Err(_) => Eval::Normal(Value::Boolean(false)),
    };
    // A find-class miss errors in statement position and yields false in
    // expression position.
    let find_result = |method: &str, table: &str, found: Result<bool, RecordError>| match found {
        Ok(true) => Eval::Normal(Value::Boolean(true)),
        Ok(false) if stmt_position => err(format!(
            "{method}: no '{table}' record matches the current filters"
        )),
        Ok(false) => Eval::Normal(Value::Boolean(false)),
        Err(error) => err(format!("{method}: {error}")),
    };

    match lower {
        "init" => {
            if let Err(error) = require_no_args("Init", &values) {
                return err(error);
            }
            store.record.init_in(view);
            Eval::Normal(Value::Empty)
        }
        "reset" => {
            if let Err(error) = require_no_args("Reset", &values) {
                return err(error);
            }
            store.record.reset_in(view);
            Eval::Normal(Value::Empty)
        }
        "insert" => match optional_boolean("Insert", &values) {
            Ok(run_trigger) => mutation_result("Insert", store.record.insert_in(view, run_trigger)),
            Err(error) => err(error),
        },
        "modify" => match optional_boolean("Modify", &values) {
            Ok(run_trigger) => mutation_result("Modify", store.record.modify_in(view, run_trigger)),
            Err(error) => err(error),
        },
        "delete" => match optional_boolean("Delete", &values) {
            Ok(run_trigger) => mutation_result("Delete", store.record.delete_in(view, run_trigger)),
            Err(error) => err(error),
        },
        "get" => {
            if values.is_empty() {
                return err("Get: requires at least one primary-key value");
            }
            if values.len() != store.record.primary_key_len() {
                return err(format!(
                    "Get: local record runtime requires all {} primary-key values (got {}); \
                     partial composite-key lookup requires live Business Central",
                    store.record.primary_key_len(),
                    values.len()
                ));
            }
            // Coerce each key part to the declared PK field type so a Text
            // literal matches a Code key and an Integer a Decimal one.
            let pk_fields: Vec<FieldNo> = store.record.primary_key_fields().to_vec();
            let mut key_values = Vec::with_capacity(values.len());
            for (field, value) in pk_fields.into_iter().zip(values) {
                match store.coerce_to_field(field, value) {
                    Ok(coerced) => key_values.push(coerced),
                    Err(error) => return err(format!("Get: {error}")),
                }
            }
            match store.record.get_in(view, key_values) {
                Ok(()) => Eval::Normal(Value::Boolean(true)),
                Err(_) if stmt_position => err(format!(
                    "Get: the record does not exist in table '{}'",
                    store.record.table_name
                )),
                Err(_) => Eval::Normal(Value::Boolean(false)),
            }
        }
        "setrange" => {
            let f = field_no.unwrap();
            match values.len() {
                0 => {
                    store.record.clear_filter_in(view, f);
                    Eval::Normal(Value::Empty)
                }
                1 => {
                    store
                        .record
                        .set_range_in(view, f, values[0].clone(), values[0].clone());
                    Eval::Normal(Value::Empty)
                }
                2 => {
                    store
                        .record
                        .set_range_in(view, f, values[0].clone(), values[1].clone());
                    Eval::Normal(Value::Empty)
                }
                count => err(format!(
                    "SetRange: expected at most two values after the field, got {count}"
                )),
            }
        }
        "setfilter" => {
            let f = field_no.unwrap();
            let raw = match values.first() {
                Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
                Some(value) => {
                    return err(format!(
                        "SetFilter: filter expression must be Text or Code, got {}",
                        value.type_name()
                    ))
                }
                None => return err("SetFilter: missing filter expression"),
            };
            // Single left-to-right pass (shared with StrSubstNo): substituted
            // text is never re-scanned, so a value containing `%1` stays
            // literal and `%10` cannot be corrupted by the `%1` replacement.
            let expr = match crate::interpreter::dispatch::substitute_placeholders_with(
                &raw,
                values.len() - 1,
                |n| render_filter_value(&values[n]),
            ) {
                Ok(expr) => expr,
                Err(error) => return err(format!("SetFilter: {error}")),
            };
            match store.record.set_filter_in(view, f, &expr) {
                Ok(()) => Eval::Normal(Value::Empty),
                Err(e) => err(format!("SetFilter: {e}")),
            }
        }
        "findset" => {
            if values.len() > 2
                || values
                    .iter()
                    .any(|value| !matches!(value, Value::Boolean(_)))
            {
                return err("FindSet: expects up to two optional Boolean arguments");
            }
            let table = store.record.table_name.clone();
            find_result("FindSet", &table, store.record.find_first_in(view))
        }
        "findfirst" => {
            if let Err(error) = require_no_args("FindFirst", &values) {
                return err(error);
            }
            let table = store.record.table_name.clone();
            find_result("FindFirst", &table, store.record.find_first_in(view))
        }
        "findlast" => {
            if let Err(error) = require_no_args("FindLast", &values) {
                return err(error);
            }
            let table = store.record.table_name.clone();
            find_result("FindLast", &table, store.record.find_last_in(view))
        }
        "find" => {
            let direction = match values.as_slice() {
                [Value::Text(direction)] | [Value::Code(direction)] => direction,
                _ => return err("Find: expects exactly one Text or Code direction argument"),
            };
            let mut chars = direction.chars();
            let Some(direction) = chars.next() else {
                return err("Find: direction cannot be empty");
            };
            if chars.next().is_some() {
                return err(
                    "Find: local record runtime supports only the single-character '-' and '+' directions",
                );
            }
            let table = store.record.table_name.clone();
            find_result("Find", &table, store.record.find_in(view, direction))
        }
        "next" => {
            let steps = match values.as_slice() {
                [] => 1,
                [Value::Integer(steps)] => match i32::try_from(*steps) {
                    Ok(steps) => steps,
                    Err(_) => {
                        return err(format!("Next: step count {steps} is outside Integer range"))
                    }
                },
                _ => return err("Next: expects one optional Integer step count"),
            };
            match store.record.next_in(view, steps) {
                Ok(moved) => Eval::Normal(Value::Integer(moved as i64)),
                Err(e) => err(format!("Next: {e}")),
            }
        }
        "count" | "countapprox" => {
            if let Err(error) = require_no_args(method, &values) {
                return err(error);
            }
            Eval::Normal(Value::Integer(store.record.count_in(view) as i64))
        }
        "isempty" => {
            if let Err(error) = require_no_args("IsEmpty", &values) {
                return err(error);
            }
            Eval::Normal(Value::Boolean(store.record.is_empty_in(view)))
        }
        "deleteall" => {
            let run_trigger = match optional_boolean("DeleteAll", &values) {
                Ok(run_trigger) => run_trigger,
                Err(error) => return err(error),
            };
            // One pass over the table (collect matching keys, remove them)
            // instead of the O(n² log n) find-first-then-delete loop.
            match store.record.delete_all_in(view, run_trigger) {
                Ok(_) => Eval::Normal(Value::Empty),
                Err(error) => err(format!("DeleteAll: {error}")),
            }
        }
        other => err(format!("unsupported record method: {other}")),
    }
}

/// Read one buffer field of the view identified by `handle`, falling back to
/// the field's typed default (BC zero-initialisation) when never assigned.
fn read_buffer_field(store: &RecordStore, handle: u64, field: FieldNo) -> Value {
    store
        .views
        .get(&handle)
        .and_then(|view| store.record.field_get_in(view, field).cloned())
        .filter(|value| !matches!(value, Value::Empty))
        .or_else(|| store.field_defaults.get(&field).cloned())
        .unwrap_or(Value::Empty)
}

/// Read a record field by name: `x := Rec."Field"`.
///
/// A FlowField with a parseable `CalcFormula` is computed on read (auto-calc);
/// any other field returns its buffer value, or the field's typed zero value
/// when it was never assigned (BC zero-initialises every field).
pub(crate) fn field_get(
    table_name: &str,
    handle: u64,
    field_name: &str,
    ctx: &mut DispatchCtx,
) -> Eval {
    if !records_enabled(ctx) {
        return records_disabled_error();
    }
    let key = match ensure_store(ctx, table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    let (f, formula) = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let f = match store.resolve_field(field_name) {
            Ok(field_no) => field_no,
            Err(error) => return err(error),
        };
        (f, store.flowfields.get(&f).cloned())
    };
    if let Some(formula) = formula {
        return eval_flowfield(ctx, &key, handle, &formula);
    }
    let store = ctx.records.get(&key).expect("store just ensured");
    Eval::Normal(read_buffer_field(store, handle, f))
}

/// `Rec.CalcFields(F1, F2, …)` — evaluate each named FlowField and store the
/// result into the current buffer. Non-FlowField (or unparseable) args are
/// ignored, matching BC's tolerance of explicitly-listed normal fields.
fn dispatch_calcfields(
    table_name: &str,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    ctx: &mut DispatchCtx,
) -> Eval {
    if nodes.is_empty() {
        return err("CalcFields: requires at least one FlowField");
    }
    let key = match ensure_store(ctx, table_name) {
        Ok(k) => k,
        Err(e) => return err(e),
    };
    // Resolve each field name to its number + formula up front (one borrow).
    let targets: Vec<(FieldNo, Option<CalcFormula>)> = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let mut targets = Vec::with_capacity(nodes.len());
        for node in nodes {
            let name = node_text(*node, source);
            let field_no = match store.resolve_field(&name) {
                Ok(field_no) => field_no,
                Err(error) => return err(format!("CalcFields: {error}")),
            };
            targets.push((field_no, store.flowfields.get(&field_no).cloned()));
        }
        targets
    };
    for (field_no, formula) in targets {
        let Some(formula) = formula else {
            return err(format!(
                "CalcFields: field number {field_no} is not a supported FlowField"
            ));
        };
        match eval_flowfield(ctx, &key, handle, &formula) {
            Eval::Normal(v) => {
                let store = ctx.records.get_mut(&key).expect("store just ensured");
                let mut view = store.take_view(handle);
                store.record.field_set_in(&mut view, field_no, v);
                store.put_view(handle, view);
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
fn eval_flowfield(
    ctx: &mut DispatchCtx,
    current_key: &str,
    handle: u64,
    formula: &CalcFormula,
) -> Eval {
    // 1. Resolve any FIELD(...) references against the calculating record's
    //    buffer first, before borrowing the referenced store (they may be the
    //    same store for a self-referencing FlowField).
    let field_values: Vec<Option<Value>> = {
        let store = ctx.records.get(current_key).expect("store just ensured");
        let mut values = Vec::with_capacity(formula.where_clause.len());
        for condition in &formula.where_clause {
            let value = match &condition.value {
                WhereValue::Field(name) => {
                    let f = match store.resolve_field(name) {
                        Ok(field_no) => field_no,
                        Err(error) => return err(format!("FlowField: {error}")),
                    };
                    Some(read_buffer_field(store, handle, f))
                }
                _ => None,
            };
            values.push(value);
        }
        values
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
    let target = match formula.field_name.as_deref() {
        Some(name) => match store.resolve_field(name) {
            Ok(field_no) => Some(field_no),
            Err(error) => return err(format!("FlowField target: {error}")),
        },
        None => None,
    };

    let mut conditions: Vec<(FieldNo, FlowFilter)> = Vec::new();
    for (i, cond) in formula.where_clause.iter().enumerate() {
        let field_no = match store.resolve_field(&cond.field) {
            Ok(field_no) => field_no,
            Err(error) => return err(format!("FlowField condition: {error}")),
        };
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

    match store.record.calc_flow(&conditions, target, agg) {
        Ok(value) => Eval::Normal(value),
        Err(error) => err(format!("FlowField calculation failed: {error}")),
    }
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
    // `WHERE(Flag = CONST(true))` — a Boolean constant, not text.
    if t.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    Value::Text(t.to_string())
}

/// If `lhs_node` is a record field access (`Rec."Field"`) whose receiver is a
/// bound `Value::Record`, set the field to `rhs_val` and return `Some(result)`.
/// Returns `None` when the LHS is not a record field access. The value is
/// coerced to the field's declared type (Code caselessness, Integer overflow
/// trap, Decimal promotion) before it is stored.
pub(crate) fn try_field_assign(
    lhs_node: Node<'_>,
    source: &[u8],
    rhs_val: &Value,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let (recv, field_name) = record_field_access(lhs_node, source)?;
    let (table_name, handle) = record_binding(&recv, stack, ctx)?;
    if !records_enabled(ctx) {
        return Some(records_disabled_error());
    }
    let key = match ensure_store(ctx, &table_name) {
        Ok(k) => k,
        Err(e) => return Some(err(e)),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let f = match store.resolve_field(&field_name) {
        Ok(field_no) => field_no,
        Err(error) => return Some(err(error)),
    };
    let coerced = match store.coerce_to_field(f, rhs_val.clone()) {
        Ok(value) => value,
        Err(error) => return Some(err(error)),
    };
    let mut view = store.take_view(handle);
    store.record.field_set_in(&mut view, f, coerced);
    store.put_view(handle, view);
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

/// True if `method` is a `List of [T]` method implemented by the local runtime.
pub fn supports_list_method(method: &str) -> bool {
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
        "add" if args.len() == 1 => {
            items.push(args[0].clone());
            Eval::Normal(Value::Boolean(true))
        }
        "add" => err("List.Add expects exactly one value"),
        "count" if args.is_empty() => match i64::try_from(items.len()) {
            Ok(count) => Eval::Normal(Value::Integer(count)),
            Err(_) => err("List.Count exceeds the supported Integer range"),
        },
        "count" => err("List.Count expects no arguments"),
        "get" => match list_index("List.Get", &args, items.len()) {
            Ok(index) => Eval::Normal(items[index].clone()),
            Err(error) => err(error),
        },
        "contains" => match args.as_slice() {
            [needle] => Eval::Normal(Value::Boolean(items.iter().any(|item| item == needle))),
            _ => err("List.Contains expects exactly one value"),
        },
        "indexof" => match args.as_slice() {
            [needle] => {
                let position = items.iter().position(|item| item == needle);
                match position {
                    Some(position) => match i64::try_from(position + 1) {
                        Ok(position) => Eval::Normal(Value::Integer(position)),
                        Err(_) => err("List.IndexOf result exceeds the supported Integer range"),
                    },
                    None => Eval::Normal(Value::Integer(0)),
                }
            }
            _ => err("List.IndexOf expects exactly one value"),
        },
        "removeat" => match list_index("List.RemoveAt", &args, items.len()) {
            Ok(index) => {
                items.remove(index);
                Eval::Normal(Value::Boolean(true))
            }
            Err(error) => err(error),
        },
        "remove" => match args.as_slice() {
            [needle] => {
                if let Some(pos) = items.iter().position(|item| item == needle) {
                    items.remove(pos);
                    Eval::Normal(Value::Boolean(true))
                } else {
                    Eval::Normal(Value::Boolean(false))
                }
            }
            _ => err("List.Remove expects exactly one value"),
        },
        "set" if args.len() == 2 => match list_index("List.Set", &args[..1], items.len()) {
            Ok(index) => {
                items[index] = args[1].clone();
                Eval::Normal(Value::Boolean(true))
            }
            Err(error) => err(error),
        },
        "set" => err("List.Set expects exactly an Integer index and one value"),
        other => err(format!("unsupported List method: {other}")),
    }
}

fn list_index(method: &str, args: &[Value], len: usize) -> Result<usize, String> {
    let index = match args {
        [Value::Integer(index)] => *index,
        _ => return Err(format!("{method} expects exactly one Integer index")),
    };
    let zero_based = index
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < len)
        .ok_or_else(|| format!("{method}: index {index} out of range 1..{len}"))?;
    Ok(zero_based)
}

/// True if `method` is a `Text`/`Code` instance method implemented by the
/// local runtime (`s.Contains(...)`, `s.Split(...)`, …).
pub fn supports_text_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "contains"
            | "startswith"
            | "endswith"
            | "indexof"
            | "lastindexof"
            | "replace"
            | "split"
            | "trim"
            | "trimstart"
            | "trimend"
            | "tolower"
            | "toupper"
            | "substring"
    )
}

/// Execute a `Text`/`Code` instance method on the string bound to `recv`.
pub(crate) fn dispatch_text_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
) -> Eval {
    let s = match stack.lookup(recv) {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        _ => return err(format!("text variable '{recv}' is not bound")),
    };
    let text_arg = |v: &Value| -> Option<String> {
        match v {
            Value::Text(t) | Value::Code(t) => Some(t.clone()),
            Value::Char(c) => Some(c.to_string()),
            _ => None,
        }
    };
    let lower = method.to_ascii_lowercase();
    match lower.as_str() {
        "contains" | "startswith" | "endswith" => match args.as_slice() {
            [needle] => match text_arg(needle) {
                Some(needle) => {
                    let result = match lower.as_str() {
                        "contains" => s.contains(&needle),
                        "startswith" => s.starts_with(&needle),
                        _ => s.ends_with(&needle),
                    };
                    Eval::Normal(Value::Boolean(result))
                }
                None => err(format!(
                    "Text.{method} expects a Text argument, got {}",
                    needle.type_name()
                )),
            },
            _ => err(format!("Text.{method} expects exactly one Text argument")),
        },
        "indexof" | "lastindexof" => match args.as_slice() {
            [needle] => match text_arg(needle) {
                Some(needle) if needle.is_empty() => Eval::Normal(Value::Integer(0)),
                Some(needle) => {
                    let byte_pos = if lower == "indexof" {
                        s.find(&needle)
                    } else {
                        s.rfind(&needle)
                    };
                    let result = byte_pos
                        .map(|i| s[..i].chars().count() as i64 + 1)
                        .unwrap_or(0);
                    Eval::Normal(Value::Integer(result))
                }
                None => err(format!(
                    "Text.{method} expects a Text argument, got {}",
                    needle.type_name()
                )),
            },
            _ => err(format!("Text.{method} expects exactly one Text argument")),
        },
        "replace" => match args.as_slice() {
            [old, new] => match (text_arg(old), text_arg(new)) {
                (Some(old), Some(new)) if !old.is_empty() => {
                    Eval::Normal(Value::Text(s.replace(&old, &new)))
                }
                (Some(_), Some(_)) => err("Text.Replace: the old value cannot be empty"),
                _ => err("Text.Replace expects (Text, Text)"),
            },
            _ => err("Text.Replace expects exactly two Text arguments"),
        },
        "split" => {
            let mut separators = Vec::with_capacity(args.len());
            for arg in &args {
                match text_arg(arg) {
                    Some(sep) if !sep.is_empty() => separators.push(sep),
                    Some(_) => return err("Text.Split: separators cannot be empty"),
                    None => {
                        return err(format!(
                            "Text.Split expects Text separators, got {}",
                            arg.type_name()
                        ))
                    }
                }
            }
            if separators.is_empty() {
                return Eval::Normal(Value::List(vec![Value::Text(s)]));
            }
            let mut parts = vec![s];
            for sep in &separators {
                parts = parts
                    .into_iter()
                    .flat_map(|part| {
                        part.split(sep.as_str())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect();
            }
            Eval::Normal(Value::List(parts.into_iter().map(Value::Text).collect()))
        }
        "trim" | "trimstart" | "trimend" => {
            if !args.is_empty() {
                return err(format!("Text.{method} expects no arguments"));
            }
            let trimmed = match lower.as_str() {
                "trim" => s.trim(),
                "trimstart" => s.trim_start(),
                _ => s.trim_end(),
            };
            Eval::Normal(Value::Text(trimmed.to_string()))
        }
        "tolower" => {
            if !args.is_empty() {
                return err("Text.ToLower expects no arguments");
            }
            Eval::Normal(Value::Text(s.to_lowercase()))
        }
        "toupper" => {
            if !args.is_empty() {
                return err("Text.ToUpper expects no arguments");
            }
            Eval::Normal(Value::Text(s.to_uppercase()))
        }
        "substring" => {
            let (start, length) = match args.as_slice() {
                [Value::Integer(start)] => (*start, None),
                [Value::Integer(start), Value::Integer(length)] => (*start, Some(*length)),
                _ => return err("Text.Substring expects (Integer[, Integer])"),
            };
            let chars: Vec<char> = s.chars().collect();
            if start < 1 || (start as usize) > chars.len() + 1 {
                return err(format!(
                    "Text.Substring: start position {start} is out of range for a {}-character string",
                    chars.len()
                ));
            }
            let zero = start as usize - 1;
            match length {
                None => Eval::Normal(Value::Text(chars[zero..].iter().collect())),
                Some(length) if length < 0 => {
                    err("Text.Substring: length must be >= 0".to_string())
                }
                Some(length) => {
                    let end = zero + length as usize;
                    if end > chars.len() {
                        return err(format!(
                            "Text.Substring: start {start} plus length {length} exceeds the string length {}",
                            chars.len()
                        ));
                    }
                    Eval::Normal(Value::Text(chars[zero..end].iter().collect()))
                }
            }
        }
        other => err(format!("unsupported Text method: {other}")),
    }
}

/// True if `method` is a `Dictionary of [K, V]` method implemented by the
/// local runtime. `Get` is intentionally absent: its common two-argument
/// `var`-out form needs by-reference write-back the builtin path does not
/// have, so Dictionary reads route to live BC (the one-argument returning
/// form still executes if a body reaches the interpreter).
pub fn supports_dict_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "add" | "set" | "containskey" | "remove" | "count" | "keys" | "values"
    )
}

/// Serialise a dictionary key value into the `Dict` map's string key space.
fn dict_key(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Text(s) => s.clone(),
        // Code keys are caseless.
        Value::Code(s) => s.to_uppercase(),
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(d) => d.normalize().to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Date(d) => format!("D{d}"),
        Value::Time(t) => format!("T{t}"),
        Value::DateTime(dt) => format!("DT{dt}"),
        Value::Guid(g) => g.to_uppercase(),
        other => {
            return Err(format!(
                "Dictionary keys of type {} are not supported by the local runtime",
                other.type_name()
            ))
        }
    })
}

/// Execute a `Dictionary of [K, V]` method call on the dictionary bound to
/// `recv`.
pub(crate) fn dispatch_dict_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
) -> Eval {
    let lower = method.to_ascii_lowercase();
    let Some(slot) = stack.lookup_mut(recv) else {
        return err(format!("dictionary variable '{recv}' is not bound"));
    };
    let Value::Dict(entries) = slot else {
        return err(format!("'{recv}' is not a Dictionary"));
    };
    match lower.as_str() {
        "add" => match args.as_slice() {
            [key, value] => match dict_key(key) {
                Ok(key_text) => match entries.entry(key_text) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        err("Dictionary.Add: the key already exists")
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(value.clone());
                        Eval::Normal(Value::Empty)
                    }
                },
                Err(error) => err(error),
            },
            _ => err("Dictionary.Add expects exactly a key and a value"),
        },
        "set" => match args.as_slice() {
            [key, value] => match dict_key(key) {
                Ok(key_text) => {
                    entries.insert(key_text, value.clone());
                    Eval::Normal(Value::Empty)
                }
                Err(error) => err(error),
            },
            _ => err("Dictionary.Set expects exactly a key and a value"),
        },
        "get" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => match entries.get(&key_text) {
                    Some(value) => Eval::Normal(value.clone()),
                    None => err("Dictionary.Get: the key does not exist"),
                },
                Err(error) => err(error),
            },
            _ => err("Dictionary.Get with a var out-parameter requires live BC; \
                 only the one-argument returning form runs locally"),
        },
        "containskey" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => Eval::Normal(Value::Boolean(entries.contains_key(&key_text))),
                Err(error) => err(error),
            },
            _ => err("Dictionary.ContainsKey expects exactly one key"),
        },
        "remove" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => Eval::Normal(Value::Boolean(entries.remove(&key_text).is_some())),
                Err(error) => err(error),
            },
            _ => err("Dictionary.Remove expects exactly one key"),
        },
        "count" => {
            if !args.is_empty() {
                return err("Dictionary.Count expects no arguments");
            }
            match i64::try_from(entries.len()) {
                Ok(count) => Eval::Normal(Value::Integer(count)),
                Err(_) => err("Dictionary.Count exceeds the supported Integer range"),
            }
        }
        "keys" => {
            if !args.is_empty() {
                return err("Dictionary.Keys expects no arguments");
            }
            Eval::Normal(Value::List(
                entries.keys().cloned().map(Value::Text).collect(),
            ))
        }
        "values" => {
            if !args.is_empty() {
                return err("Dictionary.Values expects no arguments");
            }
            Eval::Normal(Value::List(entries.values().cloned().collect()))
        }
        other => err(format!("unsupported Dictionary method: {other}")),
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
    if lower.starts_with("dictionary of") {
        return Some(Value::Dict(std::collections::BTreeMap::new()));
    }
    if lower == "variant" {
        return Some(Value::Variant(Box::new(Value::Null)));
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

fn require_no_args(method: &str, values: &[Value]) -> Result<(), String> {
    if values.is_empty() {
        Ok(())
    } else {
        Err(format!("{method}: expects no arguments"))
    }
}

fn optional_boolean(method: &str, values: &[Value]) -> Result<bool, String> {
    match values {
        [] => Ok(false),
        [Value::Boolean(value)] => Ok(*value),
        [value] => Err(format!(
            "{method}: optional RunTrigger argument must be Boolean, got {}",
            value.type_name()
        )),
        _ => Err(format!(
            "{method}: expects at most one optional Boolean argument"
        )),
    }
}

fn render_filter_value(v: &Value) -> Result<String, String> {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => Ok(n.to_string()),
        Value::Decimal(d) => Ok(d.normalize().to_string()),
        Value::Text(s) | Value::Code(s) => Ok(s.clone()),
        Value::Boolean(b) => Ok(b.to_string()),
        value => Err(format!(
            "placeholder value type {} is not supported by the local record runtime",
            value.type_name()
        )),
    }
}
