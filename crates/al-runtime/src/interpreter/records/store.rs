//! The in-memory record store and which one a record variable binds to.
//!
//! [`RecordStore`] holds one table's [`MockRecord`] plus the field schema
//! [`super::table_meta`] recovers from the workspace: the field-name/number
//! map, FlowField formulas, typed field defaults and lengths. [`TableRef`]
//! says which store a record variable reads and writes: a plain `Record "X"`
//! shares the one store keyed by table name; a `Record "X" temporary` gets
//! its own store keyed by the owning variable's handle.

use std::collections::HashMap;
use std::sync::Arc;

use al_syntax::IdentifierText;

use crate::interpreter::dispatch::{DispatchCtx, DispatchMode};
use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{RecordValue, Value};
use crate::mock::calcformula_parser::CalcFormula;
use crate::mock::record::{FieldNo, MockRecord, RecordView};

use super::table_meta::load_table_meta;

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
    pub(super) flowfields: HashMap<FieldNo, CalcFormula>,
    /// Field number → the typed default value of the declared field type
    /// (`0` / `''` / `false` / `0D` …). Reads of never-assigned fields return
    /// this default (BC zero-initialises every field); fields with types the
    /// interpreter cannot default are absent.
    pub(super) field_defaults: HashMap<FieldNo, Value>,
    /// Field number → the declared `Text[N]`/`Code[N]` capacity. A longer
    /// value assigned to the field is a runtime error, as on BC.
    field_lengths: HashMap<FieldNo, usize>,
    /// Per-record-variable view state (filters/cursor/buffer), keyed by the
    /// variable's handle. BC gives each record variable independent state over
    /// the shared physical table.
    pub(super) views: HashMap<u64, RecordView>,
}

impl RecordStore {
    /// Resolve a field name through the parsed workspace schema.
    pub(super) fn resolve_field(&self, name: &str) -> Result<FieldNo, String> {
        let key = name.unquote_identifier().to_ascii_lowercase();
        self.field_by_name.get(&key).copied().ok_or_else(|| {
            format!(
                "field '{}' is not declared on workspace table '{}'",
                name.unquote_identifier(),
                self.record.table_name
            )
        })
    }

    /// Take the view for `handle` out of the store, creating a fresh one for a
    /// first use. The caller MUST put it back with [`RecordStore::put_view`]
    /// on every path.
    pub(super) fn take_view(&mut self, handle: u64) -> RecordView {
        self.views
            .remove(&handle)
            .unwrap_or_else(|| self.record.new_view())
    }

    pub(super) fn put_view(&mut self, handle: u64, view: RecordView) {
        self.views.insert(handle, view);
    }

    /// Coerce a value being written into `field` to the field's declared type
    /// (Code caselessness, integer width, Decimal promotion). Unknown field
    /// types store the value as-is.
    pub(super) fn coerce_to_field(&self, field: FieldNo, value: Value) -> Result<Value, String> {
        match self.field_defaults.get(&field) {
            Some(default) => {
                Value::coerce_into_slot(default, value, self.field_lengths.get(&field).copied())
            }
            None => Ok(value),
        }
    }
}

pub(super) fn records_enabled(ctx: &DispatchCtx) -> bool {
    matches!(ctx.mode, DispatchMode::WithRecords)
}

pub(super) fn records_disabled_error() -> Eval {
    eval_error("record access is unavailable in pure-logic interpreter mode")
}

/// Lowercased table-name store key.
fn key_for(table_name: &str) -> String {
    table_name.unquote_identifier().to_ascii_lowercase()
}

/// Which backing store a record variable reads and writes.
///
/// A plain `Record "X"` shares one store per table, the way every AL variable
/// over a physical table sees the same rows. A `Record "X" temporary` does
/// not: its rows live in the variable, isolated from the physical table and
/// from every other temporary variable, so its store is keyed per variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableRef {
    /// Declared table name. Loads the workspace table definition.
    pub name: String,
    /// The owning variable's view handle, for a `temporary` declaration.
    pub temp_owner: Option<u64>,
}

impl TableRef {
    pub(crate) fn persistent(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            temp_owner: None,
        }
    }

    pub(super) fn key(&self) -> String {
        match self.temp_owner {
            Some(owner) => format!("{} temporary#{owner}", key_for(&self.name)),
            None => key_for(&self.name),
        }
    }
}

/// Ensure a backing store exists for `table`, building it from the workspace
/// table definition on first use. Returns the store key on success.
pub(super) fn ensure_store(ctx: &mut DispatchCtx, table: &TableRef) -> Result<String, String> {
    let key = table.key();
    if ctx.records.contains_key(&key) {
        return Ok(key);
    }
    let source = Arc::clone(&ctx.source);
    let meta = load_table_meta(&*source, &table.name)?;
    let store = RecordStore {
        record: MockRecord::new(meta.table_id, meta.table_name, meta.pk_fields)
            .with_field_defaults(meta.field_defaults.clone()),
        field_by_name: meta.field_by_name,
        flowfields: meta.flowfields,
        field_defaults: meta.field_defaults,
        field_lengths: meta.field_lengths,
        views: HashMap::new(),
    };
    ctx.records.insert(key.clone(), store);
    Ok(key)
}

/// Resolve the record variable named `recv` to its `(TableRef, handle)` pair,
/// allocating a fresh per-variable view handle on first use and writing it
/// back onto the variable's `RecordValue`. Returns `None` when `recv` is not a
/// bound record variable.
pub(crate) fn record_binding(
    recv: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<(TableRef, u64)> {
    let (table_name, temporary) = match stack.lookup(recv) {
        Some(Value::Record(rv)) => (rv.table_name.clone(), rv.temporary),
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
    let table = TableRef {
        name: table_name,
        temp_owner: temporary.then_some(handle),
    };
    Some((table, handle))
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
    let caller_table = TableRef {
        name: rv.table_name.clone(),
        temp_owner: rv.temporary.then_some(caller_handle),
    };
    let key = caller_table.key();
    ctx.next_record_handle += 1;
    let handle = ctx.next_record_handle;
    let Some(store) = ctx.records.get(&key) else {
        // The table was never materialized, so there is no view state to copy.
        // Leave the handle cleared; the callee allocates its own on demand.
        return;
    };
    // A temporary record variable holds the rows, so copying the variable
    // copies the whole in-memory table, not just the current buffer.
    if rv.temporary {
        let mut copy = store.clone();
        let mut view = copy.record.new_view();
        if let Some(caller_view) = copy.views.get(&caller_handle) {
            view.copy_buffers_from(caller_view);
        }
        copy.views.clear();
        copy.put_view(handle, view);
        let callee_key = TableRef {
            name: rv.table_name.clone(),
            temp_owner: Some(handle),
        }
        .key();
        ctx.records.insert(callee_key, copy);
        rv.handle = Some(handle);
        return;
    }
    let store = ctx.records.get_mut(&key).expect("store present above");
    let mut view = store.record.new_view();
    if let Some(caller_view) = store.views.get(&caller_handle) {
        view.copy_buffers_from(caller_view);
    }
    store.put_view(handle, view);
    rv.handle = Some(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_stores_are_keyed_per_variable() {
        let persistent = TableRef::persistent("Sales Line");
        let temp_a = TableRef {
            name: "Sales Line".into(),
            temp_owner: Some(7),
        };
        let temp_b = TableRef {
            name: "Sales Line".into(),
            temp_owner: Some(8),
        };
        assert_eq!(persistent.key(), "sales line");
        assert_ne!(temp_a.key(), persistent.key());
        assert_ne!(temp_a.key(), temp_b.key());
    }
}
