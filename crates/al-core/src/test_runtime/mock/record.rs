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

use crate::test_runtime::interpreter::value::Value;
use crate::test_runtime::mock::filter::{self, FilterExpr};

// ──────────────────────────────────────────────────────────────────────────────
// Type aliases
// ──────────────────────────────────────────────────────────────────────────────

/// A field number, matching BC's integer field-number convention.
pub type FieldNo = i32;

/// A composite primary key is an ordered list of field values.
pub type PrimaryKey = Vec<Value>;

/// A single row: field number → value.
pub type Row = BTreeMap<FieldNo, Value>;

// ──────────────────────────────────────────────────────────────────────────────
// Error type
// ──────────────────────────────────────────────────────────────────────────────

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
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-field filter spec
// ──────────────────────────────────────────────────────────────────────────────

/// A filter applied to a specific field.
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
            FieldFilter::Range(lo, hi) => value >= lo && value <= hi,
            FieldFilter::Expr(expr) => filter::matches(expr, value),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Sort key for iteration
// ──────────────────────────────────────────────────────────────────────────────

/// The current-key fields that determine iteration order.
/// Defaults to the primary key fields.
#[derive(Debug, Clone)]
struct SortKey {
    /// Field numbers, in priority order.
    fields: Vec<FieldNo>,
}

impl SortKey {
    fn from_fields(fields: Vec<FieldNo>) -> Self {
        SortKey { fields }
    }

    /// Extract the sort-key tuple from a row.
    fn key_of(&self, row: &Row) -> Vec<Value> {
        self.fields
            .iter()
            .map(|f| row.get(f).cloned().unwrap_or(Value::Empty))
            .collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// MockRecord
// ──────────────────────────────────────────────────────────────────────────────

/// An in-memory BC record table.
///
/// Supports the standard BC Record API: Init, Get, Insert, Modify, Delete,
/// Rename, FindFirst/FindLast/FindSet/Find, Next, SetRange, SetFilter,
/// IsEmpty, Count.
#[derive(Debug, Clone)]
pub struct MockRecord {
    /// The AL table ID.
    pub table_id: i32,
    /// The AL table name.
    pub table_name: String,
    /// The field numbers that form the primary key (in order).
    primary_key_fields: Vec<FieldNo>,
    /// The stored rows, keyed by primary key.
    rows: BTreeMap<PrimaryKey, Row>,
    /// The current row's field values (the "buffer").
    current: Row,
    /// Snapshot of `current` before the last Modify/Rename (xRec).
    x_rec: Row,
    /// Active per-field filters.
    filters: BTreeMap<FieldNo, FieldFilter>,
    /// Current sort/key order for iteration.
    sort_key: SortKey,
    /// Filtered, sorted keys ready for iteration (built by FindFirst/FindSet).
    iter_set: Vec<PrimaryKey>,
    /// Index of the current position in `iter_set`.
    iter_pos: Option<usize>,
}

impl MockRecord {
    /// Create a new, empty MockRecord for the given table.
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
            current: BTreeMap::new(),
            x_rec: BTreeMap::new(),
            filters: BTreeMap::new(),
            sort_key,
            iter_set: Vec::new(),
            iter_pos: None,
        }
    }

    // ── Buffer / current-record helpers ─────────────────────────────────────

    /// Set a field value in the current buffer.
    pub fn field_set(&mut self, field: FieldNo, value: Value) {
        self.current.insert(field, value);
    }

    /// Get a field value from the current buffer.
    pub fn field_get(&self, field: FieldNo) -> Option<&Value> {
        self.current.get(&field)
    }

    /// Extract the primary key from the current buffer.
    fn current_primary_key(&self) -> Result<PrimaryKey, RecordError> {
        self.primary_key_fields
            .iter()
            .map(|&f| {
                self.current
                    .get(&f)
                    .cloned()
                    .ok_or(RecordError::MissingKeyField(f))
            })
            .collect()
    }

