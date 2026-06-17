//! Native Rust ports of BC test libraries — Phase 2.
//!
//! Each library lives in its own submodule and exposes a `resolve` fn
//! that maps a procedure name to its implementation. The dispatch layer
//! consults these tables when the call receiver is the corresponding
//! library codeunit.

pub mod any;
pub mod library_assert;
pub mod library_random;
pub mod library_variable_storage;

use crate::test_runtime::interpreter::scope::Eval;
use crate::test_runtime::interpreter::value::Value;

pub type StubFn = fn(&[Value]) -> Eval;
pub type ResolveFn = fn(&str) -> Option<StubFn>;

/// One library codeunit's stub catalog. Codeunit IDs map across BC
/// versions (e.g. Library Assert is 130 in older BC, 130002 in newer);
/// `codeunit_ids` lists every ID that should resolve to this catalog.
#[derive(Debug, Clone)]
pub struct StubCatalog {
    pub codeunit_name: &'static str,
    pub codeunit_ids: &'static [i32],
    pub resolve: ResolveFn,
}

pub const CATALOGS: &[StubCatalog] = &[
    StubCatalog {
        codeunit_name: "Library Assert",
        codeunit_ids: &[130, 130002],
        resolve: library_assert::resolve,
    },
    StubCatalog {
        codeunit_name: "Library - Variable Storage",
        codeunit_ids: &[131004],
        resolve: library_variable_storage::resolve,
    },
    StubCatalog {
        codeunit_name: "Library Random",
        // 130440 is the standard ID in Microsoft_Test Libraries.
        codeunit_ids: &[130440],
        resolve: library_random::resolve,
    },
    StubCatalog {
        codeunit_name: "Any",
        codeunit_ids: &[130500],
        resolve: any::resolve,
    },
];

/// Reset every thread-local piece of stub state to its post-init default.
///
/// Called by the interpreter backend between tests so one test's
/// `LibraryRandom.SetSeed(42)` or `LibraryVariableStorage.Enqueue(x)`
/// can't bleed into the next test that happens to run on the same
/// thread. Without this, parallel test runs are non-deterministic when
/// `JoinSet::spawn_blocking` reuses a worker thread.
///
/// F-OPEN-032.
pub fn reset_thread_local_state() {
    library_variable_storage::reset_queue();
    library_random::reset_lcg();
}

