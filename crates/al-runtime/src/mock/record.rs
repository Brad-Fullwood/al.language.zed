//! Mock BC record table store.
//!
//! Each `MockRecord` is an independent in-memory table that mimics the BC
//! Record variable data-model: a `BTreeMap` keyed by composite primary key,
//! with per-field values and filter state for iteration.
//!
//! This module does **not** wire into the interpreter's `Value::Record(handle)`
//! semantics — the handle mapping is the interpreter's responsibility.

use std::collections::BTreeMap;
use thiserror::Error;

use crate::interpreter::value::{Decimal, Value};
use crate::mock::filter::{self, FilterExpr};

/// A field number, matching BC's integer field-number convention.
pub type FieldNo = i32;

pub type PrimaryKey = Vec<Value>;

pub type Row = BTreeMap<FieldNo, Value>;

/// The aggregation a FlowField `CalcFormula` performs over the referenced
/// table. `Linked` is intentionally absent — it is a record-relationship
/// marker, not an aggregation, so the interpreter does not model it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowAgg {
    Sum,
    Average,
    Min,
    Max,
    Count,
    Exist,
    Lookup,
}

/// One resolved `WHERE` condition for a FlowField calculation, already mapped
/// to a field number in *this* (the referenced) table.
#[derive(Debug, Clone)]
pub enum FlowFilter {
    /// Exact value match — a `CONST(...)` literal or a `FIELD(...)` reference
    /// resolved to the calculating record's current value. Compared
    /// type-tolerantly (numeric values numerically, `Text`/`Code`/`Option`
    /// textually and case-insensitively) so cross-type schemas still match,
    /// mirroring BC's value coercion inside FlowField filters.
    Eq(Value),
    /// A parsed BC filter expression from `FILTER(...)`.
    Expr(FilterExpr),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum RecordError {
    #[error("record not found")]
    NotFound,
    #[error("duplicate primary key")]
    DuplicateKey,
    #[error("primary key field {0} has no value in current row")]
    MissingKeyField(FieldNo),
    #[error("no current row (use FindFirst/FindSet before Next)")]
    NoCurrentRow,
    #[error("end of set reached")]
    EndOfSet,
    #[error("filter parse error for field {0}: {1}")]
    FilterParse(FieldNo, String),
    #[error("{0} trigger execution requires live Business Central")]
    TriggerExecutionUnsupported(&'static str),
    #[error("arithmetic overflow while calculating FlowField {0}")]
    FlowArithmeticOverflow(&'static str),
    #[error("invalid FIND direction '{0}' (expected '-' or '+')")]
    InvalidFindDirection(char),
}

#[derive(Debug, Clone)]
enum FieldFilter {
    /// Exact single value (SetRange with lo == hi).
    Range(Value, Value),
    /// Parsed BC filter expression (SetFilter).
    Expr(FilterExpr),
}

impl FieldFilter {
    fn matches(&self, value: &Value) -> bool {
        match self {
            // A `Code` cell compares
            // caselessly and a numeric cell numerically, so `SetRange("No.",
            // 'ABC')` matches a stored `'abc'` and cross-type `Text`/`Code`
            // bounds don't fall out via `Value`'s variant-tag ordering.
            FieldFilter::Range(lo, hi) => {
                field_cmp(value, lo).is_some_and(|o| o.is_ge())
                    && field_cmp(value, hi).is_some_and(|o| o.is_le())
            }
            FieldFilter::Expr(expr) => filter::matches(expr, value),
        }
    }
}

/// Compare a stored field `value` against a filter `bound` with BC field
/// semantics: numeric fields numerically (tolerant of Integer/Decimal mix),
/// a `Code` cell caselessly, a `Text` cell case-sensitively, and other scalars
/// by their natural order. A never-assigned cell (`Empty`) compares as the
/// typed zero of the bound (0 / "" / false / 0D), matching BC's default-value
/// semantics. Returns `None` when the two are not comparable.
fn field_cmp(value: &Value, bound: &Value) -> Option<std::cmp::Ordering> {
    if matches!(value, Value::Empty) && !matches!(bound, Value::Empty) {
        let zero = zero_like(bound)?;
        return field_cmp(&zero, bound);
    }
    if let (Some(x), Some(y)) = (as_number(value), as_number(bound)) {
        return Some(x.cmp(&y));
    }
    if let (Some(x), Some(y)) = (flow_text(value), flow_text(bound)) {
        return Some(if matches!(value, Value::Code(_)) {
            x.to_ascii_uppercase().cmp(&y.to_ascii_uppercase())
        } else {
            x.cmp(&y)
        });
    }
    // Same-variant non-string scalars (Date/Time/DateTime/Boolean/…).
    if std::mem::discriminant(value) == std::mem::discriminant(bound) {
        return Some(value.cmp(bound));
    }
    None
}

/// The typed zero value matching `bound`'s type, used to compare unset
/// (`Empty`) cells with BC's default-value semantics.
fn zero_like(bound: &Value) -> Option<Value> {
    Some(match bound {
        Value::Integer(_) | Value::BigInteger(_) => Value::Integer(0),
        Value::Decimal(_) => Value::Decimal(Decimal::ZERO),
        Value::Boolean(_) => Value::Boolean(false),
        Value::Text(_) => Value::Text(String::new()),
        Value::Code(_) => Value::Code(String::new()),
        Value::Date(_) => Value::Date(0),
        Value::Time(_) => Value::Time(0),
        Value::DateTime(_) => Value::DateTime(0),
        Value::Duration(_) => Value::Duration(0),
        Value::Char(_) => Value::Char('\0'),
        _ => return None,
    })
}

/// The current-key fields that determine iteration order.
/// Defaults to the primary key fields.
#[derive(Debug, Clone, Default)]
struct SortKey {
    /// Field numbers, in priority order.
    fields: Vec<FieldNo>,
}

impl SortKey {
    fn from_fields(fields: Vec<FieldNo>) -> Self {
        SortKey { fields }
    }

    fn key_of(&self, row: &Row) -> Vec<Value> {
        self.fields
            .iter()
            .map(|f| row.get(f).cloned().unwrap_or(Value::Empty))
            .collect()
    }
}

/// Per-record-variable state over a shared physical table: the row buffer,
/// xRec snapshot, active filters, sort key, and iteration cursor.
///
/// BC gives every record *variable* of a table its own filter set, cursor,
/// and buffer while all variables read/write one physical table. The
/// interpreter therefore keeps one [`MockRecord`] per table (the rows) and
/// one `RecordView` per record variable.
#[derive(Debug, Clone, Default)]
pub struct RecordView {
    /// The current row's field values (the "buffer").
    current: Row,
    /// Snapshot of `current` before the last Modify/Rename (xRec).
    x_rec: Row,
    filters: BTreeMap<FieldNo, FieldFilter>,
    sort_key: SortKey,
    /// Filtered, sorted keys ready for iteration (built by FindFirst/FindSet).
    iter_set: Vec<PrimaryKey>,
    iter_pos: Option<usize>,
}

/// Normalize one primary-key component so key lookup follows BC field
/// semantics: `Code` keys are caseless (uppercased) and the Integer/Decimal
/// numeric class unifies (an `Integer` key value matches a stored `Decimal`
/// one). Everything else keys by its exact value.
fn normalize_key_value(value: &Value) -> Value {
    match value {
        Value::Code(s) => Value::Code(s.to_uppercase()),
        Value::Integer(n) | Value::BigInteger(n) => Value::Decimal(Decimal::from(*n)),
        other => other.clone(),
    }
}

fn normalize_key(key: &[Value]) -> PrimaryKey {
    key.iter().map(normalize_key_value).collect()
}

fn row_matches_filters(filters: &BTreeMap<FieldNo, FieldFilter>, row: &Row) -> bool {
    for (&field, filter) in filters {
        let value = row.get(&field).unwrap_or(&Value::Empty);
        if !filter.matches(value) {
            return false;
        }
    }
    true
}

/// An in-memory BC record table.
///
/// Supports the standard BC Record API: Init, Get, Insert, Modify, Delete,
/// Rename, FindFirst/FindLast/FindSet/Find, Next, SetRange, SetFilter,
/// IsEmpty, Count, DeleteAll.
///
/// Rows are the shared physical table. View state (buffer/filters/cursor)
/// lives in a [`RecordView`]: the `*_in` methods take an explicit view (one
/// per record variable), and the plain methods delegate to a built-in default
/// view for single-variable callers and unit tests.
#[derive(Debug, Clone)]
pub struct MockRecord {
    pub table_id: i32,
    pub table_name: String,
    primary_key_fields: Vec<FieldNo>,
    rows: BTreeMap<PrimaryKey, Row>,
    /// Built-in view backing the plain (view-less) API.
    view: RecordView,
}

impl MockRecord {
    pub fn new(
        table_id: i32,
        table_name: impl Into<String>,
        primary_key_fields: Vec<FieldNo>,
    ) -> Self {
        let table_name = table_name.into();
        let sort_key = SortKey::from_fields(primary_key_fields.clone());
        MockRecord {
            table_id,
            table_name,
            primary_key_fields: primary_key_fields.clone(),
            rows: BTreeMap::new(),
            view: RecordView {
                sort_key,
                ..RecordView::default()
            },
        }
    }

    /// A fresh, unfiltered view of this table sorted by the primary key —
    /// the state a newly declared record variable starts with.
    pub fn new_view(&self) -> RecordView {
        RecordView {
            sort_key: SortKey::from_fields(self.primary_key_fields.clone()),
            ..RecordView::default()
        }
    }

    /// Run `f` against the built-in default view. Temporarily takes the view
    /// out of `self` so `f` can borrow the table and the view disjointly.
    fn with_default_view<R>(&mut self, f: impl FnOnce(&mut Self, &mut RecordView) -> R) -> R {
        let mut view = std::mem::take(&mut self.view);
        let result = f(self, &mut view);
        self.view = view;
        result
    }

    pub fn field_set_in(&self, view: &mut RecordView, field: FieldNo, value: Value) {
        let _ = self;
        view.current.insert(field, value);
    }

    pub fn field_set(&mut self, field: FieldNo, value: Value) {
        self.view.current.insert(field, value);
    }

    pub fn field_get_in<'a>(&self, view: &'a RecordView, field: FieldNo) -> Option<&'a Value> {
        let _ = self;
        view.current.get(&field)
    }

    pub fn field_get(&self, field: FieldNo) -> Option<&Value> {
        self.view.current.get(&field)
    }

    pub fn primary_key_len(&self) -> usize {
        self.primary_key_fields.len()
    }

    /// The primary-key field numbers, in key order.
    pub fn primary_key_fields(&self) -> &[FieldNo] {
        &self.primary_key_fields
    }

    fn current_primary_key(&self, view: &RecordView) -> Result<PrimaryKey, RecordError> {
        self.primary_key_fields
            .iter()
            .map(|&f| {
                view.current
                    .get(&f)
                    .map(normalize_key_value)
                    .ok_or(RecordError::MissingKeyField(f))
            })
            .collect()
    }

    /// `INIT` — reset non-key fields to defaults, but PRESERVE primary-key
    /// fields. BC's `Init` keeps the key so the ubiquitous idiom
    /// `Rec."No." := X; Rec.Init(); Rec.Insert();` inserts under `X`; clearing
    /// the whole buffer here loses the key and fails the insert.
    pub fn init_in(&self, view: &mut RecordView) {
        let preserved: Vec<(FieldNo, Value)> = self
            .primary_key_fields
            .iter()
            .filter_map(|f| view.current.get(f).map(|v| (*f, v.clone())))
            .collect();
        view.current.clear();
        view.x_rec.clear();
        view.iter_pos = None;
        for (f, v) in preserved {
            view.current.insert(f, v);
        }
    }

    pub fn init(&mut self) {
        self.with_default_view(|table, view| table.init_in(view));
    }

    /// `RESET` — clear all filters and the sort key; reset to primary key order.
    pub fn reset_in(&self, view: &mut RecordView) {
        view.filters.clear();
        view.sort_key = SortKey::from_fields(self.primary_key_fields.clone());
        view.iter_set.clear();
        view.iter_pos = None;
    }

    pub fn reset(&mut self) {
        self.with_default_view(|table, view| table.reset_in(view));
    }

    /// `GET(key_parts…)` — look up a row by primary key; load into buffer.
    pub fn get_in(&self, view: &mut RecordView, key: PrimaryKey) -> Result<(), RecordError> {
        let key = normalize_key(&key);
        let row = self.rows.get(&key).ok_or(RecordError::NotFound)?;
        view.current = row.clone();
        view.x_rec = row.clone();
        Ok(())
    }

    pub fn get(&mut self, key: PrimaryKey) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.get_in(view, key))
    }

    /// `INSERT` — insert the current buffer as a new row.
    ///
    /// The local store cannot execute table triggers. A caller that explicitly
    /// requests trigger execution must be routed to live Business Central.
    pub fn insert_in(
        &mut self,
        view: &mut RecordView,
        run_trigger: bool,
    ) -> Result<(), RecordError> {
        if run_trigger {
            return Err(RecordError::TriggerExecutionUnsupported("Insert"));
        }
        let key = self.current_primary_key(view)?;
        if self.rows.contains_key(&key) {
            return Err(RecordError::DuplicateKey);
        }
        self.rows.insert(key, view.current.clone());
        // BC behaviour: after Insert, xRec mirrors the inserted row (Rec).
        view.x_rec = view.current.clone();
        Ok(())
    }

    pub fn insert(&mut self, run_trigger: bool) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.insert_in(view, run_trigger))
    }

    /// `MODIFY` — overwrite the existing row with the current buffer.
    ///
    /// Saves the prior row as `xRec`.
    pub fn modify_in(
        &mut self,
        view: &mut RecordView,
        run_trigger: bool,
    ) -> Result<(), RecordError> {
        if run_trigger {
            return Err(RecordError::TriggerExecutionUnsupported("Modify"));
        }
        let key = self.current_primary_key(view)?;
        let old_row = self.rows.get_mut(&key).ok_or(RecordError::NotFound)?;
        view.x_rec = old_row.clone();
        *old_row = view.current.clone();
        Ok(())
    }

    pub fn modify(&mut self, run_trigger: bool) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.modify_in(view, run_trigger))
    }

    /// `DELETE` — remove the row matching the current buffer's primary key.
    pub fn delete_in(
        &mut self,
        view: &mut RecordView,
        run_trigger: bool,
    ) -> Result<(), RecordError> {
        if run_trigger {
            return Err(RecordError::TriggerExecutionUnsupported("Delete"));
        }
        let key = self.current_primary_key(view)?;
        self.rows.remove(&key).ok_or(RecordError::NotFound)?;
        // Keep `iter_pos` intact: BC's canonical delete loop
        // `if FindSet then repeat Delete until Next() = 0` relies on `Next`
        // advancing from the current position to the next surviving key. The
        // deleted key stays in `iter_set` (a snapshot) and is simply stepped
        // over; clearing `iter_pos` here made the next `Next()` error with
        // "no current row" and abort the loop after a single delete.
        Ok(())
    }

    pub fn delete(&mut self, run_trigger: bool) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.delete_in(view, run_trigger))
    }

    /// `DELETEALL` — remove every row matching the view's filters.
    ///
    /// Collects the matching keys once and removes them directly, so deleting
    /// n rows costs one pass over the table instead of the O(n² log n)
    /// find-first-then-delete loop. Returns the number of rows removed.
    pub fn delete_all_in(
        &mut self,
        view: &mut RecordView,
        run_trigger: bool,
    ) -> Result<usize, RecordError> {
        if run_trigger {
            return Err(RecordError::TriggerExecutionUnsupported("Delete"));
        }
        let doomed: Vec<PrimaryKey> = self
            .rows
            .iter()
            .filter(|(_, row)| row_matches_filters(&view.filters, row))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            self.rows.remove(key);
        }
        view.iter_set.clear();
        view.iter_pos = None;
        Ok(doomed.len())
    }

    pub fn delete_all(&mut self, run_trigger: bool) -> Result<usize, RecordError> {
        self.with_default_view(|table, view| table.delete_all_in(view, run_trigger))
    }

    /// `RENAME(new_key)` — move the current row to a new primary key.
    ///
    /// The new key values must be provided as a `Vec<(FieldNo, Value)>` that
    /// covers all primary key fields. Saves the old row as `xRec`.
    pub fn rename_in(
        &mut self,
        view: &mut RecordView,
        new_key_values: Vec<(FieldNo, Value)>,
    ) -> Result<(), RecordError> {
        let old_key = self.current_primary_key(view)?;
        let old_row = self.rows.remove(&old_key).ok_or(RecordError::NotFound)?;
        view.x_rec = old_row.clone();
        let mut new_row = old_row;
        for (field, value) in new_key_values {
            new_row.insert(field, value.clone());
            view.current.insert(field, value);
        }
        let new_key = self.current_primary_key(view)?;
        if self.rows.contains_key(&new_key) {
            self.rows.insert(old_key, view.x_rec.clone());
            return Err(RecordError::DuplicateKey);
        }
        self.rows.insert(new_key, new_row);
        Ok(())
    }

    pub fn rename(&mut self, new_key_values: Vec<(FieldNo, Value)>) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.rename_in(view, new_key_values))
    }

    /// `SETCURRENTKEY(fields…)` — change iteration sort order.
    pub fn set_current_key_in(&self, view: &mut RecordView, fields: Vec<FieldNo>) {
        let _ = self;
        view.sort_key = SortKey::from_fields(fields);
        view.iter_set.clear();
        view.iter_pos = None;
    }

    pub fn set_current_key(&mut self, fields: Vec<FieldNo>) {
        self.with_default_view(|table, view| table.set_current_key_in(view, fields));
    }

    /// `SETRANGE(field, low, high)` — filter a field to an inclusive value range.
    pub fn set_range_in(&self, view: &mut RecordView, field: FieldNo, low: Value, high: Value) {
        let _ = self;
        view.filters.insert(field, FieldFilter::Range(low, high));
        view.iter_set.clear();
        view.iter_pos = None;
    }

    pub fn set_range(&mut self, field: FieldNo, low: Value, high: Value) {
        self.with_default_view(|table, view| table.set_range_in(view, field, low, high));
    }

    /// Remove the active filter for one field.
    pub fn clear_filter_in(&self, view: &mut RecordView, field: FieldNo) {
        let _ = self;
        view.filters.remove(&field);
        view.iter_set.clear();
        view.iter_pos = None;
    }

    pub fn clear_filter(&mut self, field: FieldNo) {
        self.with_default_view(|table, view| table.clear_filter_in(view, field));
    }

    /// `SETFILTER(field, expr)` — set a BC filter expression on a field.
    pub fn set_filter_in(
        &self,
        view: &mut RecordView,
        field: FieldNo,
        expr: &str,
    ) -> Result<(), RecordError> {
        let _ = self;
        let parsed =
            filter::parse(expr).map_err(|e| RecordError::FilterParse(field, e.to_string()))?;
        view.filters.insert(field, FieldFilter::Expr(parsed));
        view.iter_set.clear();
        view.iter_pos = None;
        Ok(())
    }

    pub fn set_filter(&mut self, field: FieldNo, expr: &str) -> Result<(), RecordError> {
        self.with_default_view(|table, view| table.set_filter_in(view, field, expr))
    }

    fn build_iter_set(&self, view: &mut RecordView) {
        let mut keys: Vec<PrimaryKey> = self
            .rows
            .iter()
            .filter_map(|(key, row)| {
                if row_matches_filters(&view.filters, row) {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();

        let sort_key = view.sort_key.clone();
        keys.sort_by(|a, b| {
            let row_a = self.rows.get(a).unwrap();
            let row_b = self.rows.get(b).unwrap();
            sort_key.key_of(row_a).cmp(&sort_key.key_of(row_b))
        });

        view.iter_set = keys;
    }

    fn load_row_at(&self, view: &mut RecordView, pos: usize) -> Result<(), RecordError> {
        let key = view.iter_set.get(pos).ok_or(RecordError::EndOfSet)?.clone();
        let row = self.rows.get(&key).ok_or(RecordError::NotFound)?;
        view.current = row.clone();
        view.x_rec = row.clone();
        view.iter_pos = Some(pos);
        Ok(())
    }

    /// `FINDFIRST` — position on the first matching record.
    pub fn find_first_in(&self, view: &mut RecordView) -> Result<bool, RecordError> {
        self.build_iter_set(view);
        if view.iter_set.is_empty() {
            view.iter_pos = None;
            return Ok(false);
        }
        self.load_row_at(view, 0)?;
        Ok(true)
    }

    pub fn find_first(&mut self) -> Result<bool, RecordError> {
        self.with_default_view(|table, view| table.find_first_in(view))
    }

    /// `FINDLAST` — position on the last matching record.
    pub fn find_last_in(&self, view: &mut RecordView) -> Result<bool, RecordError> {
        self.build_iter_set(view);
        let last = view.iter_set.len().saturating_sub(1);
        if view.iter_set.is_empty() {
            view.iter_pos = None;
            return Ok(false);
        }
        self.load_row_at(view, last)?;
        Ok(true)
    }

    pub fn find_last(&mut self) -> Result<bool, RecordError> {
        self.with_default_view(|table, view| table.find_last_in(view))
    }

    /// `FINDSET` — prepare iteration set and position at first record.
    ///
    /// Returns `false` if the set is empty (no rows match filters).
    pub fn find_set_in(&self, view: &mut RecordView) -> Result<bool, RecordError> {
        self.find_first_in(view)
    }

    pub fn find_set(&mut self) -> Result<bool, RecordError> {
        self.with_default_view(|table, view| table.find_set_in(view))
    }

    /// `FIND('-')` / `FIND('+')` — position at first (−) or last (+) record.
    pub fn find_in(&self, view: &mut RecordView, direction: char) -> Result<bool, RecordError> {
        match direction {
            '-' => self.find_first_in(view),
            '+' => self.find_last_in(view),
            _ => Err(RecordError::InvalidFindDirection(direction)),
        }
    }

    pub fn find(&mut self, direction: char) -> Result<bool, RecordError> {
        self.with_default_view(|table, view| table.find_in(view, direction))
    }

    /// `NEXT` — advance to the next (or previous) record.
    ///
    /// `steps` is typically 1 (forward) or -1 (backward), matching BC's
    /// `NEXT(steps)` signature.  Returns `Ok(steps_actually_moved)`.
    pub fn next_in(&self, view: &mut RecordView, steps: i32) -> Result<i32, RecordError> {
        let current_pos = view.iter_pos.ok_or(RecordError::NoCurrentRow)?;
        if steps == 0 || view.iter_set.is_empty() {
            return Ok(0);
        }
        // BC moves as far as possible toward the target and returns the number
        // of steps ACTUALLY taken, rather than staying put and returning 0 on
        // overshoot. `until Next() = 0` loops behave the same either way,
        // but a batch `Next(N)` that overshoots the end now advances to the
        // boundary and reports the partial move.
        let last = view.iter_set.len() as i64 - 1;
        let target = current_pos as i64 + steps as i64;
        let clamped = target.clamp(0, last);
        let actual = clamped - current_pos as i64;
        if actual == 0 {
            return Ok(0);
        }
        self.load_row_at(view, clamped as usize)?;
        Ok(actual as i32)
    }

    pub fn next(&mut self, steps: i32) -> Result<i32, RecordError> {
        self.with_default_view(|table, view| table.next_in(view, steps))
    }

    /// `ISEMPTY` — `true` if no rows match the view's filters.
    pub fn is_empty_in(&self, view: &RecordView) -> bool {
        self.rows
            .values()
            .all(|row| !row_matches_filters(&view.filters, row))
    }

    pub fn is_empty(&self) -> bool {
        self.is_empty_in(&self.view)
    }

    /// `COUNT` — number of rows matching the view's filters.
    pub fn count_in(&self, view: &RecordView) -> usize {
        self.rows
            .values()
            .filter(|row| row_matches_filters(&view.filters, row))
            .count()
    }

    pub fn count(&self) -> usize {
        self.count_in(&self.view)
    }

    pub fn x_rec(&self) -> &Row {
        &self.view.x_rec
    }

    pub fn x_rec_field(&self, field: FieldNo) -> Option<&Value> {
        self.view.x_rec.get(&field)
    }

    /// Evaluate a FlowField `CalcFormula` aggregation over this table's rows.
    ///
    /// This is a **pure read**: it does not touch the current buffer, the active
    /// filters, the sort key or the iteration cursor, so it is safe to run
    /// against a store that another record variable is mid-iteration on (stores
    /// are shared per table name — see `interpreter::records`).
    ///
    /// `conditions` are `(field, filter)` pairs that **all** must match (AND
    /// semantics). `target` is the field being aggregated and is ignored for
    /// `Count`/`Exist`. Numeric aggregates (`Sum`/`Min`/`Max`) return `Integer`
    /// when every contributing cell is an `Integer`, otherwise `Decimal`;
    /// `Average` always returns `Decimal`. An empty match set yields
    /// `Integer(0)` for `Sum`/`Min`/`Max`/`Count`, `Decimal(0)` for `Average`,
    /// `Boolean(false)` for `Exist`, and `Empty` for `Lookup`.
    pub fn calc_flow(
        &self,
        conditions: &[(FieldNo, FlowFilter)],
        target: Option<FieldNo>,
        agg: FlowAgg,
    ) -> Result<Value, RecordError> {
        let matching: Vec<&Row> = self
            .rows
            .values()
            .filter(|row| {
                conditions.iter().all(|(field, filt)| {
                    flow_filter_matches(filt, row.get(field).unwrap_or(&Value::Empty))
                })
            })
            .collect();

        // The cells of the aggregated field across the matching rows.
        let target_cells = || {
            matching
                .iter()
                .filter_map(|row| target.and_then(|t| row.get(&t)))
        };

        match agg {
            FlowAgg::Count => Ok(Value::Integer(
                i64::try_from(matching.len())
                    .map_err(|_| RecordError::FlowArithmeticOverflow("Count"))?,
            )),
            FlowAgg::Exist => Ok(Value::Boolean(!matching.is_empty())),
            FlowAgg::Sum => {
                let mut int_sum: i64 = 0;
                let mut dec_sum = Decimal::ZERO;
                let mut any_decimal = false;
                for cell in target_cells() {
                    match cell {
                        Value::Integer(n) | Value::BigInteger(n) => {
                            int_sum = int_sum
                                .checked_add(*n)
                                .ok_or(RecordError::FlowArithmeticOverflow("Sum"))?;
                            dec_sum = dec_sum
                                .checked_add(Decimal::from(*n))
                                .ok_or(RecordError::FlowArithmeticOverflow("Sum"))?;
                        }
                        Value::Decimal(d) => {
                            any_decimal = true;
                            dec_sum = dec_sum
                                .checked_add(*d)
                                .ok_or(RecordError::FlowArithmeticOverflow("Sum"))?;
                        }
                        _ => {}
                    }
                }
                if any_decimal {
                    Ok(Value::Decimal(dec_sum))
                } else {
                    Ok(Value::Integer(int_sum))
                }
            }
            FlowAgg::Average => {
                let nums: Vec<Decimal> = target_cells().filter_map(as_number).collect();
                if nums.is_empty() {
                    Ok(Value::Decimal(Decimal::ZERO))
                } else {
                    let sum = nums.iter().copied().try_fold(Decimal::ZERO, |sum, value| {
                        sum.checked_add(value)
                            .ok_or(RecordError::FlowArithmeticOverflow("Average"))
                    })?;
                    let divisor = Decimal::from(nums.len());
                    Ok(Value::Decimal(
                        sum.checked_div(divisor)
                            .ok_or(RecordError::FlowArithmeticOverflow("Average"))?,
                    ))
                }
            }
            FlowAgg::Min | FlowAgg::Max => {
                let mut best: Option<&Value> = None;
                for cell in target_cells() {
                    let Some(cur) = as_number(cell) else { continue };
                    best = match best {
                        None => Some(cell),
                        Some(prev) => {
                            let prev_n = as_number(prev).unwrap_or(cur);
                            let take = match agg {
                                FlowAgg::Min => cur < prev_n,
                                _ => cur > prev_n,
                            };
                            if take {
                                Some(cell)
                            } else {
                                Some(prev)
                            }
                        }
                    };
                }
                Ok(best.cloned().unwrap_or(Value::Integer(0)))
            }
            FlowAgg::Lookup => Ok(target_cells().next().cloned().unwrap_or(Value::Empty)),
        }
    }
}

/// Numeric view of a value for FlowField aggregation (`Integer`/`Decimal`).
fn as_number(v: &Value) -> Option<Decimal> {
    v.as_decimal()
}

/// Test one resolved FlowField condition against a row's cell value.
fn flow_filter_matches(filt: &FlowFilter, cell: &Value) -> bool {
    match filt {
        FlowFilter::Eq(want) => flow_value_eq(want, cell),
        FlowFilter::Expr(expr) => filter::matches(expr, cell),
    }
}

/// Type-tolerant equality used by `FlowFilter::Eq`: exact `Value` equality,
/// else numeric equality, else case-insensitive text equality.
fn flow_value_eq(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    if let (Some(x), Some(y)) = (as_number(a), as_number(b)) {
        return x == y;
    }
    match (flow_text(a), flow_text(b)) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(&y),
        _ => false,
    }
}

/// Textual view of a value for `flow_value_eq` (`Text`/`Code`/`Option` member,
/// plus a `Boolean` bridge so `WHERE(Flag = CONST(true))` matches a Boolean
/// cell whichever side was parsed as text).
fn flow_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) | Value::Code(s) | Value::Guid(s) => Some(s.clone()),
        Value::Option { member, .. } => Some(member.clone()),
        Value::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_table() -> MockRecord {
        MockRecord::new(27, "Item", vec![1])
    }

    /// Helper: build a row with field 1 = no, field 2 = description.
    fn row_simple(no: i64, desc: &str) -> (Row, PrimaryKey) {
        let mut row = BTreeMap::new();
        row.insert(1, Value::Integer(no));
        row.insert(2, Value::Text(desc.to_string()));
        let key = vec![Value::Integer(no)];
        (row, key)
    }

    fn insert_row(rec: &mut MockRecord, no: i64, desc: &str) {
        let (row, _) = row_simple(no, desc);
        for (f, v) in &row {
            rec.field_set(*f, v.clone());
        }
        rec.insert(false).expect("insert should succeed");
    }

    #[test]
    fn test_insert_then_get_matches() {
        let mut rec = make_table();
        insert_row(&mut rec, 1000, "Bike");
        let mut rec2 = rec.clone();
        rec2.get(vec![Value::Integer(1000)]).unwrap();
        assert_eq!(rec2.field_get(2), Some(&Value::Text("Bike".to_string())));
    }

    #[test]
    fn test_delete_then_is_empty() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "X");
        rec.field_set(1, Value::Integer(1));
        rec.delete(false).unwrap();
        assert!(rec.is_empty());
    }

    #[test]
    fn test_modify_xrec_preserved() {
        let mut rec = make_table();
        insert_row(&mut rec, 100, "OriginalName");

        rec.get(vec![Value::Integer(100)]).unwrap();
        let old_desc = rec.field_get(2).cloned().unwrap();
        rec.field_set(2, Value::Text("NewName".to_string()));
        rec.modify(false).unwrap();

        assert_eq!(rec.x_rec_field(2), Some(&old_desc));
        assert_eq!(rec.field_get(2), Some(&Value::Text("NewName".to_string())));
    }

    /// A `Code` field is caseless, so `SetRange("No.", 'ABC')`
    /// must match a stored `'abc'`, and a `Text` filter bound on a `Code` cell
    /// (different `Value` variants) must not fall out via variant-tag ordering.
    #[test]
    fn code_field_setrange_is_caseless() {
        let mut rec = MockRecord::new(50100, "CodeKeyed", vec![1]);
        for code in ["abc", "def", "xyz"] {
            rec.field_set(1, Value::Code(code.to_string()));
            rec.insert(false).expect("insert should succeed");
        }
        rec.set_range(1, Value::Text("ABC".into()), Value::Text("ABC".into()));
        assert_eq!(rec.count(), 1, "SetRange('ABC') must match stored 'abc'");

        rec.reset();
        rec.set_range(1, Value::Code("ABC".into()), Value::Code("DEF".into()));
        assert_eq!(
            rec.count(),
            2,
            "range [ABC..DEF] must match 'abc' and 'def'"
        );

        rec.reset();
        rec.set_range(1, Value::Text("QQQ".into()), Value::Text("QQQ".into()));
        assert_eq!(rec.count(), 0);
    }

    /// A `Text` field is case-sensitive; only `Code` is caseless.
    /// `SetRange('ABC')` on a Text cell must NOT match a stored `'abc'`.
    #[test]
    fn text_field_setrange_is_case_sensitive() {
        let mut rec = MockRecord::new(50101, "TextKeyed", vec![1]);
        for v in ["abc", "ABC", "AbC"] {
            rec.field_set(1, Value::Text(v.to_string()));
            rec.insert(false).expect("insert should succeed");
        }
        rec.set_range(1, Value::Text("ABC".into()), Value::Text("ABC".into()));
        assert_eq!(
            rec.count(),
            1,
            "Text SetRange('ABC') must match only the exact-case 'ABC'"
        );
    }

    /// A numeric field filter tolerates mixed Integer and Decimal bounds;
    /// an Integer cell in [1.5 .. 3.5] matches when the bounds are Decimals.
    #[test]
    fn numeric_field_setrange_mixes_integer_and_decimal() {
        let mut rec = MockRecord::new(50102, "NumKeyed", vec![1]);
        for n in 1..=5i64 {
            rec.field_set(1, Value::Integer(n));
            rec.insert(false).expect("insert should succeed");
        }
        rec.set_range(1, Value::Decimal(dec!(1.5)), Value::Decimal(dec!(3.5)));
        assert_eq!(rec.count(), 2, "integers 2 and 3 fall in [1.5..3.5]");
    }

    #[test]
    fn test_count_after_mass_delete_is_zero() {
        let mut rec = make_table();
        for i in 1..=10i64 {
            insert_row(&mut rec, i, "item");
        }
        assert_eq!(rec.count(), 10);

        for i in 1..=10i64 {
            rec.field_set(1, Value::Integer(i));
            rec.delete(false).unwrap();
        }
        assert_eq!(rec.count(), 0);
        assert!(rec.is_empty());
    }

    #[test]
    fn delete_during_findset_iteration_drains_the_set() {
        let mut rec = make_table();
        for i in 1..=5i64 {
            insert_row(&mut rec, i, "item");
        }
        assert!(rec.find_set().unwrap());
        loop {
            rec.delete(false).expect("delete current row");
            if rec.next(1).expect("next after delete must not error") == 0 {
                break;
            }
        }
        assert_eq!(rec.count(), 0, "delete loop must drain every row");
        assert!(rec.is_empty());
    }

    #[test]
    fn test_findset_iteration_order_matches_sort_key() {
        let mut rec = make_table();
        for i in [5i64, 3, 1, 4, 2] {
            insert_row(&mut rec, i, "x");
        }
        assert!(rec.find_set().unwrap());
        let mut order = vec![rec.field_get(1).unwrap().clone()];
        while rec.next(1).unwrap() != 0 {
            order.push(rec.field_get(1).unwrap().clone());
        }
        let expected: Vec<Value> = (1i64..=5).map(Value::Integer).collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn test_setrange_filters_before_iteration() {
        let mut rec = make_table();
        for i in 1i64..=10 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(3), Value::Integer(7));
        assert!(rec.find_set().unwrap());
        let mut seen = Vec::new();
        seen.push(rec.field_get(1).unwrap().clone());
        while rec.next(1).unwrap() != 0 {
            seen.push(rec.field_get(1).unwrap().clone());
        }
        let expected: Vec<Value> = (3i64..=7).map(Value::Integer).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn clear_filter_restores_unfiltered_iteration() {
        let mut rec = make_table();
        for i in 1i64..=3 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(2), Value::Integer(2));
        assert_eq!(rec.count(), 1);

        rec.clear_filter(1);

        assert_eq!(rec.count(), 3);
    }

    #[test]
    fn test_set_current_key_changes_iteration_order() {
        let mut rec = MockRecord::new(99, "Test", vec![1]);
        for (no, priority) in [(10i64, 30i64), (20, 10), (30, 20)] {
            rec.field_set(1, Value::Integer(no));
            rec.field_set(3, Value::Integer(priority));
            rec.insert(false).unwrap();
        }
        rec.set_current_key(vec![3]);
        assert!(rec.find_set().unwrap());
        let mut seen_no = Vec::new();
        seen_no.push(rec.field_get(1).unwrap().clone());
        while rec.next(1).unwrap() != 0 {
            seen_no.push(rec.field_get(1).unwrap().clone());
        }
        assert_eq!(
            seen_no,
            vec![Value::Integer(20), Value::Integer(30), Value::Integer(10)]
        );
    }

    #[test]
    fn test_rename_moves_row() {
        let mut rec = make_table();
        insert_row(&mut rec, 100, "Alpha");
        rec.get(vec![Value::Integer(100)]).unwrap();
        rec.rename(vec![(1, Value::Integer(200))]).unwrap();

        assert!(rec.get(vec![Value::Integer(100)]).is_err());
        rec.get(vec![Value::Integer(200)]).unwrap();
        assert_eq!(rec.field_get(2), Some(&Value::Text("Alpha".to_string())));
    }

    #[test]
    fn test_insert_duplicate_key_error() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "A");
        rec.field_set(1, Value::Integer(1));
        rec.field_set(2, Value::Text("B".to_string()));
        let err = rec.insert(false).unwrap_err();
        assert_eq!(err, RecordError::DuplicateKey);
    }

    #[test]
    fn test_get_missing_record_error() {
        let mut rec = make_table();
        let err = rec.get(vec![Value::Integer(999)]).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    #[test]
    fn test_modify_nonexistent_error() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(1));
        let err = rec.modify(false).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    #[test]
    fn test_delete_nonexistent_error() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(1));
        let err = rec.delete(false).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    #[test]
    fn test_next_without_findset_error() {
        let mut rec = make_table();
        let err = rec.next(1).unwrap_err();
        assert_eq!(err, RecordError::NoCurrentRow);
    }

    #[test]
    fn test_findset_empty_table_returns_false() {
        let mut rec = make_table();
        assert!(!rec.find_set().unwrap());
    }

    #[test]
    fn test_invalid_setfilter_returns_error() {
        let mut rec = make_table();
        assert!(rec.set_filter(1, "").is_err());
    }

    #[test]
    fn test_rename_to_existing_key_error() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "A");
        insert_row(&mut rec, 2, "B");
        rec.get(vec![Value::Integer(1)]).unwrap();
        let err = rec.rename(vec![(1, Value::Integer(2))]).unwrap_err();
        assert_eq!(err, RecordError::DuplicateKey);
    }

    #[test]
    fn test_setfilter_bc_expression() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "item");
        }
        rec.set_filter(1, ">=3").unwrap();
        assert_eq!(rec.count(), 3);
    }

    #[test]
    fn test_find_first_and_last() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "x");
        }
        assert!(rec.find_first().unwrap());
        assert_eq!(rec.field_get(1), Some(&Value::Integer(1)));

        assert!(rec.find_last().unwrap());
        assert_eq!(rec.field_get(1), Some(&Value::Integer(5)));
    }

    #[test]
    fn test_find_directions() {
        let mut rec = make_table();
        for i in [10i64, 20, 30] {
            insert_row(&mut rec, i, "x");
        }
        assert!(rec.find('-').unwrap());
        assert_eq!(rec.field_get(1), Some(&Value::Integer(10)));

        assert!(rec.find('+').unwrap());
        assert_eq!(rec.field_get(1), Some(&Value::Integer(30)));
    }

    #[test]
    fn test_composite_primary_key() {
        let mut rec = MockRecord::new(37, "Sales Line", vec![1, 2]);
        rec.field_set(1, Value::Text("Order".to_string()));
        rec.field_set(2, Value::Integer(1000));
        rec.field_set(3, Value::Integer(10)); // line amount
        rec.insert(false).unwrap();

        let key = vec![Value::Text("Order".to_string()), Value::Integer(1000)];
        rec.get(key).unwrap();
        assert_eq!(rec.field_get(3), Some(&Value::Integer(10)));
    }

    #[test]
    fn test_is_empty_respects_filters() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(100), Value::Integer(200));
        assert!(rec.is_empty());
        rec.reset();
        assert!(!rec.is_empty());
    }

    #[test]
    fn test_reset_clears_filters() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(3), Value::Integer(3));
        assert_eq!(rec.count(), 1);
        rec.reset();
        assert_eq!(rec.count(), 5);
    }

    #[test]
    fn decimal_primary_key_equality_is_consistent() {
        let d = Value::Decimal(dec!(1.25));
        assert!(d == d, "Eq contract: a == a must hold for a Decimal value");
        assert_eq!(
            d.cmp(&Value::Decimal(dec!(1.25))),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn decimal_primary_key_round_trips() {
        let mut rec = MockRecord::new(99, "DecTable", vec![1]);
        rec.field_set(1, Value::Decimal(dec!(3.14)));
        rec.field_set(2, Value::Text("decrow".to_string()));
        rec.insert(false).expect("insert Decimal PK should succeed");

        rec.field_set(1, Value::Decimal(dec!(3.14)));
        rec.field_set(2, Value::Text("duplicate".to_string()));
        let err = rec.insert(false).unwrap_err();
        assert_eq!(
            err,
            RecordError::DuplicateKey,
            "Decimal PK must trigger DuplicateKey on second insert"
        );
    }

    #[test]
    fn rename_to_same_key_succeeds() {
        let mut rec = make_table();
        insert_row(&mut rec, 42, "SameKey");
        rec.get(vec![Value::Integer(42)]).unwrap();
        rec.rename(vec![(1, Value::Integer(42))]).unwrap();
        rec.get(vec![Value::Integer(42)])
            .expect("row must survive no-op rename to same key");
        assert_eq!(rec.field_get(2), Some(&Value::Text("SameKey".to_string())));
    }

    #[test]
    fn setfilter_overwrites_setrange_on_same_field() {
        let mut rec = make_table();
        for i in 1i64..=10 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(3), Value::Integer(7));
        rec.set_filter(1, ">=5").unwrap();
        let count = rec.count();
        assert_eq!(count, 6, "SetFilter after SetRange on same field must overwrite (last-write-wins), giving >=5 → 6 rows");
    }

    #[test]
    fn setrange_and_setfilter_compose_across_fields() {
        let mut rec = MockRecord::new(99, "Test", vec![1]);
        for i in 1i64..=10 {
            rec.field_set(1, Value::Integer(i));
            let label = if i % 2 == 0 { "A" } else { "B" };
            rec.field_set(2, Value::Text(label.to_string()));
            rec.insert(false).unwrap();
        }
        rec.set_range(1, Value::Integer(3), Value::Integer(7));
        rec.set_filter(2, "A").unwrap();
        assert_eq!(
            rec.count(),
            2,
            "Filters on different fields must be AND: rows 4 and 6"
        );
    }

    #[test]
    fn modify_after_init_preserves_key() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "Row");
        rec.init(); // resets non-key fields, PRESERVES the key
        assert_eq!(
            rec.field_get(1),
            Some(&Value::Integer(1)),
            "Init must preserve the primary-key field"
        );
        rec.modify(false)
            .expect("Modify after Init should target the preserved key's row");
    }

    #[test]
    fn test_modify_with_no_key_set() {
        let mut rec = make_table();
        let err = rec.modify(false).unwrap_err();
        assert!(
            matches!(err, RecordError::MissingKeyField(_)),
            "Modify with no key set should return MissingKeyField, got: {err:?}"
        );
    }

    #[test]
    fn init_after_key_supports_insert_idiom() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(42));
        rec.init();
        rec.insert(false)
            .expect("Insert after Init-with-key must succeed");
        rec.get(vec![Value::Integer(42)]).expect("row 42 exists");
    }

    #[test]
    fn next_clamps_and_reports_actual_steps() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "r");
        }
        rec.find_set().unwrap(); // position at first (pos 0)
        assert_eq!(
            rec.next(10).unwrap(),
            4,
            "Next overshoot reports actual steps"
        );
        assert_eq!(
            rec.field_get(1),
            Some(&Value::Integer(5)),
            "Next overshoot lands on the last row"
        );
        assert_eq!(rec.next(1).unwrap(), 0);
    }

    #[test]
    fn modify_after_reset_retains_current_record() {
        let mut rec = make_table();
        insert_row(&mut rec, 10, "Ten");
        rec.get(vec![Value::Integer(10)]).unwrap();
        rec.reset();
        rec.field_set(2, Value::Text("TenModified".to_string()));
        rec.modify(false)
            .expect("Modify after reset should succeed when buffer has valid key");
        rec.get(vec![Value::Integer(10)]).unwrap();
        assert_eq!(
            rec.field_get(2),
            Some(&Value::Text("TenModified".to_string()))
        );
    }

    #[test]
    fn findset_allows_modify_during_iteration() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "original");
        }
        assert!(rec.find_set().unwrap());
        let mut visited = Vec::new();
        loop {
            let key = rec.field_get(1).unwrap().clone();
            visited.push(key.clone());
            rec.field_set(2, Value::Text("modified".to_string()));
            rec.modify(false)
                .expect("modify during iteration must not fail");
            if rec.next(1).unwrap() == 0 {
                break;
            }
        }
        assert_eq!(
            visited.len(),
            5,
            "All 5 rows must be visited during iteration with mid-loop Modify"
        );
        for i in 1i64..=5 {
            rec.get(vec![Value::Integer(i)]).unwrap();
            assert_eq!(
                rec.field_get(2),
                Some(&Value::Text("modified".to_string())),
                "Row {i} should have been modified"
            );
        }
    }

    #[test]
    fn count_and_isempty_reflect_delete_all() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "item");
        }
        assert_eq!(rec.count(), 5);
        assert!(!rec.is_empty());

        for i in 1i64..=5 {
            rec.field_set(1, Value::Integer(i));
            rec.delete(false).unwrap();
        }
        assert_eq!(rec.count(), 0, "Count must be 0 after deleting all rows");
        assert!(
            rec.is_empty(),
            "IsEmpty must be true after deleting all rows"
        );
        assert!(
            !rec.find_set().unwrap(),
            "FindSet on empty table must return false"
        );
    }

    #[test]
    fn xrec_matches_current_after_insert() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(99));
        rec.field_set(2, Value::Text("NewRow".to_string()));
        rec.insert(false).unwrap();
        assert_eq!(
            rec.x_rec_field(2),
            Some(&Value::Text("NewRow".to_string())),
            "xRec after Insert must reflect the inserted row (BC behaviour); got: {:?}",
            rec.x_rec_field(2)
        );
    }

    #[test]
    fn set_current_key_rebuilds_iteration_order() {
        let mut rec = MockRecord::new(99, "SortTest", vec![1]);
        for (no, priority) in [(10i64, 30i64), (20, 10), (30, 20)] {
            rec.field_set(1, Value::Integer(no));
            rec.field_set(3, Value::Integer(priority));
            rec.insert(false).unwrap();
        }

        assert!(rec.find_set().unwrap());
        let mut first_pass = vec![rec.field_get(1).unwrap().clone()];
        while rec.next(1).unwrap() != 0 {
            first_pass.push(rec.field_get(1).unwrap().clone());
        }
        assert_eq!(
            first_pass,
            vec![Value::Integer(10), Value::Integer(20), Value::Integer(30)]
        );

        rec.set_current_key(vec![3]);

        assert!(rec.find_set().unwrap());
        let mut second_pass = vec![rec.field_get(1).unwrap().clone()];
        while rec.next(1).unwrap() != 0 {
            second_pass.push(rec.field_get(1).unwrap().clone());
        }
        assert_eq!(
            second_pass,
            vec![Value::Integer(20), Value::Integer(30), Value::Integer(10)],
            "After SetCurrentKey, iter_set must be rebuilt in new sort order"
        );
    }

    /// A "Detail" table: fields 1=Doc No.(Code), 2=Line No.(Integer),
    /// 3=Amount, 4=Type, populated with three rows for ORD1 and one for ORD2.
    fn detail_table() -> MockRecord {
        let mut rec = MockRecord::new(50121, "Detail", vec![1, 2]);
        let rows = [
            ("ORD1", 1i64, 10i64, "Item"),
            ("ORD1", 2, 20, "Service"),
            ("ORD1", 3, 30, "Item"),
            ("ORD2", 1, 999, "Item"),
        ];
        for (doc, line, amt, typ) in rows {
            rec.field_set(1, Value::Code(doc.to_string()));
            rec.field_set(2, Value::Integer(line));
            rec.field_set(3, Value::Integer(amt));
            rec.field_set(4, Value::Code(typ.to_string()));
            rec.insert(false).unwrap();
        }
        rec
    }

    #[test]
    fn calc_flow_sum_with_field_eq() {
        let rec = detail_table();
        let conds = [(1, FlowFilter::Eq(Value::Code("ORD1".into())))];
        assert_eq!(
            rec.calc_flow(&conds, Some(3), FlowAgg::Sum),
            Ok(Value::Integer(60))
        );
    }

    #[test]
    fn calc_flow_count_and_exist() {
        let rec = detail_table();
        let conds = [(1, FlowFilter::Eq(Value::Code("ORD1".into())))];
        assert_eq!(
            rec.calc_flow(&conds, None, FlowAgg::Count),
            Ok(Value::Integer(3))
        );
        assert_eq!(
            rec.calc_flow(&conds, None, FlowAgg::Exist),
            Ok(Value::Boolean(true))
        );
        let none = [(1, FlowFilter::Eq(Value::Code("ZZZ".into())))];
        assert_eq!(
            rec.calc_flow(&none, None, FlowAgg::Count),
            Ok(Value::Integer(0))
        );
        assert_eq!(
            rec.calc_flow(&none, None, FlowAgg::Exist),
            Ok(Value::Boolean(false))
        );
    }

    #[test]
    fn calc_flow_const_filter_and_filter_expr() {
        let rec = detail_table();
        let const_conds = [
            (1, FlowFilter::Eq(Value::Code("ORD1".into()))),
            (4, FlowFilter::Eq(Value::Text("Item".into()))),
        ];
        assert_eq!(
            rec.calc_flow(&const_conds, Some(3), FlowAgg::Sum),
            Ok(Value::Integer(40))
        );

        let expr = filter::parse(">15").unwrap();
        let filter_conds = [
            (1, FlowFilter::Eq(Value::Code("ORD1".into()))),
            (3, FlowFilter::Expr(expr)),
        ];
        assert_eq!(
            rec.calc_flow(&filter_conds, Some(3), FlowAgg::Sum),
            Ok(Value::Integer(50))
        );
    }

    #[test]
    fn calc_flow_min_max_average() {
        let rec = detail_table();
        let conds = [(1, FlowFilter::Eq(Value::Code("ORD1".into())))];
        assert_eq!(
            rec.calc_flow(&conds, Some(3), FlowAgg::Min),
            Ok(Value::Integer(10))
        );
        assert_eq!(
            rec.calc_flow(&conds, Some(3), FlowAgg::Max),
            Ok(Value::Integer(30))
        );
        assert_eq!(
            rec.calc_flow(&conds, Some(3), FlowAgg::Average),
            Ok(Value::Decimal(dec!(20.0)))
        );
    }

    #[test]
    fn calc_flow_empty_defaults() {
        let rec = detail_table();
        let none = [(1, FlowFilter::Eq(Value::Code("ZZZ".into())))];
        assert_eq!(
            rec.calc_flow(&none, Some(3), FlowAgg::Sum),
            Ok(Value::Integer(0))
        );
        assert_eq!(
            rec.calc_flow(&none, Some(3), FlowAgg::Min),
            Ok(Value::Integer(0))
        );
        assert_eq!(
            rec.calc_flow(&none, Some(3), FlowAgg::Max),
            Ok(Value::Integer(0))
        );
        assert_eq!(
            rec.calc_flow(&none, Some(3), FlowAgg::Average),
            Ok(Value::Decimal(dec!(0.0)))
        );
        assert_eq!(
            rec.calc_flow(&none, Some(3), FlowAgg::Lookup),
            Ok(Value::Empty)
        );
    }

    #[test]
    fn calc_flow_sum_decimal_promotes() {
        let mut rec = MockRecord::new(1, "Dec", vec![1]);
        rec.field_set(1, Value::Integer(1));
        rec.field_set(2, Value::Integer(10));
        rec.insert(false).unwrap();
        rec.field_set(1, Value::Integer(2));
        rec.field_set(2, Value::Decimal(dec!(2.5)));
        rec.insert(false).unwrap();
        assert_eq!(
            rec.calc_flow(&[], Some(2), FlowAgg::Sum),
            Ok(Value::Decimal(dec!(12.5)))
        );
    }

    #[test]
    fn calc_flow_integer_overflow_is_an_error() {
        let mut rec = MockRecord::new(1, "Overflow", vec![1]);
        rec.field_set(1, Value::Integer(1));
        rec.field_set(2, Value::Integer(i64::MAX));
        rec.insert(false).unwrap();
        rec.field_set(1, Value::Integer(2));
        rec.field_set(2, Value::Integer(1));
        rec.insert(false).unwrap();

        assert_eq!(
            rec.calc_flow(&[], Some(2), FlowAgg::Sum),
            Err(RecordError::FlowArithmeticOverflow("Sum"))
        );
    }

    #[test]
    fn requested_trigger_execution_is_rejected_before_mutation() {
        let mut rec = MockRecord::new(1, "Triggered", vec![1]);
        rec.field_set(1, Value::Integer(1));
        assert_eq!(
            rec.insert(true),
            Err(RecordError::TriggerExecutionUnsupported("Insert"))
        );
        assert_eq!(rec.count(), 0);

        rec.insert(false).unwrap();
        rec.field_set(2, Value::Text("changed".into()));
        assert_eq!(
            rec.modify(true),
            Err(RecordError::TriggerExecutionUnsupported("Modify"))
        );
        assert_eq!(
            rec.delete(true),
            Err(RecordError::TriggerExecutionUnsupported("Delete"))
        );
        assert_eq!(rec.count(), 1);
    }

    #[test]
    fn invalid_find_direction_is_rejected() {
        let mut rec = detail_table();
        assert_eq!(rec.find('?'), Err(RecordError::InvalidFindDirection('?')));
    }

    #[test]
    fn delete_all_removes_filtered_rows_in_one_pass() {
        let mut rec = make_table();
        for i in 1i64..=10 {
            insert_row(&mut rec, i, "x");
        }
        rec.set_range(1, Value::Integer(1), Value::Integer(4));
        assert_eq!(rec.delete_all(false), Ok(4));
        rec.reset();
        assert_eq!(rec.count(), 6, "only the filtered rows may be deleted");
        assert_eq!(
            rec.delete_all(true),
            Err(RecordError::TriggerExecutionUnsupported("Delete")),
            "trigger execution stays a live-BC capability"
        );
    }

    #[test]
    fn separate_views_have_independent_filters_and_cursors() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "x");
        }
        let mut a = rec.new_view();
        let mut b = rec.new_view();
        rec.set_range_in(&mut a, 1, Value::Integer(1), Value::Integer(2));
        rec.set_range_in(&mut b, 1, Value::Integer(4), Value::Integer(5));
        assert_eq!(rec.count_in(&a), 2);
        assert_eq!(rec.count_in(&b), 2);
        assert!(rec.find_first_in(&mut a).unwrap());
        assert!(rec.find_first_in(&mut b).unwrap());
        assert_eq!(rec.field_get_in(&a, 1), Some(&Value::Integer(1)));
        assert_eq!(rec.field_get_in(&b, 1), Some(&Value::Integer(4)));
        // Advancing B must not disturb A's cursor.
        assert_eq!(rec.next_in(&mut b, 1).unwrap(), 1);
        assert_eq!(rec.next_in(&mut a, 1).unwrap(), 1);
        assert_eq!(rec.field_get_in(&a, 1), Some(&Value::Integer(2)));
    }

    #[test]
    fn code_primary_key_is_caseless_and_numeric_class_unifies() {
        let mut rec = MockRecord::new(1, "CodePk", vec![1]);
        rec.field_set(1, Value::Code("abc".into()));
        rec.insert(false).unwrap();
        rec.get(vec![Value::Code("ABC".into())])
            .expect("Get('ABC') must find the row stored under 'abc'");

        let mut num = MockRecord::new(2, "NumPk", vec![1]);
        num.field_set(1, Value::Decimal(dec!(5)));
        num.insert(false).unwrap();
        num.get(vec![Value::Integer(5)])
            .expect("an Integer key value must match a stored Decimal one");
    }

    #[test]
    fn unset_cell_matches_typed_zero_filters() {
        let mut rec = MockRecord::new(3, "Sparse", vec![1]);
        rec.field_set(1, Value::Integer(1));
        // Field 2 (Qty) intentionally never set.
        rec.insert(false).unwrap();
        rec.set_range(2, Value::Integer(0), Value::Integer(0));
        assert_eq!(
            rec.count(),
            1,
            "SetRange(Qty, 0) must match a row whose Qty was never assigned"
        );
        rec.reset();
        rec.set_range(2, Value::Integer(1), Value::Integer(9));
        assert_eq!(rec.count(), 0);
    }
}