    // ── AL Record API ─────────────────────────────────────────────────────────

    /// `INIT` — reset the current buffer to empty defaults.
    pub fn init(&mut self) {
        self.current.clear();
        self.x_rec.clear();
        self.iter_pos = None;
    }

    /// `RESET` — clear all filters and the sort key; reset to primary key order.
    pub fn reset(&mut self) {
        self.filters.clear();
        self.sort_key = SortKey::from_fields(self.primary_key_fields.clone());
        self.iter_set.clear();
        self.iter_pos = None;
    }

    /// `GET(key_parts…)` — look up a row by primary key; load into buffer.
    pub fn get(&mut self, key: PrimaryKey) -> Result<(), RecordError> {
        let row = self.rows.get(&key).ok_or(RecordError::NotFound)?;
        self.current = row.clone();
        self.x_rec = row.clone();
        Ok(())
    }

    /// `INSERT` — insert the current buffer as a new row.
    ///
    /// `run_trigger` is accepted for API parity; triggers are no-ops in the mock.
    pub fn insert(&mut self, _run_trigger: bool) -> Result<(), RecordError> {
        let key = self.current_primary_key()?;
        if self.rows.contains_key(&key) {
            return Err(RecordError::DuplicateKey);
        }
        self.rows.insert(key, self.current.clone());
        // BC behaviour: after Insert, xRec mirrors the inserted row (Rec).
        self.x_rec = self.current.clone();
        Ok(())
    }

    /// `MODIFY` — overwrite the existing row with the current buffer.
    ///
    /// Saves the prior row as `xRec`.
    pub fn modify(&mut self, _run_trigger: bool) -> Result<(), RecordError> {
        let key = self.current_primary_key()?;
        let old_row = self.rows.get_mut(&key).ok_or(RecordError::NotFound)?;
        self.x_rec = old_row.clone();
        *old_row = self.current.clone();
        Ok(())
    }

    /// `DELETE` — remove the row matching the current buffer's primary key.
    pub fn delete(&mut self, _run_trigger: bool) -> Result<(), RecordError> {
        let key = self.current_primary_key()?;
        self.rows.remove(&key).ok_or(RecordError::NotFound)?;
        self.iter_pos = None;
        Ok(())
    }

    /// `RENAME(new_key)` — move the current row to a new primary key.
    ///
    /// The new key values must be provided as a `Vec<(FieldNo, Value)>` that
    /// covers all primary key fields. Saves the old row as `xRec`.
    pub fn rename(&mut self, new_key_values: Vec<(FieldNo, Value)>) -> Result<(), RecordError> {
        let old_key = self.current_primary_key()?;
        let old_row = self.rows.remove(&old_key).ok_or(RecordError::NotFound)?;
        self.x_rec = old_row.clone();
        // Apply new key fields to the current buffer.
        let mut new_row = old_row;
        for (field, value) in new_key_values {
            new_row.insert(field, value.clone());
            self.current.insert(field, value);
        }
        let new_key = self.current_primary_key()?;
        if self.rows.contains_key(&new_key) {
            // Restore old row on conflict.
            self.rows.insert(old_key, self.x_rec.clone());
            return Err(RecordError::DuplicateKey);
        }
        self.rows.insert(new_key, new_row);
        Ok(())
    }

    /// `SETCURRENTKEY(fields…)` — change iteration sort order.
    pub fn set_current_key(&mut self, fields: Vec<FieldNo>) {
        self.sort_key = SortKey::from_fields(fields);
        self.iter_set.clear();
        self.iter_pos = None;
    }

    /// `SETRANGE(field, low, high)` — filter a field to an inclusive value range.
    pub fn set_range(&mut self, field: FieldNo, low: Value, high: Value) {
        self.filters.insert(field, FieldFilter::Range(low, high));
        self.iter_set.clear();
        self.iter_pos = None;
    }