/// Look up a `(codeunit_name_or_id, procedure_name)` pair across all
/// catalogs. Returns the first matching procedure, or None.
pub fn resolve(receiver: &str, procedure: &str) -> Option<StubFn> {
    for cat in CATALOGS {
        let name_match = receiver.eq_ignore_ascii_case(cat.codeunit_name);
        let id_match = receiver
            .parse::<i32>()
            .ok()
            .map(|id| cat.codeunit_ids.contains(&id))
            .unwrap_or(false);
        if name_match || id_match {
            if let Some(f) = (cat.resolve)(procedure) {
                return Some(f);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_library_assert_by_name() {
        assert!(resolve("Library Assert", "AreEqual").is_some());
        assert!(resolve("library assert", "areequal").is_some());
    }

    #[test]
    fn resolve_library_assert_by_id() {
        assert!(resolve("130002", "AreEqual").is_some());
        assert!(resolve("130", "IsTrue").is_some());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(resolve("Library Assert", "DoesNotExist").is_none());
        assert!(resolve("Some Other Codeunit", "AreEqual").is_none());
        assert!(resolve("99999", "AreEqual").is_none());
    }

    #[test]
    fn resolve_library_variable_storage_by_name() {
        assert!(resolve("Library - Variable Storage", "Enqueue").is_some());
        assert!(resolve("library - variable storage", "dequeue").is_some());
        assert!(resolve("LIBRARY - VARIABLE STORAGE", "Length").is_some());
    }

    #[test]
    fn resolve_library_variable_storage_by_id() {
        assert!(resolve("131004", "Enqueue").is_some());
        assert!(resolve("131004", "Dequeue").is_some());
        assert!(resolve("131004", "AssertEmpty").is_some());
    }

    #[test]
    fn resolve_library_variable_storage_unknown_proc_returns_none() {
        assert!(resolve("Library - Variable Storage", "DoesNotExist").is_none());
        assert!(resolve("131004", "AreEqual").is_none());
    }

    #[test]
    fn resolve_library_random_by_name() {
        assert!(resolve("Library Random", "RandInt").is_some());
        assert!(resolve("library random", "randint").is_some());
        assert!(resolve("LIBRARY RANDOM", "RandDec").is_some());
    }

    #[test]
    fn resolve_library_random_by_id() {
        assert!(resolve("130440", "RandInt").is_some());
        assert!(resolve("130440", "RandDec").is_some());
        assert!(resolve("130440", "SetSeed").is_some());
    }

    #[test]
    fn resolve_library_random_unknown_proc_returns_none() {
        assert!(resolve("Library Random", "DoesNotExist").is_none());
        assert!(resolve("130440", "AreEqual").is_none());
    }

    #[test]
    fn resolve_any_by_name() {
        assert!(resolve("Any", "AlphabeticText").is_some());
        assert!(resolve("any", "alphabetictext").is_some());
        assert!(resolve("ANY", "IntegerInRange").is_some());
    }

    #[test]
    fn resolve_any_by_id() {
        assert!(resolve("130500", "AlphabeticText").is_some());
        assert!(resolve("130500", "IntegerInRange").is_some());
        assert!(resolve("130500", "GuidValue").is_some());
    }

    #[test]
    fn resolve_any_unknown_proc_returns_none() {
        assert!(resolve("Any", "DoesNotExist").is_none());
        assert!(resolve("130500", "RandInt").is_none());
    }

    #[test]
    fn reset_thread_local_state_clears_queue_and_lcg() {
        // Positive regression for F-OPEN-032. Prime the LVS queue and
        // advance the LCG (via SetSeed) — the reset must wipe both
        // back to "fresh thread" defaults so the next test can't see
        // either.
        let _ = library_variable_storage::enqueue(&[Value::Integer(42)]);
        let _ = library_random::set_seed(&[Value::Integer(99)]);

        // After reset, the LCG is back to its initial seed and the
        // queue is empty. We verify the queue by trying a Peek which
        // returns an "index out of bounds" error on empty queue; the
        // LCG by checking the first rand_int(100) is the same as it
        // would be on a fresh thread.
        reset_thread_local_state();

        // 1. Queue is empty: Dequeue must fail with "underflow".
        let dq = library_variable_storage::dequeue(&[]);
        match dq {
            Eval::Error(e) => assert!(e.message.contains("underflow")),
            other => panic!("expected Dequeue underflow after reset, got {other:?}"),
        }

        // 2. LCG starts at seed 1: rand_int(100) is deterministic
        // (specifically `((1 * 214013 + 2531011) >> 16) & 0x7FFF) % 100 + 1`
        // = `((2745024) >> 16 & 0x7FFF) % 100 + 1` = `41 % 100 + 1` = 42.
        // We don't hard-code 42 here in case the LCG ever changes — instead
        // we run rand_int twice on freshly-reset state and compare; both
        // must produce identical results.
        reset_thread_local_state();
        let a = library_random::rand_int(&[Value::Integer(100)]);
        reset_thread_local_state();
        let b = library_random::rand_int(&[Value::Integer(100)]);
        match (a, b) {
            (Eval::Normal(Value::Integer(x)), Eval::Normal(Value::Integer(y))) => {
                assert_eq!(
                    x, y,
                    "post-reset rand_int(100) must be deterministic across resets"
                );
            }
            (other_a, other_b) => panic!("unexpected eval results: {other_a:?} vs {other_b:?}"),
        }
    }
}
