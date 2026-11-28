//! Record CRUD and find methods: `Init`, `Insert`, `Modify`, `Delete`, `Get`,
//! `SetRange`, `SetFilter`, `FindSet`/`FindFirst`/`FindLast`/`Find`/`Next`,
//! `Count`, `IsEmpty`, `Reset`, `SetCurrentKey`, `DeleteAll` and `Rename`.
//!
//! [`dispatch_record_method`] routes a `Rec.Method(args)` call. A write
//! (`Insert`/`Modify`/`Delete`) goes through [`write_with_table_code`], which
//! raises the table's `OnBefore`/`OnAfter` events and runs its trigger before
//! and after the write, the way Business Central does.

use std::sync::Arc;

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::eval_stmt::arg_expr_nodes;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::mock::record::{FieldNo, RecordView};

use super::field_access::{record_value_on, x_rec_of};
use super::helpers::{node_text, optional_boolean, render_filter_value, require_no_args};
use super::relations::{RelationIndex, RenameCascade};
use super::store::{ensure_store, records_disabled_error, records_enabled, RecordStore, TableRef};
use super::validate::{
    dispatch_calcfields, dispatch_calcsums, dispatch_modifyall, dispatch_testfield,
    dispatch_validate,
};

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
            | "calcsums"
            | "modifyall"
            | "ascending"
            | "rename"
            | "testfield"
            | "validate"
            | "istemporary"
    )
}

