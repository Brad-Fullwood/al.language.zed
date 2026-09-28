//! `Validate`, `TestField`, `ModifyAll`, `CalcSums`, `CalcFields` and
//! FlowField evaluation.
//!
//! [`validate_relation`] also answers the test router's question of whether
//! a field's `TableRelation` is one the local runtime can check, so the two
//! cannot disagree on what needs live Business Central.

use std::sync::Arc;

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::mock::calcformula_parser::{CalcFormula, FormulaType, WhereValue};
use crate::mock::filter;
use crate::mock::record::{FieldNo, FlowAgg, FlowFilter};

use super::crud::{write_all, RecordWrite};
use super::field_access::{read_buffer_field, record_value_on, x_rec_of};
use super::helpers::node_text;
use super::store::{ensure_store, TableRef};
use super::table_meta::load_table_meta;

/// `Rec.Validate(Field[, Value])` — assign the field (when a value is given),
/// check its TableRelation, then run its OnValidate trigger with the record
/// as it was before as `xRec`.
pub(super) fn dispatch_validate(
    table: &TableRef,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let (field_node, value_node) = match nodes {
        [field] => (*field, None),
        [field, value] => (*field, Some(*value)),
        _ => return eval_error("Validate expects (Field[, Value])"),
    };
    let field_name = node_text(field_node, source)
        .unquote_identifier()
        .into_owned();
    let value = match value_node.map(|node| eval_expr(node, source, stack, ctx)) {
        None => None,
        Some(Eval::Normal(value)) => Some(value),
        Some(other) => return other,
    };
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    let field_no = match ctx.records[&key].resolve_field(&field_name) {
        Ok(field_no) => field_no,
        Err(error) => return eval_error(format!("Validate: {error}")),
    };
    let x_rec = x_rec_of(table, handle, false, ctx);
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    if let Some(value) = value {
        let coerced = match store.coerce_to_field(field_no, value) {
            Ok(value) => value,
            Err(error) => return eval_error(format!("Validate: {error}")),
        };
        let mut view = store.take_view(handle);
        store.record.field_set_in(&mut view, field_no, coerced);
        store.put_view(handle, view);
    }
    let store = ctx.records.get(&key).expect("store just ensured");
    let current = read_buffer_field(store, handle, field_no);
    let blank = store
        .field_defaults
        .get(&field_no)
        .is_some_and(|default| *default == current);
    if !blank {
        if let Err(error) = check_table_relation(&table.name, &field_name, &current, ctx) {
            return eval_error(error);
        }
    }
    let validate_event = |event: &str, stack: &mut ScopeStack, ctx: &mut DispatchCtx| {
        let mut values = vec![
            record_value_on(table, handle),
            x_rec.clone(),
            Value::Integer(0),
        ];
        crate::interpreter::dispatch::events::raise(
            "table",
            &table.name,
            event,
            &field_name,
            &["Rec", "xRec", "CurrFieldNo"],
            &mut values,
            None,
            stack,
            ctx,
        )
    };
    if let Err(error) = validate_event("OnBeforeValidateEvent", stack, ctx) {
        return error;
    }
    if let Some(error @ Eval::Error(_)) = crate::interpreter::dispatch::table_code::run_table_code(
        record_value_on(table, handle),
        x_rec.clone(),
        crate::interpreter::dispatch::table_code::TableCode::FieldTrigger {
            field: &field_name,
            trigger: "OnValidate",
        },
        Vec::new(),
        stack,
        ctx,
    ) {
        return error;
    }
    match validate_event("OnAfterValidateEvent", stack, ctx) {
        Ok(()) => Eval::Normal(Value::Empty),
        Err(error) => error,
    }
}

/// How `Validate` checks field `field_name` of table `table_name` against
/// its `TableRelation`: `Ok(None)` without one, `Ok(Some((table, field)))`
/// naming the related table and, when the relation names one, its field
/// (otherwise its single primary-key field). `Err` says why the check needs
/// live BC: a conditional or filtered relation, a related table outside the
/// workspace, or a composite key with no field named. The test router asks
/// the same question, so the two cannot disagree.
pub fn validate_relation(
    source: &dyn al_types::ProcedureSource,
    table_name: &str,
    field_name: &str,
) -> Result<Option<(String, Option<String>)>, String> {
    let relation = source
        .find_object_of_kind(table_name, &["table"])
        .and_then(|path| {
            let (text, tree) = source.get_cached_parse(&path)?;
            crate::interpreter::dispatch::table_code::table_relation_in(
                tree.root_node(),
                text.as_bytes(),
                table_name,
                field_name,
            )
        });
    let Some(relation) = relation else {
        return Ok(None);
    };
    let Some((target, target_field)) =
        crate::interpreter::dispatch::table_code::relation_target(&relation)
    else {
        return Err(format!(
            "the TableRelation of {field_name} ('{relation}') needs live Business Central to check"
        ));
    };
    let meta = load_table_meta(source, &target).map_err(|_| {
        format!(
            "{field_name} relates to table '{target}', which is not in the workspace; \
             checking it needs live Business Central"
        )
    })?;
    match &target_field {
        Some(name) if !meta.field_by_name.contains_key(&name.to_ascii_lowercase()) => Err(format!(
            "{field_name} relates to field '{name}', which table '{target}' does not declare"
        )),
        None if meta.pk_fields.len() != 1 => Err(format!(
            "{field_name} relates to table '{target}' by a composite key; \
             checking it needs live Business Central"
        )),
        _ => Ok(Some((target, target_field))),
    }
}

