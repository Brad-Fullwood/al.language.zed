//! A stateful property test for `MockRecord` against a `BTreeMap` model.
//!
//! The table has a one-field `Code` primary key and one `Integer` data field. The model
//! is a `BTreeMap<String, i64>` keyed by the uppercased code, which is how
//! `normalize_key_value` treats a `Code` key. After every operation the test compares
//! `Count`, `IsEmpty` and the full ordered key list read back through
//! `FindSet` + `Next`, so an operation that corrupts the table or the cursor shows up on
//! the next step rather than at the end.
//!
//! Case count follows `PROPTEST_CASES` (default 128).

use al_runtime::mock::record::{MockRecord, RecordError};
use al_runtime::Value;
use proptest::prelude::*;
use std::collections::BTreeMap;

const KEY: i32 = 1;
const DATA: i32 = 2;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        ..ProptestConfig::default()
    }
}

#[derive(Debug, Clone)]
enum Op {
    Insert(String, i64),
    Modify(String, i64),
    Delete(String),
    Get(String),
    DeleteAll,
    SetRange(String, String),
    SetFilter(String),
    ClearFilter,
    Reset,
}

fn code() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "A", "B", "C", "a", "b", "AA", "AB", "Z", "", "A B", "É", "é", "ß",
    ])
    .prop_map(String::from)
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (code(), -50i64..50).prop_map(|(k, v)| Op::Insert(k, v)),
        2 => (code(), -50i64..50).prop_map(|(k, v)| Op::Modify(k, v)),
        2 => code().prop_map(Op::Delete),
        2 => code().prop_map(Op::Get),
        1 => Just(Op::DeleteAll),
        2 => (code(), code()).prop_map(|(a, b)| Op::SetRange(a, b)),
        1 => prop::sample::select(vec!["A*", "<>B", "A|B", "<C", ">=A", "@a"])
            .prop_map(|s| Op::SetFilter(s.to_string())),
        1 => Just(Op::ClearFilter),
        1 => Just(Op::Reset),
    ]
}

/// Filters the model understands, mirroring the filter forms the generator emits.
#[derive(Debug, Clone)]
enum ModelFilter {
    None,
    Range(String, String),
    Expr(String),
}

impl ModelFilter {
    /// A `Code` cell is caseless, so the model compares uppercased keys throughout.
    fn accepts(&self, key: &str) -> bool {
        match self {
            ModelFilter::None => true,
            ModelFilter::Range(lo, hi) => key >= lo.as_str() && key <= hi.as_str(),
            ModelFilter::Expr(e) => model_filter_expr(e, key),
        }
    }
}

fn model_filter_expr(expr: &str, key: &str) -> bool {
    match expr {
        "A*" => key.starts_with('A'),
        "<>B" => key != "B",
        "A|B" => key == "A" || key == "B",
        "<C" => key < "C",
        ">=A" => key >= "A",
        "@a" => key == "A",
        other => panic!("model does not know the filter {other}"),
    }
}

/// Keyed by the uppercased code, because that is what `normalize_key_value` indexes and
/// sorts by. The value keeps the code as it was written, because the row buffer stores
/// the assigned text and `field_get` hands it back unchanged.
struct Model {
    rows: BTreeMap<String, (String, i64)>,
    filter: ModelFilter,
}

impl Model {
    /// The codes a `FindSet` loop reads back, in table order, with their stored casing.
    fn visible(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|(k, _)| self.filter.accepts(k))
            .map(|(_, (original, _))| original.clone())
            .collect()
    }

    fn visible_keys(&self) -> Vec<String> {
        self.rows
            .keys()
            .filter(|k| self.filter.accepts(k))
            .cloned()
            .collect()
    }
}

/// Read the visible keys back out of the table with the canonical BC loop.
fn iterate(table: &mut MockRecord) -> Vec<String> {
    let mut seen = Vec::new();
    if !table.find_set().unwrap_or(false) {
        return seen;
    }
    loop {
        match table.field_get(KEY) {
            Some(Value::Code(c)) => seen.push(c.clone()),
            other => panic!("key field is {other:?}"),
        }
        match table.next(1) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => panic!("Next failed: {e}"),
        }
        if seen.len() > 1000 {
            panic!("Next never reported 0");
        }
    }
    seen
}