/// Execute a record-API method call (`Rec.Method(args)`).
///
/// `table` names the receiver record variable's backing store and `handle`
/// identifies that variable's own view (filters/cursor/buffer) over it.
/// `args_node` is the call's `argument_list` (so field-reference arguments —
/// e.g. the first arg of `SetRange` — can be read as field names rather than
/// evaluated as variables).
pub(crate) fn dispatch_record_method(
    table: &TableRef,
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
        return dispatch_calcfields(table, handle, &nodes, source, ctx);
    }
    // CalcSums takes field names too, and totals each over the rows the
    // view's filters select, into the buffer.
    if lower == "calcsums" {
        return dispatch_calcsums(table, handle, &nodes, source, ctx);
    }
    if lower == "istemporary" {
        return Eval::Normal(Value::Boolean(table.temp_owner.is_some()));
    }
    // Validate(Field[, Value]): the first argument names a field.
    if lower == "validate" {
        return dispatch_validate(table, handle, &nodes, source, stack, ctx);
    }
    // TestField(Field[, Value]): the first argument names a field.
    if lower == "testfield" {
        return dispatch_testfield(table, handle, &nodes, source, stack, ctx);
    }
    // ModifyAll(Field, Value[, RunTrigger]): the first argument names a field.
    if lower == "modifyall" {
        return dispatch_modifyall(table, handle, &nodes, source, stack, ctx);
    }

    // SetCurrentKey takes only field references. Handle it before the general
    // expression loop so later field arguments are not evaluated as variables.
    if lower == "setcurrentkey" {
        if nodes.is_empty() {
            return eval_error("SetCurrentKey: requires at least one field");
        }
        let key = match ensure_store(ctx, table) {
            Ok(key) => key,
            Err(error) => return eval_error(error),
        };
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let mut field_nos = Vec::with_capacity(nodes.len());
        for node in &nodes {
            let name = node_text(*node, source);
            if name.is_empty() {
                return eval_error("SetCurrentKey: field name is empty");
            }
            match store.resolve_field(&name) {
                Ok(field_no) => field_nos.push(field_no),
                Err(error) => return eval_error(format!("SetCurrentKey: {error}")),
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

    if lower == "rename" {
        return dispatch_rename(table, handle, values, stmt_position, stack, ctx);
    }

    let write = match lower.as_str() {
        "insert" => Some(RecordWrite::Insert),
        "modify" => Some(RecordWrite::Modify),
        "delete" => Some(RecordWrite::Delete),
        _ => None,
    };
    // Insert(true), Modify(true) and Delete(true) run the table's trigger as
    // table code. The operation itself then runs without triggers.
    let run_trigger = write.is_some() && matches!(values.first(), Some(Value::Boolean(true)));
    if run_trigger {
        values[0] = Value::Boolean(false);
    }
    if lower == "deleteall" {
        let run_trigger = match optional_boolean("DeleteAll", &values) {
            Ok(run_trigger) => run_trigger,
            Err(error) => return eval_error(error),
        };
        return write_all(
            table,
            handle,
            (RecordWrite::Delete, run_trigger),
            None,
            stack,
            ctx,
        );
    }

    let operate = |ctx: &mut DispatchCtx| {
        let key = match ensure_store(ctx, table) {
            Ok(k) => k,
            Err(e) => return eval_error(e),
        };

        // Resolve the field-name argument for field-reference methods.
        let field_no = if field_methods {
            let fname = nodes
                .first()
                .map(|n| node_text(*n, source))
                .unwrap_or_default();
            if fname.is_empty() {
                return eval_error(format!("{method}: missing field name argument"));
            }
            let store = ctx.records.get_mut(&key).expect("store just ensured");
            match store.resolve_field(&fname) {
                Ok(field_no) => Some(field_no),
                Err(error) => return eval_error(format!("{method}: {error}")),
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
    };
    match write {
        Some(write) => {
            write_with_table_code(table, handle, (write, run_trigger), stack, ctx, operate)
        }
        // Reset also clears the AL variables the table declares.
        None if lower == "reset" => {
            let result = operate(ctx);
            if !result.is_error() {
                crate::interpreter::dispatch::table_code::reset_record_globals(
                    &table.name,
                    handle,
                    stack,
                    ctx,
                );
            }
            result
        }
        None => operate(ctx),
    }
}

/// A record write that raises table events and can run a table trigger.
#[derive(Debug, Clone, Copy)]
pub(super) enum RecordWrite {
    Insert,
    Modify,
    Delete,
}

impl RecordWrite {
    /// The operation in the event names: `Insert` of `OnBeforeInsertEvent`.
    fn event(self) -> &'static str {
        match self {
            RecordWrite::Insert => "Insert",
            RecordWrite::Modify => "Modify",
            RecordWrite::Delete => "Delete",
        }
    }

    fn trigger(self) -> &'static str {
        match self {
            RecordWrite::Insert => "OnInsert",
            RecordWrite::Modify => "OnModify",
            RecordWrite::Delete => "OnDelete",
        }
    }

    /// `xRec` is the row as the table holds it for a Modify or Delete, and
    /// the buffer itself for an Insert.
    fn x_rec_is_stored(self) -> bool {
        !matches!(self, RecordWrite::Insert)
    }

    /// Whether the table has a subscriber to OnBefore…Event or OnAfter…Event.
    fn observed(self, table: &TableRef, ctx: &mut DispatchCtx) -> bool {
        ["OnBefore", "OnAfter"].iter().any(|when| {
            crate::interpreter::dispatch::events::has_subscribers(
                "table",
                &table.name,
                &format!("{when}{}Event", self.event()),
                "",
                ctx,
            )
        })
    }
}

/// Run `operate`, the `write` of the record on view `handle`, as Business
/// Central does: OnBefore…Event, the table's trigger when `run_trigger` is
/// set and the table declares one, the write, then OnAfter…Event when the
/// write succeeded. Subscribers get the events whatever RunTrigger says.
fn write_with_table_code(
    table: &TableRef,
    handle: u64,
    (write, run_trigger): (RecordWrite, bool),
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
    operate: impl FnOnce(&mut DispatchCtx) -> Eval,
) -> Eval {
    use crate::interpreter::dispatch::table_code::{declares, run_table_code, TableCode};

    let stored = write.x_rec_is_stored();
    // Made only when a subscriber or trigger will see it: copying a
    // temporary record copies its rows. Taken before the write even when
    // only the OnAfter event has subscribers: afterwards the stored row
    // already holds the new values.
    let mut x_rec: Option<Value> = None;
    if write.observed(table, ctx) {
        x_rec = Some(x_rec_of(table, handle, stored, ctx));
    }
    if let Err(error) = raise_table_event(
        table,
        handle,
        (&mut x_rec, stored),
        &format!("OnBefore{}Event", write.event()),
        run_trigger,
        stack,
        ctx,
    ) {
        return error;
    }
    let trigger = TableCode::Trigger(write.trigger());
    if run_trigger && declares(ctx, &table.name, trigger) {
        let x_rec = x_rec
            .get_or_insert_with(|| x_rec_of(table, handle, stored, ctx))
            .clone();
        if let Some(result) = run_table_code(
            record_value_on(table, handle),
            x_rec,
            trigger,
            Vec::new(),
            stack,
            ctx,
        ) {
            if matches!(result, Eval::Error(_)) {
                return result;
            }
        }
    }
    let result = operate(ctx);
    let succeeded = matches!(result, Eval::Normal(ref value) if *value != Value::Boolean(false));
    if succeeded {
        if let Err(error) = raise_table_event(
            table,
            handle,
            (&mut x_rec, stored),
            &format!("OnAfter{}Event", write.event()),
            run_trigger,
            stack,
            ctx,
        ) {
            return error;
        }
    }
    result
}

/// `DeleteAll([RunTrigger])`, or `ModifyAll(Field, Value[, RunTrigger])`
/// with `assign` holding the field and its value.
///
/// Business Central writes the rows one at a time when the table has
/// subscribers to the Delete or Modify events, or trigger code
/// ("AL database methods and performance on SQL Server", ModifyAll and
/// DeleteAll), and each row then goes through the events and, with
/// RunTrigger, the trigger, as Delete and Modify do. The rows are written
/// here one at a time on the caller's view when a subscriber or a trigger
/// that RunTrigger runs would see them, and in one pass otherwise. The
/// caller's buffer and filters are as they were afterwards.
pub(super) fn write_all(
    table: &TableRef,
    handle: u64,
    (write, run_trigger): (RecordWrite, bool),
    assign: Option<(FieldNo, Value)>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    use crate::interpreter::dispatch::table_code::{declares, without_record_globals, TableCode};

    let method = match write {
        RecordWrite::Modify => "ModifyAll",
        _ => "DeleteAll",
    };
    let row_by_row = write.observed(table, ctx)
        || (run_trigger && declares(ctx, &table.name, TableCode::Trigger(write.trigger())));
    let key = match ensure_store(ctx, table) {
        Ok(key) => key,
        Err(error) => return eval_error(error),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let mut view = store.take_view(handle);
    if !row_by_row {
        let result = match assign {
            Some((field, value)) => store
                .record
                .modify_all_in(&view, field, value, false)
                .map(drop),
            None => store.record.delete_all_in(&mut view, false).map(drop),
        };
        store.put_view(handle, view);
        return match result {
            Ok(()) => Eval::Normal(Value::Empty),
            Err(error) => eval_error(format!("{method}: {error}")),
        };
    }
    if let Some((field, _)) = &assign {
        if store.record.primary_key_fields().contains(field) {
            store.put_view(handle, view);
            return eval_error(format!(
                "{method}: {}",
                crate::mock::record::RecordError::PrimaryKeyModifyAll(*field)
            ));
        }
    }
    let rows = store.record.matching_keys_in(&view);
    let saved = view.clone();
    store.put_view(handle, view);
    let restore = |ctx: &mut DispatchCtx| {
        let mut saved = saved.clone();
        if matches!(write, RecordWrite::Delete) {
            saved.clear_cursor();
        }
        if let Some(store) = ctx.records.get_mut(&key) {
            store.put_view(handle, saved);
        }
    };
    let each_row = |ctx: &mut DispatchCtx| {
        for row in rows {
            let store = ctx.records.get_mut(&key).expect("store just ensured");
            let mut view = store.take_view(handle);
            // Code that ran for an earlier row can have removed this one.
            let loaded = store.record.get_in(&mut view, row).is_ok();
            if loaded {
                if let Some((field, value)) = &assign {
                    store.record.field_set_in(&mut view, *field, value.clone());
                }
            }
            store.put_view(handle, view);
            if !loaded {
                continue;
            }
            let result =
                write_with_table_code(table, handle, (write, run_trigger), stack, ctx, |ctx| {
                    let store = ctx.records.get_mut(&key).expect("store just ensured");
                    let mut view = store.take_view(handle);
                    let result = match write {
                        RecordWrite::Modify => store.record.modify_in(&mut view, false),
                        _ => store.record.delete_in(&mut view, false),
                    };
                    store.put_view(handle, view);
                    match result {
                        Ok(()) => Eval::Normal(Value::Boolean(true)),
                        Err(error) => eval_error(format!("{method}: {error}")),
                    }
                });
            if result.is_error() {
                return result;
            }
        }
        Eval::Normal(Value::Empty)
    };
    // The rows' table code runs on a copy of the record whose globals start
    // at their defaults: "When you use DeleteAll(true), a copy of the AL
    // variable with its initial values is created" (Insert, Modify,
    // ModifyAll, Delete, DeleteAll, and Truncate methods on Learn).
    let result = without_record_globals(handle, ctx, each_row);
    restore(ctx);
    result
}

/// `Rec.Rename(key values…)`, the new primary key with all its parts.
///
/// The new key goes into the buffer first, so OnBeforeRenameEvent, OnRename
/// and OnAfterRenameEvent get it as `Rec` and the row as stored as `xRec`.
/// The row then moves to the new key with the buffer written to it, so what
/// OnRename sets on `Rec` is saved, and every field that relates to the old
/// key gets the new one (see [`RelationIndex`]). A failure leaves the buffer
/// as it was.
fn dispatch_rename(
    table: &TableRef,
    handle: u64,
    values: Vec<Value>,
    stmt_position: bool,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    use crate::interpreter::dispatch::{events, table_code};

    let key = match ensure_store(ctx, table) {
        Ok(key) => key,
        Err(error) => return eval_error(error),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    if values.len() != store.record.primary_key_len() {
        return eval_error(format!(
            "Rename: requires all {} primary-key values (got {})",
            store.record.primary_key_len(),
            values.len()
        ));
    }
    let pk_fields: Vec<FieldNo> = store.record.primary_key_fields().to_vec();
    let mut new_key = Vec::with_capacity(values.len());
    for (field, value) in pk_fields.iter().copied().zip(values) {
        match store.coerce_to_field(field, value) {
            Ok(coerced) => new_key.push((field, coerced)),
            Err(error) => return eval_error(format!("Rename: {error}")),
        }
    }
    let view = store.take_view(handle);
    let old_key: Vec<(FieldNo, Value)> = pk_fields
        .iter()
        .map(|&field| {
            let value = store
                .record
                .field_get_in(&view, field)
                .or_else(|| store.field_defaults.get(&field))
                .cloned()
                .unwrap_or(Value::Empty);
            (field, value)
        })
        .collect();
    store.put_view(handle, view);
    // A temporary record has no related rows in the database.
    let cascades = match table.temp_owner {
        Some(_) => Vec::new(),
        None => match relation_index(ctx).rename_cascades(&*Arc::clone(&ctx.source), &table.name) {
            Ok(cascades) => cascades,
            Err(reason) => return eval_error(format!("Rename: {reason}")),
        },
    };
    let failed = |error: crate::mock::record::RecordError| {
        if stmt_position {
            eval_error(format!("Rename: {error}"))
        } else {
            Eval::Normal(Value::Boolean(false))
        }
    };

    let trigger = table_code::TableCode::Trigger("OnRename");
    let has_trigger = table_code::declares(ctx, &table.name, trigger);
    let observed = has_trigger
        || ["OnBeforeRenameEvent", "OnAfterRenameEvent"]
            .iter()
            .any(|event| events::has_subscribers("table", &table.name, event, "", ctx));
    // The row as stored, read while the buffer still holds the old key.
    let mut x_rec = observed.then(|| x_rec_of(table, handle, true, ctx));

    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let mut view = store.take_view(handle);
    let started = store.record.start_rename_in(&mut view, new_key.clone());
    store.put_view(handle, view);
    let pending = match started {
        Ok(pending) => pending,
        Err(error) => return failed(error),
    };

    let mut code_result = raise_table_event(
        table,
        handle,
        (&mut x_rec, true),
        "OnBeforeRenameEvent",
        true,
        stack,
        ctx,
    );
    // `observed` covers the trigger, so `x_rec` holds the stored row here.
    if let (Ok(()), true, Some(x_rec)) = (&code_result, has_trigger, x_rec.clone()) {
        if let Some(result @ Eval::Error(_)) = table_code::run_table_code(
            record_value_on(table, handle),
            x_rec,
            trigger,
            Vec::new(),
            stack,
            ctx,
        ) {
            code_result = Err(result);
        }
    }

    let key = match ensure_store(ctx, table) {
        Ok(key) => key,
        Err(error) => return eval_error(error),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let mut view = store.take_view(handle);
    let finished = match code_result {
        Ok(()) => store.record.finish_rename_in(&mut view, pending),
        Err(error) => {
            store.record.cancel_rename_in(&mut view, pending);
            store.put_view(handle, view);
            return error;
        }
    };
    store.put_view(handle, view);
    if let Err(error) = finished {
        return failed(error);
    }
    if let Err(error) = cascade_rename(&key, &cascades, &old_key, &new_key, ctx) {
        return eval_error(format!("Rename: {error}"));
    }
    if let Err(error) = raise_table_event(
        table,
        handle,
        (&mut x_rec, true),
        "OnAfterRenameEvent",
        true,
        stack,
        ctx,
    ) {
        return error;
    }
    Eval::Normal(Value::Boolean(true))
}

/// The workspace's table relations, built on first use.
fn relation_index(ctx: &mut DispatchCtx) -> Arc<RelationIndex> {
    match &ctx.relations {
        Some(index) => Arc::clone(index),
        None => {
            let index = Arc::new(RelationIndex::build(&*ctx.source));
            ctx.relations = Some(Arc::clone(&index));
            index
        }
    }
}

/// Give every field in `cascades` the new value of the renamed key where it
/// holds the old one. `renamed` is the store key of the renamed table. A
/// related table no code has touched holds no rows and is skipped.
fn cascade_rename(
    renamed: &str,
    cascades: &[RenameCascade],
    old_key: &[(FieldNo, Value)],
    new_key: &[(FieldNo, Value)],
    ctx: &mut DispatchCtx,
) -> Result<(), String> {
    for cascade in cascades {
        let key_field = ctx
            .records
            .get(renamed)
            .expect("the renamed table has a store")
            .resolve_field(&cascade.key_field)?;
        let value_of = |key: &[(FieldNo, Value)]| {
            key.iter()
                .find(|(field, _)| *field == key_field)
                .map(|(_, value)| value.clone())
        };
        let (Some(old), Some(new)) = (value_of(old_key), value_of(new_key)) else {
            continue;
        };
        if old == new {
            continue;
        }
        let related = TableRef::persistent(cascade.table.clone()).key();
        let Some(store) = ctx.records.get_mut(&related) else {
            continue;
        };
        let field = store.resolve_field(&cascade.field)?;
        let old = store.coerce_to_field(field, old)?;
        let new = store.coerce_to_field(field, new)?;
        store
            .record
            .replace_field_value(field, &old, new)
            .map_err(|error| {
                format!(
                    "updating field {} of table {}: {error}",
                    cascade.field, cascade.table
                )
            })?;
    }
    Ok(())
}

/// Raise table event `event` (`OnAfterInsertEvent`, ...) on the record on
/// view `handle`: subscribers get it as `Rec` (sharing the view, so their
/// changes are the caller's), with `xRec` and `RunTrigger`.
fn raise_table_event(
    table: &TableRef,
    handle: u64,
    (x_rec, stored): (&mut Option<Value>, bool),
    event: &str,
    run_trigger: bool,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<(), Eval> {
    if !crate::interpreter::dispatch::events::has_subscribers("table", &table.name, event, "", ctx)
    {
        return Ok(());
    }
    let x_rec = x_rec
        .get_or_insert_with(|| x_rec_of(table, handle, stored, ctx))
        .clone();
    let mut values = vec![
        record_value_on(table, handle),
        x_rec,
        Value::Boolean(run_trigger),
    ];
    crate::interpreter::dispatch::events::raise(
        "table",
        &table.name,
        event,
        "",
        &["Rec", "xRec", "RunTrigger"],
        &mut values,
        None,
        stack,
        ctx,
    )
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
            eval_error(format!("{method}: {error}"))
        }
        Err(error) if stmt_position => eval_error(format!("{method}: {error}")),
        Err(_) => Eval::Normal(Value::Boolean(false)),
    };
    // A find-class miss errors in statement position and yields false in
    // expression position.
    let find_result = |method: &str, table: &str, found: Result<bool, RecordError>| match found {
        Ok(true) => Eval::Normal(Value::Boolean(true)),
        Ok(false) if stmt_position => eval_error(format!(
            "{method}: no '{table}' record matches the current filters"
        )),
        Ok(false) => Eval::Normal(Value::Boolean(false)),
        Err(error) => eval_error(format!("{method}: {error}")),
    };

    match lower {
        "init" => {
            if let Err(error) = require_no_args("Init", &values) {
                return eval_error(error);
            }
            store.record.init_in(view);
            Eval::Normal(Value::Empty)
        }
        "reset" => {
            if let Err(error) = require_no_args("Reset", &values) {
                return eval_error(error);
            }
            store.record.reset_in(view);
            Eval::Normal(Value::Empty)
        }
        "ascending" => match values.as_slice() {
            [] => Eval::Normal(Value::Boolean(store.record.is_ascending_in(view))),
            [Value::Boolean(ascending)] => {
                store.record.set_ascending_in(view, *ascending);
                Eval::Normal(Value::Boolean(*ascending))
            }
            _ => eval_error("Ascending expects an optional Boolean"),
        },
        "insert" => match optional_boolean("Insert", &values) {
            Ok(run_trigger) => mutation_result("Insert", store.record.insert_in(view, run_trigger)),
            Err(error) => eval_error(error),
        },
        "modify" => match optional_boolean("Modify", &values) {
            Ok(run_trigger) => mutation_result("Modify", store.record.modify_in(view, run_trigger)),
            Err(error) => eval_error(error),
        },
        "delete" => match optional_boolean("Delete", &values) {
            Ok(run_trigger) => mutation_result("Delete", store.record.delete_in(view, run_trigger)),
            Err(error) => eval_error(error),
        },
        "get" => {
            let key_len = store.record.primary_key_len();
            if values.len() > key_len {
                return eval_error(format!(
                    "Get: {} values given for a primary key of {} fields",
                    values.len(),
                    key_len
                ));
            }
            // Coerce each given key part to the declared PK field type so a Text
            // literal matches a Code key and an Integer a Decimal one.
            let pk_fields: Vec<FieldNo> = store.record.primary_key_fields().to_vec();
            let mut key_values = Vec::with_capacity(key_len);
            for (field, value) in pk_fields.into_iter().zip(values) {
                match store.coerce_to_field(field, value) {
                    Ok(coerced) => key_values.push(coerced),
                    Err(error) => return eval_error(format!("Get: {error}")),
                }
            }
            // Key fields left out take their declared default, as BC fills them.
            let key_values = match store.record.key_with_defaults(key_values) {
                Ok(key) => key,
                Err(field) => {
                    return eval_error(format!(
                        "Get: primary-key field {field} of table '{}' has no declared \
                         default, so Get needs every key value",
                        store.record.table_name
                    ));
                }
            };
            match store.record.get_in(view, key_values) {
                Ok(()) => Eval::Normal(Value::Boolean(true)),
                Err(_) if stmt_position => eval_error(format!(
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
                count => eval_error(format!(
                    "SetRange: expected at most two values after the field, got {count}"
                )),
            }
        }
        "setfilter" => {
            let f = field_no.unwrap();
            let raw = match values.first() {
                Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
                Some(value) => {
                    return eval_error(format!(
                        "SetFilter: filter expression must be Text or Code, got {}",
                        value.type_name()
                    ))
                }
                None => return eval_error("SetFilter: missing filter expression"),
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
                Err(error) => return eval_error(format!("SetFilter: {error}")),
            };
            match store.record.set_filter_in(view, f, &expr) {
                Ok(()) => Eval::Normal(Value::Empty),
                Err(e) => eval_error(format!("SetFilter: {e}")),
            }
        }
        "findset" => {
            if values.len() > 2
                || values
                    .iter()
                    .any(|value| !matches!(value, Value::Boolean(_)))
            {
                return eval_error("FindSet: expects up to two optional Boolean arguments");
            }
            let table = store.record.table_name.clone();
            find_result("FindSet", &table, store.record.find_first_in(view))
        }
        "findfirst" => {
            if let Err(error) = require_no_args("FindFirst", &values) {
                return eval_error(error);
            }
            let table = store.record.table_name.clone();
            find_result("FindFirst", &table, store.record.find_first_in(view))
        }
        "findlast" => {
            if let Err(error) = require_no_args("FindLast", &values) {
                return eval_error(error);
            }
            let table = store.record.table_name.clone();
            find_result("FindLast", &table, store.record.find_last_in(view))
        }
        "find" => {
            let which = match values.as_slice() {
                [] => "=",
                [Value::Text(which)] | [Value::Code(which)] => which.as_str(),
                _ => return eval_error("Find: expects at most one Text or Code argument"),
            };
            let table = store.record.table_name.clone();
            find_result("Find", &table, store.record.find_in(view, which))
        }
        "next" => {
            let steps = match values.as_slice() {
                [] => 1,
                [Value::Integer(steps)] => match i32::try_from(*steps) {
                    Ok(steps) => steps,
                    Err(_) => {
                        return eval_error(format!(
                            "Next: step count {steps} is outside Integer range"
                        ))
                    }
                },
                _ => return eval_error("Next: expects one optional Integer step count"),
            };
            match store.record.next_in(view, steps) {
                Ok(moved) => Eval::Normal(Value::Integer(moved as i64)),
                Err(e) => eval_error(format!("Next: {e}")),
            }
        }
        "count" | "countapprox" => {
            if let Err(error) = require_no_args(method, &values) {
                return eval_error(error);
            }
            Eval::Normal(Value::Integer(store.record.count_in(view) as i64))
        }
        "isempty" => {
            if let Err(error) = require_no_args("IsEmpty", &values) {
                return eval_error(error);
            }
            Eval::Normal(Value::Boolean(store.record.is_empty_in(view)))
        }
        other => eval_error(format!("unsupported record method: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::interpreter::dispatch::{dispatch_call, DispatchCtx};
    use crate::interpreter::scope::Eval;
    use crate::interpreter::value::Value;
    use crate::test_support::MockSource as Workspace;

    /// Key ("Item No.", "Line No."). `Line No.` is an Integer, so a `Get`
    /// that gives only "Item No." looks for `Line No.` = 0.
    const PRICE_LINE_TABLE: &str = r#"table 50110 "Price Line"
{
    fields
    {
        field(1; "Item No."; Code[20]) { }
        field(2; "Line No."; Integer) { }
    }
    keys
    {
        key(PK; "Item No.", "Line No.") { Clustered = true; }
    }
}
"#;

    const PRICE_LINE_TESTS: &str = r#"codeunit 50111 "Price Line Tests"
{
    procedure InsertFiveThenGetPartial(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        PriceLine.Init();
        PriceLine."Item No." := 'A';
        PriceLine."Line No." := 5;
        PriceLine.Insert();
        exit(PriceLine.Get('A'));
    end;

    procedure InsertFiveThenGetExact(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        PriceLine.Init();
        PriceLine."Item No." := 'A';
        PriceLine."Line No." := 5;
        PriceLine.Insert();
        exit(PriceLine.Get('A', 5));
    end;

    procedure InsertZeroThenGetPartial(): Integer
    var
        PriceLine: Record "Price Line";
        Found: Boolean;
    begin
        PriceLine.Init();
        PriceLine."Item No." := 'A';
        PriceLine."Line No." := 0;
        PriceLine.Insert();
        PriceLine.Init();
        PriceLine."Item No." := 'A';
        PriceLine."Line No." := 9;
        Found := PriceLine.Get('A');
        if not Found then
            exit(-1);
        exit(PriceLine."Line No.");
    end;

    procedure GetTooManyValues(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        exit(PriceLine.Get('A', 5, 1));
    end;
}
"#;

    const INIT_TABLE: &str = r#"table 50100 T
{
    fields
    {
        field(1; No; Code[20]) { }
        field(2; Quantity; Integer) { InitValue = 7; }
    }
    keys
    {
        key(PK; No) { }
    }
}
"#;

    const INIT_TESTS: &str = r#"codeunit 50102 "Init Tests"
{
    procedure InitRestoresInitValue(): Integer
    var
        R: Record T;
    begin
        R.Quantity := 3;
        R.Init();
        exit(R.Quantity);
    end;
}
"#;

    const SETUP_TESTS: &str = r#"codeunit 50112 "Setup Tests"
{
    procedure GetBlankKeyAfterInsert(): Integer
    var
        R: Record T;
        R2: Record T;
    begin
        R.Init();
        R.Quantity := 3;
        R.Insert();
        if not R2.Get() then
            exit(-1);
        exit(R2.Quantity);
    end;

    procedure GetOnEmptyTable(): Boolean
    var
        R: Record T;
    begin
        exit(R.Get());
    end;

    procedure GetStatementOnEmptyTable()
    var
        R: Record T;
    begin
        R.Get();
    end;
}
"#;

    const FIND_TESTS: &str = r#"codeunit 50113 "Find Tests"
{
    local procedure Setup(var PriceLine: Record "Price Line")
    begin
        PriceLine.Init();
        PriceLine."Item No." := 'A';
        PriceLine."Line No." := 1;
        PriceLine.Insert();
        PriceLine."Line No." := 2;
        PriceLine.Insert();
    end;

    procedure FindWithoutArgument(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine."Line No." := 2;
        exit(PriceLine.Find());
    end;

    procedure FindEqualMisses(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine."Line No." := 3;
        exit(PriceLine.Find('='));
    end;

    procedure FindEqualOutsideFilter(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine.SetRange("Line No.", 1);
        PriceLine."Line No." := 2;
        exit(PriceLine.Find('='));
    end;

    procedure FindEqualThenNext(): Integer
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine."Line No." := 1;
        PriceLine.Find('=');
        if PriceLine.Next() <> 1 then
            exit(-1);
        exit(PriceLine."Line No.");
    end;

    procedure FindNextLarger(): Integer
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine."Line No." := 5;
        if not PriceLine.Find('=><') then
            exit(-1);
        exit(PriceLine."Line No.");
    end;

    procedure FindMissInStatement()
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        PriceLine."Line No." := 3;
        PriceLine.Find();
    end;

    procedure FindEmptyString(): Boolean
    var
        PriceLine: Record "Price Line";
    begin
        Setup(PriceLine);
        exit(PriceLine.Find(''));
    end;
}
"#;

    const FORM_TABLE: &str = r#"table 50103 Forms
{
    fields
    {
        field(1; No; Code[20]) { }
        field(2; Amount; Decimal) { InitValue = 1.5; }
        field(3; Active; Boolean) { InitValue = true; }
        field(4; Note; Text[30]) { InitValue = 'abc'; }
    }
    keys
    {
        key(PK; No) { }
    }
}
"#;

    const FORM_TESTS: &str = r#"codeunit 50104 "Form Tests"
{
    procedure InitValueForms(): Boolean
    var
        R: Record Forms;
    begin
        R.Init();
        exit((R.Amount = 1.5) and R.Active and (R.Note = 'abc'));
    end;
}
"#;

    fn run_sources(sources: &[(&str, &str)], object: &str, proc: &str) -> Eval {
        let ws = Arc::new(Workspace::new());
        for (path, src) in sources {
            ws.file_index
                .add_file(std::path::PathBuf::from(path), src.to_string());
        }
        let mut ctx = DispatchCtx::new_with_records(ws, Default::default());
        dispatch_call(Some(object), proc, vec![], &mut ctx)
    }

    fn run(object: &str, proc: &str) -> Eval {
        run_sources(
            &[
                ("/ws/PriceLine.al", PRICE_LINE_TABLE),
                ("/ws/PriceLineTests.al", PRICE_LINE_TESTS),
            ],
            object,
            proc,
        )
    }

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) | Eval::Exit(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    #[test]
    fn get_with_omitted_trailing_key_uses_default() {
        // Only ('A', 5) is stored, so Get('A') looks for ('A', 0) and misses.
        assert_eq!(
            ok(run("Price Line Tests", "InsertFiveThenGetPartial")),
            Value::Boolean(false)
        );
        // The exact key finds the row, so the miss above comes from the key.
        assert_eq!(
            ok(run("Price Line Tests", "InsertFiveThenGetExact")),
            Value::Boolean(true)
        );
        // ('A', 0) is stored, so Get('A') finds it and loads its Line No.
        assert_eq!(
            ok(run("Price Line Tests", "InsertZeroThenGetPartial")),
            Value::Integer(0)
        );
    }

    #[test]
    fn get_with_more_values_than_key_fields_is_an_error() {
        match run("Price Line Tests", "GetTooManyValues") {
            Eval::Error(error) => assert!(
                error
                    .message
                    .contains("3 values given for a primary key of 2 fields"),
                "unexpected message: {}",
                error.message
            ),
            other => panic!("expected an interpreter error, got {other:?}"),
        }
    }

    #[test]
    fn get_with_no_values_finds_the_blank_key_row() {
        let sources = [("/ws/T.al", INIT_TABLE), ("/ws/SetupTests.al", SETUP_TESTS)];
        assert_eq!(
            ok(run_sources(
                &sources,
                "Setup Tests",
                "GetBlankKeyAfterInsert"
            )),
            Value::Integer(3)
        );
        assert_eq!(
            ok(run_sources(&sources, "Setup Tests", "GetOnEmptyTable")),
            Value::Boolean(false)
        );
        match run_sources(&sources, "Setup Tests", "GetStatementOnEmptyTable") {
            Eval::Error(error) => assert!(
                error.message.contains("does not exist in table"),
                "unexpected message: {}",
                error.message
            ),
            other => panic!("expected an interpreter error, got {other:?}"),
        }
    }

    #[test]
    fn find_without_argument_and_with_a_string_follow_business_central() {
        let sources = [
            ("/ws/PriceLine.al", PRICE_LINE_TABLE),
            ("/ws/FindTests.al", FIND_TESTS),
        ];
        let run = |proc: &str| run_sources(&sources, "Find Tests", proc);
        assert_eq!(ok(run("FindWithoutArgument")), Value::Boolean(true));
        assert_eq!(ok(run("FindEqualMisses")), Value::Boolean(false));
        assert_eq!(ok(run("FindEqualOutsideFilter")), Value::Boolean(false));
        assert_eq!(ok(run("FindEqualThenNext")), Value::Integer(2));
        assert_eq!(ok(run("FindNextLarger")), Value::Integer(2));
        for (proc, text) in [
            ("FindMissInStatement", "no 'Price Line' record matches"),
            (
                "FindEmptyString",
                "invalid character in the Find expression ''",
            ),
        ] {
            match run(proc) {
                Eval::Error(error) => assert!(
                    error.message.contains(text),
                    "{proc}: unexpected message: {}",
                    error.message
                ),
                other => panic!("{proc}: expected an interpreter error, got {other:?}"),
            }
        }
    }

    #[test]
    fn init_applies_init_value() {
        let sources = [("/ws/T.al", INIT_TABLE), ("/ws/InitTests.al", INIT_TESTS)];
        // Init writes the InitValue over the caller's Quantity.
        assert_eq!(
            ok(run_sources(&sources, "Init Tests", "InitRestoresInitValue")),
            Value::Integer(7)
        );
    }

    #[test]
    fn init_value_decimal_boolean_and_text_literals_apply() {
        let sources = [
            ("/ws/Forms.al", FORM_TABLE),
            ("/ws/FormTests.al", FORM_TESTS),
        ];
        assert_eq!(
            ok(run_sources(&sources, "Form Tests", "InitValueForms")),
            Value::Boolean(true)
        );
    }

    const FLOW_HDR_TABLE: &str = r#"table 50100 Hdr
{
    fields
    {
        field(1; No; Code[20]) { }
        field(2; OpenLines; Integer)
        {
            FieldClass = FlowField;
            CalcFormula = Count(Line WHERE (DocNo = FIELD(No)));
        }
    }
    keys
    {
        key(PK; No) { }
    }
}
"#;

    const FLOW_LINE_TABLE: &str = r#"table 50101 Line
{
    fields
    {
        field(1; DocNo; Code[20]) { }
        field(2; LineNo; Integer) { }
    }
    keys
    {
        key(PK; DocNo, LineNo) { }
    }
}
"#;

    const FLOW_TESTS: &str = r#"codeunit 50105 "Flow Tests"
{
    procedure OpenLinesBeforeCalc(): Integer
    var
        H: Record Hdr;
        L: Record Line;
    begin
        H.No := 'D1';
        H.Insert();
        L.DocNo := 'D1';
        L.LineNo := 10;
        L.Insert();
        H.Get('D1');
        exit(H.OpenLines);
    end;

    procedure OpenLinesAfterCalc(): Integer
    var
        H: Record Hdr;
        L: Record Line;
    begin
        H.No := 'D1';
        H.Insert();
        L.DocNo := 'D1';
        L.LineNo := 10;
        L.Insert();
        H.Get('D1');
        H.CalcFields(OpenLines);
        exit(H.OpenLines);
    end;

    procedure OpenLinesAfterCalcThenGet(): Integer
    var
        H: Record Hdr;
        L: Record Line;
    begin
        H.No := 'D1';
        H.Insert();
        L.DocNo := 'D1';
        L.LineNo := 10;
        L.Insert();
        H.Get('D1');
        H.CalcFields(OpenLines);
        H.Get('D1');
        exit(H.OpenLines);
    end;

    procedure OpenLinesStoredThenGet(): Integer
    var
        H: Record Hdr;
        L: Record Line;
    begin
        H.No := 'D1';
        H.Insert();
        L.DocNo := 'D1';
        L.LineNo := 10;
        L.Insert();
        H.Get('D1');
        H.CalcFields(OpenLines);
        H.Modify();
        H.Get('D1');
        exit(H.OpenLines);
    end;

    procedure OpenLinesStoredThenFindFirst(): Integer
    var
        H: Record Hdr;
        L: Record Line;
    begin
        H.No := 'D1';
        H.Insert();
        L.DocNo := 'D1';
        L.LineNo := 10;
        L.Insert();
        H.Get('D1');
        H.CalcFields(OpenLines);
        H.Modify();
        H.FindFirst();
        exit(H.OpenLines);
    end;
}
"#;

    #[test]
    fn flowfield_is_zero_until_calcfields() {
        let sources = [
            ("/ws/Hdr.al", FLOW_HDR_TABLE),
            ("/ws/Line.al", FLOW_LINE_TABLE),
            ("/ws/FlowTests.al", FLOW_TESTS),
        ];
        // The line exists, but the FlowField reads zero until CalcFields runs.
        assert_eq!(
            ok(run_sources(&sources, "Flow Tests", "OpenLinesBeforeCalc")),
            Value::Integer(0)
        );
        assert_eq!(
            ok(run_sources(&sources, "Flow Tests", "OpenLinesAfterCalc")),
            Value::Integer(1)
        );
        // Get reloads the row without FlowField values, so the calculation is gone.
        assert_eq!(
            ok(run_sources(
                &sources,
                "Flow Tests",
                "OpenLinesAfterCalcThenGet"
            )),
            Value::Integer(0)
        );
    }

    // Modify writes the calculated value into the row. Get must not keep it.
    #[test]
    fn flowfield_is_zero_after_get_reloads_a_row_that_stores_a_value() {
        let sources = [
            ("/ws/Hdr.al", FLOW_HDR_TABLE),
            ("/ws/Line.al", FLOW_LINE_TABLE),
            ("/ws/FlowTests.al", FLOW_TESTS),
        ];
        assert_eq!(
            ok(run_sources(
                &sources,
                "Flow Tests",
                "OpenLinesStoredThenGet"
            )),
            Value::Integer(0)
        );
    }

    // FindFirst loads a row the same way, so it must drop the calculation too.
    #[test]
    fn flowfield_is_zero_after_find_first_loads_a_row_that_stores_a_value() {
        let sources = [
            ("/ws/Hdr.al", FLOW_HDR_TABLE),
            ("/ws/Line.al", FLOW_LINE_TABLE),
            ("/ws/FlowTests.al", FLOW_TESTS),
        ];
        assert_eq!(
            ok(run_sources(
                &sources,
                "Flow Tests",
                "OpenLinesStoredThenFindFirst"
            )),
            Value::Integer(0)
        );
    }
}