    /// `SETFILTER(field, expr)` — set a BC filter expression on a field.
    pub fn set_filter(&mut self, field: FieldNo, expr: &str) -> Result<(), RecordError> {
        let parsed =
            filter::parse(expr).map_err(|e| RecordError::FilterParse(field, e.to_string()))?;
        self.filters.insert(field, FieldFilter::Expr(parsed));
        self.iter_set.clear();
        self.iter_pos = None;
        Ok(())
    }

    // ── Internal: build filtered, sorted iteration set ────────────────────────

    fn build_iter_set(&mut self) {
        let mut keys: Vec<PrimaryKey> = self
            .rows
            .iter()
            .filter_map(|(key, row)| {
                if self.row_matches_filters(row) {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();

        // Sort by current-key fields.
        let sort_key = self.sort_key.clone();
        keys.sort_by(|a, b| {
            let row_a = self.rows.get(a).unwrap();
            let row_b = self.rows.get(b).unwrap();
            sort_key.key_of(row_a).cmp(&sort_key.key_of(row_b))
        });

        self.iter_set = keys;
    }

    fn row_matches_filters(&self, row: &Row) -> bool {
        for (&field, filter) in &self.filters {
            let value = row.get(&field).unwrap_or(&Value::Empty);
            if !filter.matches(value) {
                return false;
            }
        }
        true
    }

    fn load_row_at(&mut self, pos: usize) -> Result<(), RecordError> {
        let key = self.iter_set.get(pos).ok_or(RecordError::EndOfSet)?.clone();
        let row = self.rows.get(&key).ok_or(RecordError::NotFound)?;
        self.current = row.clone();
        self.x_rec = row.clone();
        self.iter_pos = Some(pos);
        Ok(())
    }

    // ── Iteration ─────────────────────────────────────────────────────────────

    /// `FINDFIRST` — position on the first matching record.
    pub fn find_first(&mut self) -> Result<bool, RecordError> {
        self.build_iter_set();
        if self.iter_set.is_empty() {
            self.iter_pos = None;
            return Ok(false);
        }
        self.load_row_at(0)?;
        Ok(true)
    }

    /// `FINDLAST` — position on the last matching record.
    pub fn find_last(&mut self) -> Result<bool, RecordError> {
        self.build_iter_set();
        let last = self.iter_set.len().saturating_sub(1);
        if self.iter_set.is_empty() {
            self.iter_pos = None;
            return Ok(false);
        }
        self.load_row_at(last)?;
        Ok(true)
    }

    /// `FINDSET` — prepare iteration set and position at first record.
    ///
    /// Returns `false` if the set is empty (no rows match filters).
    pub fn find_set(&mut self) -> Result<bool, RecordError> {
        self.find_first()
    }

    /// `FIND('-')` / `FIND('+')` — position at first (−) or last (+) record.
    pub fn find(&mut self, direction: char) -> Result<bool, RecordError> {
        match direction {
            '-' => self.find_first(),
            '+' => self.find_last(),
            _ => Ok(false),
        }
    }

    /// `NEXT` — advance to the next (or previous) record.
    ///
    /// `steps` is typically 1 (forward) or -1 (backward), matching BC's
    /// `NEXT(steps)` signature.  Returns `Ok(steps_actually_moved)`.
    pub fn next(&mut self, steps: i32) -> Result<i32, RecordError> {
        let current_pos = self.iter_pos.ok_or(RecordError::NoCurrentRow)?;
        let new_pos = current_pos as i64 + steps as i64;
        if new_pos < 0 || new_pos >= self.iter_set.len() as i64 {
            return Ok(0);
        }
        let new_pos = new_pos as usize;
        self.load_row_at(new_pos)?;
        Ok(steps)
    }

    // ── Aggregates ────────────────────────────────────────────────────────────

    /// `ISEMPTY` — `true` if no rows match the current filters.
    pub fn is_empty(&self) -> bool {
        self.rows.values().all(|row| !self.row_matches_filters(row))
    }

    /// `COUNT` — number of rows matching the current filters.
    pub fn count(&self) -> usize {
        self.rows
            .values()
            .filter(|row| self.row_matches_filters(row))
            .count()
    }

    // ── xRec access ───────────────────────────────────────────────────────────

    /// Return the `xRec` snapshot (the row before the last Modify/Rename).
    pub fn x_rec(&self) -> &Row {
        &self.x_rec
    }

    /// Get a field from the xRec snapshot.
    pub fn x_rec_field(&self, field: FieldNo) -> Option<&Value> {
        self.x_rec.get(&field)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a simple single-field-PK table.
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

    // ── Positive: insert → get ────────────────────────────────────────────────

    #[test]
    fn test_insert_then_get_matches() {
        let mut rec = make_table();
        insert_row(&mut rec, 1000, "Bike");
        let mut rec2 = rec.clone();
        rec2.get(vec![Value::Integer(1000)]).unwrap();
        assert_eq!(rec2.field_get(2), Some(&Value::Text("Bike".to_string())));
    }

    // ── Positive: delete → isEmpty ────────────────────────────────────────────

    #[test]
    fn test_delete_then_is_empty() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "X");
        rec.field_set(1, Value::Integer(1));
        rec.delete(false).unwrap();
        assert!(rec.is_empty());
    }

    // ── Positive: modify → xRec preserved ────────────────────────────────────

    #[test]
    fn test_modify_xrec_preserved() {
        let mut rec = make_table();
        insert_row(&mut rec, 100, "OriginalName");

        // Load and modify.
        rec.get(vec![Value::Integer(100)]).unwrap();
        let old_desc = rec.field_get(2).cloned().unwrap();
        rec.field_set(2, Value::Text("NewName".to_string()));
        rec.modify(false).unwrap();

        // xRec should still hold the old value.
        assert_eq!(rec.x_rec_field(2), Some(&old_desc));
        // Current should hold the new value.
        assert_eq!(rec.field_get(2), Some(&Value::Text("NewName".to_string())));
    }

    // ── Positive: count after mass delete ────────────────────────────────────

    #[test]
    fn test_count_after_mass_delete_is_zero() {
        let mut rec = make_table();
        for i in 1..=10i64 {
            insert_row(&mut rec, i, "item");
        }
        assert_eq!(rec.count(), 10);

        // Delete all rows.
        for i in 1..=10i64 {
            rec.field_set(1, Value::Integer(i));
            rec.delete(false).unwrap();
        }
        assert_eq!(rec.count(), 0);
        assert!(rec.is_empty());
    }

    // ── Positive: FindSet iteration order matches sort key ────────────────────

    #[test]
    fn test_findset_iteration_order_matches_sort_key() {
        let mut rec = make_table();
        // Insert in reverse order.
        for i in [5i64, 3, 1, 4, 2] {
            insert_row(&mut rec, i, "x");
        }
        assert!(rec.find_set().unwrap());
        let mut order = vec![rec.field_get(1).unwrap().clone()];
        while rec.next(1).unwrap() != 0 {
            order.push(rec.field_get(1).unwrap().clone());
        }
        // Should be in ascending key order.
        let expected: Vec<Value> = (1i64..=5).map(Value::Integer).collect();
        assert_eq!(order, expected);
    }

    // ── Positive: SetRange filters before iteration ───────────────────────────

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

    // ── Positive: SetCurrentKey changes order ─────────────────────────────────

    #[test]
    fn test_set_current_key_changes_iteration_order() {
        // Two-field table: key = field 1, also field 2 = sort priority field.
        let mut rec = MockRecord::new(99, "Test", vec![1]);
        for (no, priority) in [(10i64, 30i64), (20, 10), (30, 20)] {
            rec.field_set(1, Value::Integer(no));
            rec.field_set(3, Value::Integer(priority));
            rec.insert(false).unwrap();
        }
        // Sort by field 3 (priority).
        rec.set_current_key(vec![3]);
        assert!(rec.find_set().unwrap());
        let mut seen_no = Vec::new();
        seen_no.push(rec.field_get(1).unwrap().clone());
        while rec.next(1).unwrap() != 0 {
            seen_no.push(rec.field_get(1).unwrap().clone());
        }
        // Field 3 values: 10→no=20, 20→no=30, 30→no=10.
        assert_eq!(
            seen_no,
            vec![Value::Integer(20), Value::Integer(30), Value::Integer(10)]
        );
    }

    // ── Positive: Rename ──────────────────────────────────────────────────────

    #[test]
    fn test_rename_moves_row() {
        let mut rec = make_table();
        insert_row(&mut rec, 100, "Alpha");
        rec.get(vec![Value::Integer(100)]).unwrap();
        rec.rename(vec![(1, Value::Integer(200))]).unwrap();

        // Old key gone.
        assert!(rec.get(vec![Value::Integer(100)]).is_err());
        // New key present.
        rec.get(vec![Value::Integer(200)]).unwrap();
        assert_eq!(rec.field_get(2), Some(&Value::Text("Alpha".to_string())));
    }

    // ── Negative: duplicate insert ────────────────────────────────────────────

    #[test]
    fn test_insert_duplicate_key_error() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "A");
        // Reset buffer to same key.
        rec.field_set(1, Value::Integer(1));
        rec.field_set(2, Value::Text("B".to_string()));
        let err = rec.insert(false).unwrap_err();
        assert_eq!(err, RecordError::DuplicateKey);
    }