/// BC's Validate refuses a value its field's `TableRelation` does not
/// contain. Checks the value exists in the related workspace table, or
/// says why that needs live BC (see [`validate_relation`]).
fn check_table_relation(
    table_name: &str,
    field_name: &str,
    value: &Value,
    ctx: &mut DispatchCtx,
) -> Result<(), String> {
    let source = Arc::clone(&ctx.source);
    let Some((target, target_field)) = validate_relation(&*source, table_name, field_name)
        .map_err(|reason| format!("Validate: {reason}"))?
    else {
        return Ok(());
    };
    let key = ensure_store(ctx, &TableRef::persistent(target.clone()))?;
    let store = ctx.records.get(&key).expect("store just ensured");
    let related_field = match &target_field {
        Some(name) => store.resolve_field(name)?,
        None => store.record.primary_key_fields()[0],
    };
    let related_value = store.coerce_to_field(related_field, value.clone())?;
    let mut view = store.record.new_view();
    store.record.set_range_in(
        &mut view,
        related_field,
        related_value.clone(),
        related_value,
    );
    if store.record.count_in(&view) == 0 {
        return Err(format!(
            "The field {field_name} of table {table_name} contains a value ({}) that cannot be found in the related table ({target}).",
            crate::interpreter::dispatch::render_value(value)
        ));
    }
    Ok(())
}

/// `Rec.TestField(Field[, Value])` — raise unless `Field` has a value (is
/// not its type's zero) or, with `Value`, equals it.
pub(super) fn dispatch_testfield(
    table: &TableRef,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let (field_node, expected_node) = match nodes {
        [field] => (*field, None),
        [field, expected] => (*field, Some(*expected)),
        _ => return eval_error("TestField expects (Field[, Value])"),
    };
    let expected = match expected_node.map(|node| eval_expr(node, source, stack, ctx)) {
        None => None,
        Some(Eval::Normal(value)) => Some(value),
        Some(other) => return other,
    };
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let field_name = node_text(field_node, source)
        .unquote_identifier()
        .into_owned();
    let field_no = match store.resolve_field(&field_name) {
        Ok(field_no) => field_no,
        Err(error) => return eval_error(format!("TestField: {error}")),
    };
    let default = store
        .field_defaults
        .get(&field_no)
        .cloned()
        .unwrap_or(Value::Empty);
    let view = store.take_view(handle);
    let current = store
        .record
        .field_get_in(&view, field_no)
        .cloned()
        .unwrap_or_else(|| default.clone());
    store.put_view(handle, view);
    let table_name = store.record.table_name.clone();
    match expected {
        None if current == default || matches!(current, Value::Empty | Value::Null) => eval_error(
            format!("{field_name} must have a value in {table_name}. It cannot be zero or empty."),
        ),
        None => Eval::Normal(Value::Empty),
        Some(expected) => match store.coerce_to_field(field_no, expected) {
            Ok(expected) if expected == current => Eval::Normal(Value::Empty),
            Ok(expected) => eval_error(format!(
                "{field_name} must be equal to '{}' in {table_name}. Current value is '{}'.",
                crate::interpreter::dispatch::render_value(&expected),
                crate::interpreter::dispatch::render_value(&current)
            )),
            Err(error) => eval_error(format!("TestField: {error}")),
        },
    }
}

