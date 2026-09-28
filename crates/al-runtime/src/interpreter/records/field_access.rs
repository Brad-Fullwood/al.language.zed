//! Reading and writing one field of a bound record, and the implicit `Rec`
//! that table code binds a bare field name to.

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{RecordValue, Value};
use crate::mock::record::FieldNo;

use super::store::{
    ensure_store, fork_record_for_by_value, record_binding, records_disabled_error,
    records_enabled, RecordStore, TableRef,
};
use super::validate::eval_flowfield;

/// Read one buffer field of the view identified by `handle`, falling back to
/// the field's typed default (BC zero-initialisation) when never assigned.
pub(super) fn read_buffer_field(store: &RecordStore, handle: u64, field: FieldNo) -> Value {
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
    table: &TableRef,
    handle: u64,
    field_name: &str,
    ctx: &mut DispatchCtx,
) -> Eval {
    if !records_enabled(ctx) {
        return records_disabled_error();
    }
    let key = match ensure_store(ctx, table) {
        Ok(k) => k,
        Err(e) => return eval_error(e),
    };
    let (f, formula) = {
        let store = ctx.records.get_mut(&key).expect("store just ensured");
        let f = match store.resolve_field(field_name) {
            Ok(field_no) => field_no,
            Err(error) => return eval_error(error),
        };
        (f, store.flowfields.get(&f).cloned())
    };
    if let Some(formula) = formula {
        return eval_flowfield(ctx, &key, handle, &formula);
    }
    let store = ctx.records.get(&key).expect("store just ensured");
    Eval::Normal(read_buffer_field(store, handle, f))
}

/// Whether workspace table `table` declares a field named `field`.
pub(crate) fn declares_field(table: &TableRef, field: &str, ctx: &mut DispatchCtx) -> bool {
    ensure_store(ctx, table).is_ok_and(|key| {
        ctx.records
            .get(&key)
            .is_some_and(|store| store.resolve_field(field).is_ok())
    })
}

/// The record on `table`'s view `handle`, as table code's implicit `Rec`.
pub(super) fn record_value_on(table: &TableRef, handle: u64) -> Value {
    Value::Record(RecordValue {
        table_name: table.name.clone(),
        table_id: 0,
        handle: Some(handle),
        temporary: table.temp_owner.is_some(),
    })
}

/// `xRec` for table code on view `handle`: a copy of the record on its own
/// view, holding the buffer as it is now or, with `stored`, the row as the
/// table holds it.
pub(super) fn x_rec_of(
    table: &TableRef,
    handle: u64,
    stored: bool,
    ctx: &mut DispatchCtx,
) -> Value {
    let Value::Record(mut copy) = record_value_on(table, handle) else {
        unreachable!("record_value_on builds a record");
    };
    fork_record_for_by_value(ctx, &mut copy);
    if stored {
        if let Some(copy_handle) = copy.handle {
            let copy_table = TableRef {
                name: copy.table_name.clone(),
                temp_owner: copy.temporary.then_some(copy_handle),
            };
            if let Ok(key) = ensure_store(ctx, &copy_table) {
                let store = ctx.records.get_mut(&key).expect("store just ensured");
                let mut view = store.take_view(copy_handle);
                store.record.reload_stored_in(&mut view);
                store.put_view(copy_handle, view);
            }
        }
    }
    Value::Record(copy)
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
    set_record_field(&recv, &field_name, rhs_val, stack, ctx)
}

/// Write `value` to field `field_name` of the record variable `recv`,
/// coerced to the field's type. `None` when `recv` is not a record variable.
fn set_record_field(
    recv: &str,
    field_name: &str,
    rhs_val: &Value,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let (table, handle) = record_binding(recv, stack, ctx)?;
    if !records_enabled(ctx) {
        return Some(records_disabled_error());
    }
    let key = match ensure_store(ctx, &table) {
        Ok(k) => k,
        Err(e) => return Some(eval_error(e)),
    };
    let store = ctx.records.get_mut(&key).expect("store just ensured");
    let f = match store.resolve_field(field_name) {
        Ok(field_no) => field_no,
        Err(error) => return Some(eval_error(error)),
    };
    let coerced = match store.coerce_to_field(f, rhs_val.clone()) {
        Ok(value) => value,
        Err(error) => return Some(eval_error(error)),
    };
    let mut view = store.take_view(handle);
    store.record.field_set_in(&mut view, f, coerced);
    store.put_view(handle, view);
    Some(Eval::Normal(Value::Empty))
}

/// The name table code binds its record to.
pub(crate) const IMPLICIT_RECORD: &str = "Rec";

/// In table code, the field of the implicit record that a bare `name`
/// denotes. `None` outside table code or when `name` is not a field.
fn implicit_field(name: &str, stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Option<String> {
    if !stack.top().is_some_and(|frame| frame.implicit_record) {
        return None;
    }
    let field = name.unquote_identifier().into_owned();
    let (table, _) = record_binding(IMPLICIT_RECORD, stack, ctx)?;
    let key = ensure_store(ctx, &table).ok()?;
    ctx.records.get(&key)?.resolve_field(&field).ok()?;
    Some(field)
}

/// Read a bare field name in table code (`"Search Name"` for
/// `Rec."Search Name"`).
pub(crate) fn implicit_field_get(
    name: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let field = implicit_field(name, stack, ctx)?;
    let (table, handle) = record_binding(IMPLICIT_RECORD, stack, ctx)?;
    Some(field_get(&table, handle, &field, ctx))
}

/// Assign a bare field name in table code.
pub(crate) fn implicit_field_set(
    name: &str,
    value: &Value,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let field = implicit_field(name, stack, ctx)?;
    set_record_field(IMPLICIT_RECORD, &field, value, stack, ctx)
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
        .map(|t| t.unquote_identifier().into_owned())?;
    let member_node = suffix.child_by_field_name("member").or_else(|| {
        let mut c = suffix.walk();
        let found = suffix
            .named_children(&mut c)
            .find(|n| matches!(n.kind(), "identifier" | "quoted_identifier" | "name"));
        found
    });
    let field = member_node
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.unquote_identifier().into_owned())?;
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