    // ── Negative: get missing record ─────────────────────────────────────────

    #[test]
    fn test_get_missing_record_error() {
        let mut rec = make_table();
        let err = rec.get(vec![Value::Integer(999)]).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    // ── Negative: modify non-existent ────────────────────────────────────────

    #[test]
    fn test_modify_nonexistent_error() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(1));
        let err = rec.modify(false).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    // ── Negative: delete non-existent ────────────────────────────────────────

    #[test]
    fn test_delete_nonexistent_error() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(1));
        let err = rec.delete(false).unwrap_err();
        assert_eq!(err, RecordError::NotFound);
    }

    // ── Negative: Next without FindSet ───────────────────────────────────────

    #[test]
    fn test_next_without_findset_error() {
        let mut rec = make_table();
        let err = rec.next(1).unwrap_err();
        assert_eq!(err, RecordError::NoCurrentRow);
    }

    // ── Negative: FindSet on empty table ─────────────────────────────────────

    #[test]
    fn test_findset_empty_table_returns_false() {
        let mut rec = make_table();
        assert!(!rec.find_set().unwrap());
    }

    // ── Negative: bad SetFilter expression ───────────────────────────────────

    #[test]
    fn test_invalid_setfilter_returns_error() {
        let mut rec = make_table();
        // Empty expression is a parse error.
        assert!(rec.set_filter(1, "").is_err());
    }

    // ── Negative: rename to existing key ─────────────────────────────────────

    #[test]
    fn test_rename_to_existing_key_error() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "A");
        insert_row(&mut rec, 2, "B");
        rec.get(vec![Value::Integer(1)]).unwrap();
        let err = rec.rename(vec![(1, Value::Integer(2))]).unwrap_err();
        assert_eq!(err, RecordError::DuplicateKey);
    }

    // ── Positive: SetFilter with BC expression ────────────────────────────────

    #[test]
    fn test_setfilter_bc_expression() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "item");
        }
        // Keep only rows where field 1 >= 3.
        rec.set_filter(1, ">=3").unwrap();
        assert_eq!(rec.count(), 3);
    }

    // ── Positive: FindFirst / FindLast ───────────────────────────────────────

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

    // ── Positive: Find('-') and Find('+') ────────────────────────────────────

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

    // ── Positive: composite primary key ──────────────────────────────────────

    #[test]
    fn test_composite_primary_key() {
        // Fields 1 (doc type) + 2 (no.) form the composite PK.
        let mut rec = MockRecord::new(37, "Sales Line", vec![1, 2]);
        rec.field_set(1, Value::Text("Order".to_string()));
        rec.field_set(2, Value::Integer(1000));
        rec.field_set(3, Value::Integer(10)); // line amount
        rec.insert(false).unwrap();

        let key = vec![Value::Text("Order".to_string()), Value::Integer(1000)];
        rec.get(key).unwrap();
        assert_eq!(rec.field_get(3), Some(&Value::Integer(10)));
    }

    // ── Positive: IsEmpty respects filters ───────────────────────────────────

    #[test]
    fn test_is_empty_respects_filters() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "x");
        }
        // Filter that matches nothing.
        rec.set_range(1, Value::Integer(100), Value::Integer(200));
        assert!(rec.is_empty());
        // Filter that matches everything.
        rec.reset();
        assert!(!rec.is_empty());
    }

    // ── Positive: Reset clears filters ───────────────────────────────────────

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

    // ──────────────────────────────────────────────────────────────────────────
    // ADVERSARIAL-I tests
    // ──────────────────────────────────────────────────────────────────────────

    // Vector 1: NaN Decimal as primary key — BTreeMap uses Ord (total_cmp)
    // for lookup, so insert+get should round-trip. However, PartialEq is
    // derived (uses f64 ==) which returns false for NaN==NaN, violating the
    // Eq contract. This test exposes the Eq/PartialEq inconsistency: the
    // derived PartialEq says NaN != NaN, but Ord says they are Equal.
    #[test]
    fn test_nan_decimal_pk_eq_consistency_adversarial_i_1() {
        let nan = Value::Decimal(f64::NAN);
        // Ord/total_cmp says NaN == NaN — this should hold for Eq.
        // If the derived PartialEq is used, this assertion will FAIL
        // because f64 NaN != NaN.
        assert!(
            nan == nan,
            "Eq contract: a == a must hold for Value::Decimal(NaN)"
        );
    }

    // Vector 1b: NaN Decimal round-trips through BTreeMap insert/get.
    // BTreeMap uses Ord for all key operations, so even with broken PartialEq
    // the map should find the key. But contains_key on the extracted PrimaryKey
    // goes through Vec<Value> comparison which uses PartialEq — potential
    // inconsistency with BTreeMap's Ord-based lookup.
    #[test]
    fn test_nan_decimal_pk_roundtrip_adversarial_i_1b() {
        let mut rec = MockRecord::new(99, "NanTable", vec![1]);
        rec.field_set(1, Value::Decimal(f64::NAN));
        rec.field_set(2, Value::Text("nanrow".to_string()));
        rec.insert(false).expect("insert NaN PK should succeed");

        // Second insert with NaN PK should be a DuplicateKey error.
        rec.field_set(1, Value::Decimal(f64::NAN));
        rec.field_set(2, Value::Text("duplicate".to_string()));
        let err = rec.insert(false).unwrap_err();
        assert_eq!(
            err,
            RecordError::DuplicateKey,
            "NaN PK must trigger DuplicateKey on second insert"
        );
    }

    // Vector 3: Rename to self (same key) should succeed without row loss.
    // When old_key == new_key: we remove the row, then find no conflict
    // (since the row is now absent), and re-insert. Result: row survives.
    #[test]
    fn test_rename_to_same_key_adversarial_i_3() {
        let mut rec = make_table();
        insert_row(&mut rec, 42, "SameKey");
        rec.get(vec![Value::Integer(42)]).unwrap();
        // Rename to the identical key value.
        rec.rename(vec![(1, Value::Integer(42))]).unwrap();
        // Row must still be findable.
        rec.get(vec![Value::Integer(42)])
            .expect("row must survive no-op rename to same key");
        assert_eq!(rec.field_get(2), Some(&Value::Text("SameKey".to_string())));
    }

    // Vector 4: SetRange then SetFilter on the SAME field — second call
    // must overwrite the first (last-write-wins). Combined on DIFFERENT
    // fields must be AND semantics (both filters must match).
    #[test]
    fn test_setrange_then_setfilter_same_field_overwrite_adversarial_i_4() {
        let mut rec = make_table();
        for i in 1i64..=10 {
            insert_row(&mut rec, i, "x");
        }
        // SetRange to [3,7], then SetFilter >=5 on the same field.
        // Expected BC behaviour: SetFilter replaces SetRange → only >=5 applies.
        rec.set_range(1, Value::Integer(3), Value::Integer(7));
        rec.set_filter(1, ">=5").unwrap();
        // If last-write-wins: count == 6 (5..=10).
        // If AND semantics:    count == 3 (5..=7).
        // Document which one actually happens:
        let count = rec.count();
        assert_eq!(count, 6, "SetFilter after SetRange on same field must overwrite (last-write-wins), giving >=5 → 6 rows");
    }

    // Vector 4b: SetRange and SetFilter on DIFFERENT fields must be AND.
    #[test]
    fn test_setrange_and_setfilter_different_fields_and_semantics_adversarial_i_4b() {
        let mut rec = MockRecord::new(99, "Test", vec![1]);
        // Insert rows: field1 in 1..=10, field2 = "A" or "B" alternating.
        for i in 1i64..=10 {
            rec.field_set(1, Value::Integer(i));
            let label = if i % 2 == 0 { "A" } else { "B" };
            rec.field_set(2, Value::Text(label.to_string()));
            rec.insert(false).unwrap();
        }
        // SetRange on field1: [3,7] (rows 3,4,5,6,7).
        rec.set_range(1, Value::Integer(3), Value::Integer(7));
        // SetFilter on field2: only "A" (even rows: 4,6).
        rec.set_filter(2, "A").unwrap();
        // AND semantics: 5 rows in [3,7] AND field2="A" (case-insensitive).
        // Even rows in [3,7]: 4, 6 → count = 2.
        assert_eq!(
            rec.count(),
            2,
            "Filters on different fields must be AND: rows 4 and 6"
        );
    }

    // Vector 5: Modify after Reset with empty current buffer.
    // reset() clears iter_pos and filters but NOT the current buffer.
    // After init() (which does clear current), modify() has no key field
    // → should return MissingKeyField, not NotFound, not panic.
    #[test]
    fn test_modify_after_init_empty_buffer_adversarial_i_5() {
        let mut rec = make_table();
        insert_row(&mut rec, 1, "Row");
        rec.init(); // clears current buffer
                    // Now modify() — current buffer is empty, no PK field set.
        let err = rec.modify(false).unwrap_err();
        assert!(
            matches!(err, RecordError::MissingKeyField(_)),
            "Modify with empty buffer should return MissingKeyField, got: {err:?}"
        );
    }

    // Vector 5b: Modify after Reset (NOT init) — current buffer retains
    // the last loaded row's key. Modify should succeed if the row exists.
    #[test]
    fn test_modify_after_reset_retains_current_adversarial_i_5b() {
        let mut rec = make_table();
        insert_row(&mut rec, 10, "Ten");
        rec.get(vec![Value::Integer(10)]).unwrap();
        // reset() does NOT clear the current buffer.
        rec.reset();
        // Modify a non-PK field and call modify — should succeed.
        rec.field_set(2, Value::Text("TenModified".to_string()));
        rec.modify(false)
            .expect("Modify after reset should succeed when buffer has valid key");
        rec.get(vec![Value::Integer(10)]).unwrap();
        assert_eq!(
            rec.field_get(2),
            Some(&Value::Text("TenModified".to_string()))
        );
    }

    // Vector 6: FindSet then Modify mid-iteration — cursor (iter_set)
    // remains stable when a non-PK field is modified. Iteration must
    // continue to the next row without skipping or panicking.
    #[test]
    fn test_findset_modify_mid_iteration_adversarial_i_6() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "original");
        }
        assert!(rec.find_set().unwrap());
        let mut visited = Vec::new();
        loop {
            let key = rec.field_get(1).unwrap().clone();
            visited.push(key.clone());
            // Modify the current row's non-PK field.
            rec.field_set(2, Value::Text("modified".to_string()));
            rec.modify(false)
                .expect("modify during iteration must not fail");
            // Advance to next record.
            if rec.next(1).unwrap() == 0 {
                break;
            }
        }
        assert_eq!(
            visited.len(),
            5,
            "All 5 rows must be visited during iteration with mid-loop Modify"
        );
        // Verify modifications persisted.
        for i in 1i64..=5 {
            rec.get(vec![Value::Integer(i)]).unwrap();
            assert_eq!(
                rec.field_get(2),
                Some(&Value::Text("modified".to_string())),
                "Row {i} should have been modified"
            );
        }
    }

    // Vector 7: Count and IsEmpty after deleting all rows using FindSet+Delete.
    #[test]
    fn test_count_isempty_after_delete_all_adversarial_i_7() {
        let mut rec = make_table();
        for i in 1i64..=5 {
            insert_row(&mut rec, i, "item");
        }
        assert_eq!(rec.count(), 5);
        assert!(!rec.is_empty());

        // Delete all rows via direct key deletion.
        for i in 1i64..=5 {
            rec.field_set(1, Value::Integer(i));
            rec.delete(false).unwrap();
        }
        assert_eq!(rec.count(), 0, "Count must be 0 after deleting all rows");
        assert!(
            rec.is_empty(),
            "IsEmpty must be true after deleting all rows"
        );
        // FindSet on empty table must return false.
        assert!(
            !rec.find_set().unwrap(),
            "FindSet on empty table must return false"
        );
    }

    // Vector 8: xRec after Insert on a fresh record (no prior Get).
    // In BC, after Insert, xRec should equal the inserted record.
    // In the mock, insert() does NOT update x_rec — it stays empty.
    // This test documents the deviation: x_rec is empty after a first Insert.
    #[test]
    fn test_xrec_after_insert_equals_current_adversarial_i_8() {
        let mut rec = make_table();
        rec.field_set(1, Value::Integer(99));
        rec.field_set(2, Value::Text("NewRow".to_string()));
        rec.insert(false).unwrap();
        // BC behaviour: xRec should match the inserted record.
        // Mock behaviour: xRec was not updated by insert() — it is empty.
        assert_eq!(
            rec.x_rec_field(2),
            Some(&Value::Text("NewRow".to_string())),
            "xRec after Insert must reflect the inserted row (BC behaviour); got: {:?}",
            rec.x_rec_field(2)
        );
    }

    // Vector 19: SetCurrentKey changes sort key and the NEXT FindSet
    // rebuilds iter_set in the new order (not stale from old FindSet).
    #[test]
    fn test_set_current_key_iter_set_rebuilds_adversarial_i_19() {
        // Two-field rows: field1 = PK, field3 = secondary sort.
        let mut rec = MockRecord::new(99, "SortTest", vec![1]);
        for (no, priority) in [(10i64, 30i64), (20, 10), (30, 20)] {
            rec.field_set(1, Value::Integer(no));
            rec.field_set(3, Value::Integer(priority));
            rec.insert(false).unwrap();
        }

        // First FindSet in PK order: 10, 20, 30.
        assert!(rec.find_set().unwrap());
        let mut first_pass = vec![rec.field_get(1).unwrap().clone()];
        while rec.next(1).unwrap() != 0 {
            first_pass.push(rec.field_get(1).unwrap().clone());
        }
        assert_eq!(
            first_pass,
            vec![Value::Integer(10), Value::Integer(20), Value::Integer(30)]
        );

        // Change sort key to field3 (priority).
        rec.set_current_key(vec![3]);

        // Second FindSet must use new sort key: priority 10→no=20, 20→no=30, 30→no=10.
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
}