/// `Rec.ModifyAll(Field, Value[, RunTrigger])` — set `Field` on every row the
/// view's filters select.
pub(super) fn dispatch_modifyall(
    table: &TableRef,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let (field_node, value_node) = match nodes {
        [field, value] | [field, value, _] => (*field, *value),
        _ => return eval_error("ModifyAll expects (Field, Value[, RunTrigger])"),
    };
    let value = match eval_expr(value_node, source, stack, ctx) {
        Eval::Normal(value) => value,
        other => return other,
    };
    let run_trigger = match nodes.get(2) {
        None => false,
        Some(node) => match eval_expr(*node, source, stack, ctx) {
            Eval::Normal(Value::Boolean(flag)) => flag,
            Eval::Normal(_) => return eval_error("ModifyAll: RunTrigger must be a Boolean"),
            other => return other,
        },
    };
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let field_no = match store.resolve_field(&node_text(field_node, source)) {
        Ok(field_no) => field_no,
        Err(error) => return eval_error(format!("ModifyAll: {error}")),
    };
    let value = match store.coerce_to_field(field_no, value) {
        Ok(value) => value,
        Err(error) => return eval_error(format!("ModifyAll: {error}")),
    };
    write_all(
        table,
        handle,
        (RecordWrite::Modify, run_trigger),
        Some((field_no, value)),
        stack,
        ctx,
    )
}

pub(super) fn dispatch_calcsums(
    table: &TableRef,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    ctx: &mut DispatchCtx,
) -> Eval {
    if nodes.is_empty() {
        return eval_error("CalcSums: requires at least one field");
    }
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let mut view = store.take_view(handle);
    for node in nodes {
        let name = node_text(*node, source);
        let result = store
            .resolve_field(&name)
            .map_err(|error| format!("CalcSums: {error}"))
            .and_then(|field_no| {
                store
                    .record
                    .calc_sum_in(&view, field_no)
                    .map(|total| (field_no, total))
                    .map_err(|error| format!("CalcSums: {error}"))
            });
        match result {
            Ok((field_no, total)) => store.record.field_set_in(&mut view, field_no, total),
            Err(error) => {
                store.put_view(handle, view);
                return eval_error(error);
            }
        }
    }
    store.put_view(handle, view);
    Eval::Normal(Value::Empty)
}

/// `Rec.CalcFields(F1, F2, …)` — evaluate each named FlowField and store the
/// result into the current buffer. Non-FlowField (or unparseable) args are
/// ignored, matching BC's tolerance of explicitly-listed normal fields.
pub(super) fn dispatch_calcfields(
    table: &TableRef,
    handle: u64,
    nodes: &[Node<'_>],
    source: &[u8],
    ctx: &mut DispatchCtx,
) -> Eval {
    if nodes.is_empty() {
        return eval_error("CalcFields: requires at least one FlowField");
    }
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    // Resolve each field name to its number + formula up front (one borrow).
    let targets: Vec<(FieldNo, Option<CalcFormula>)> = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let mut targets = Vec::with_capacity(nodes.len());
        for node in nodes {
            let name = node_text(*node, source);
            let field_no = match store.resolve_field(&name) {
                Ok(field_no) => field_no,
                Err(error) => return eval_error(format!("CalcFields: {error}")),
            };
            targets.push((field_no, store.flowfields.get(&field_no).cloned()));
        }
        targets
    };
    for (field_no, formula) in targets {
        let Some(formula) = formula else {
            return eval_error(format!(
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
/// the referenced table's store via [`crate::mock::record::MockRecord::calc_flow`]. `FIELD(...)`
/// references read the *calculating* record's current buffer; the aggregation
/// and the constrained fields are resolved against the *referenced* table.
pub(super) fn eval_flowfield(
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
                        Err(error) => return eval_error(format!("FlowField: {error}")),
                    };
                    Some(read_buffer_field(store, handle, f))
                }
                _ => None,
            };
            values.push(value);
        }
        values
    };

    // 2. Ensure the referenced table's store exists. A FlowField aggregates the
    //    referenced table itself, so it reads the persistent store even when
    //    the calculating record is temporary.
    let ref_key = match ensure_store(ctx, &TableRef::persistent(&formula.table_name)) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
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
            return eval_error(
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
            Err(error) => return eval_error(format!("FlowField target: {error}")),
        },
        None => None,
    };

    let mut conditions: Vec<(FieldNo, FlowFilter)> = Vec::new();
    for (i, cond) in formula.where_clause.iter().enumerate() {
        let field_no = match store.resolve_field(&cond.field) {
            Ok(field_no) => field_no,
            Err(error) => return eval_error(format!("FlowField condition: {error}")),
        };
        let filt = match &cond.value {
            WhereValue::Const(s) => FlowFilter::Eq(parse_scalar(s)),
            WhereValue::Field(_) => FlowFilter::Eq(field_values[i].clone().unwrap_or(Value::Empty)),
            WhereValue::Filter(expr) => match filter::parse(expr) {
                Ok(parsed) => FlowFilter::Expr(parsed),
                Err(e) => return eval_error(format!("FlowField filter '{expr}': {e}")),
            },
        };
        conditions.push((field_no, filt));
    }

    match store.record.calc_flow(&conditions, target, agg) {
        Ok(value) => Eval::Normal(value),
        Err(error) => eval_error(format!("FlowField calculation failed: {error}")),
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