fn apply(table: &mut MockRecord, model: &mut Model, o: &Op) -> Result<(), TestCaseError> {
    match o {
        Op::Insert(k, v) => {
            table.init();
            table.field_set(KEY, Value::Code(k.clone()));
            table.field_set(DATA, Value::Integer(*v));
            let got = table.insert(false);
            let up = k.to_uppercase();
            match model.rows.entry(up) {
                std::collections::btree_map::Entry::Occupied(_) => prop_assert_eq!(
                    got,
                    Err(RecordError::DuplicateKey),
                    "insert of existing key {:?} must be a duplicate",
                    k
                ),
                std::collections::btree_map::Entry::Vacant(slot) => {
                    prop_assert_eq!(got, Ok(()), "insert of new key {:?} failed", k);
                    slot.insert((k.clone(), *v));
                }
            }
        }
        Op::Modify(k, v) => {
            table.init();
            table.field_set(KEY, Value::Code(k.clone()));
            table.field_set(DATA, Value::Integer(*v));
            let got = table.modify(false);
            let up = k.to_uppercase();
            if let Some(slot) = model.rows.get_mut(&up) {
                prop_assert_eq!(got, Ok(()), "modify of existing key {:?} failed", k);
                // Modify overwrites the whole row with the buffer, casing included.
                *slot = (k.clone(), *v);
            } else {
                prop_assert_eq!(
                    got,
                    Err(RecordError::NotFound),
                    "modify of missing key {:?} must not succeed",
                    k
                );
            }
        }
        Op::Delete(k) => {
            table.init();
            table.field_set(KEY, Value::Code(k.clone()));
            let got = table.delete(false);
            let up = k.to_uppercase();
            if model.rows.remove(&up).is_some() {
                prop_assert_eq!(got, Ok(()), "delete of existing key {:?} failed", k);
            } else {
                prop_assert_eq!(
                    got,
                    Err(RecordError::NotFound),
                    "delete of missing key {:?} must not succeed",
                    k
                );
            }
        }
        Op::Get(k) => {
            let got = table.get(vec![Value::Code(k.clone())]);
            let up = k.to_uppercase();
            match model.rows.get(&up) {
                Some((original, v)) => {
                    prop_assert_eq!(got, Ok(()), "get of existing key {:?} failed", k);
                    prop_assert_eq!(
                        table.field_get(DATA),
                        Some(&Value::Integer(*v)),
                        "get of {:?} loaded the wrong data field",
                        k
                    );
                    prop_assert_eq!(
                        table.field_get(KEY),
                        Some(&Value::Code(original.clone())),
                        "get of {:?} loaded the wrong key casing",
                        k
                    );
                }
                None => prop_assert_eq!(
                    got,
                    Err(RecordError::NotFound),
                    "get of missing key {:?} must not succeed",
                    k
                ),
            }
        }
        Op::DeleteAll => {
            let doomed = model.visible_keys();
            let got = table.delete_all(false);
            prop_assert_eq!(got, Ok(doomed.len()), "DeleteAll count");
            for k in doomed {
                model.rows.remove(&k);
            }
        }
        Op::SetRange(lo, hi) => {
            table.set_range(KEY, Value::Code(lo.clone()), Value::Code(hi.clone()));
            model.filter = ModelFilter::Range(lo.to_uppercase(), hi.to_uppercase());
        }
        Op::SetFilter(expr) => {
            table
                .set_filter(KEY, expr)
                .map_err(|e| TestCaseError::fail(format!("SetFilter({expr}) failed: {e}")))?;
            model.filter = ModelFilter::Expr(expr.clone());
        }
        Op::ClearFilter => {
            table.clear_filter(KEY);
            model.filter = ModelFilter::None;
        }
        Op::Reset => {
            table.reset();
            model.filter = ModelFilter::None;
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn record_operations_match_a_btreemap_model(ops in prop::collection::vec(op(), 1..30)) {
        let mut table = MockRecord::new(50100, "Prop", vec![KEY]);
        let mut model = Model {
            rows: BTreeMap::new(),
            filter: ModelFilter::None,
        };

        for (i, o) in ops.iter().enumerate() {
            apply(&mut table, &mut model, o)?;

            let expected = model.visible();
            prop_assert_eq!(
                table.count(),
                expected.len(),
                "Count disagreed after step {} ({:?})",
                i, o
            );
            prop_assert_eq!(
                table.is_empty(),
                expected.is_empty(),
                "IsEmpty disagreed after step {} ({:?})",
                i, o
            );
            prop_assert_eq!(
                iterate(&mut table),
                expected,
                "FindSet/Next disagreed after step {} ({:?})",
                i, o
            );
        }
    }

    /// The canonical BC delete loop removes exactly the filtered rows and terminates.
    #[test]
    fn find_set_delete_loop_removes_the_filtered_rows(
        keys in prop::collection::vec(code(), 1..12),
        lo in code(),
        hi in code(),
    ) {
        let mut table = MockRecord::new(50100, "Prop", vec![KEY]);
        let mut present: BTreeMap<String, String> = BTreeMap::new();
        for k in &keys {
            table.init();
            table.field_set(KEY, Value::Code(k.clone()));
            if table.insert(false).is_ok() {
                present.insert(k.to_uppercase(), k.clone());
            }
        }
        let (lo_u, hi_u) = (lo.to_uppercase(), hi.to_uppercase());
        table.set_range(KEY, Value::Code(lo.clone()), Value::Code(hi.clone()));

        let doomed: Vec<String> = present
            .keys()
            .filter(|k| k.as_str() >= lo_u.as_str() && k.as_str() <= hi_u.as_str())
            .cloned()
            .collect();

        let mut deleted = 0usize;
        if table.find_set().unwrap() {
            loop {
                table.delete(false).unwrap();
                deleted += 1;
                prop_assert!(deleted <= doomed.len(), "loop deleted more rows than matched");
                if table.next(1).unwrap() == 0 {
                    break;
                }
            }
        }
        prop_assert_eq!(deleted, doomed.len(), "delete loop removed the wrong count");

        table.reset();
        let survivors: Vec<String> = iterate(&mut table);
        let expected: Vec<String> = present
            .iter()
            .filter(|(k, _)| !doomed.contains(k))
            .map(|(_, original)| original.clone())
            .collect();
        prop_assert_eq!(survivors, expected);
    }

    /// `Next` reports the steps it actually took, and stepping back returns to the
    /// same row.
    #[test]
    fn next_is_reversible_within_the_set(
        keys in prop::collection::vec(code(), 1..10),
        steps in 1i32..6,
    ) {
        let mut table = MockRecord::new(50100, "Prop", vec![KEY]);
        for k in &keys {
            table.init();
            table.field_set(KEY, Value::Code(k.clone()));
            let _ = table.insert(false);
        }
        if !table.find_set().unwrap() {
            return Ok(());
        }
        let first = table.field_get(KEY).cloned();
        let moved = table.next(steps).unwrap();
        let back = table.next(-moved).unwrap();
        prop_assert_eq!(back, -moved, "stepping back did not undo the move");
        prop_assert_eq!(table.field_get(KEY).cloned(), first, "did not return to the first row");
    }
}
