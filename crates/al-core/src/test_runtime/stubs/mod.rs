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

/// Built-in stub procedure: takes positional Value args, returns an Eval.
pub type StubFn = fn(&[Value]) -> Eval;
/// Procedure-name resolver for a library catalog.
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

/// All built-in stub catalogs.
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
        // Negative paths.
        assert!(resolve("Library Assert", "DoesNotExist").is_none());
        assert!(resolve("Some Other Codeunit", "AreEqual").is_none());
        assert!(resolve("99999", "AreEqual").is_none());
    }

    // ── Library Variable Storage routing ─────────────────────────────────────

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
        // Negative: known codeunit, unknown procedure.
        assert!(resolve("Library - Variable Storage", "DoesNotExist").is_none());
        assert!(resolve("131004", "AreEqual").is_none());
    }

    // ── Library Random routing ────────────────────────────────────────────────

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
        // Negative: known codeunit, unknown procedure.
        assert!(resolve("Library Random", "DoesNotExist").is_none());
        assert!(resolve("130440", "AreEqual").is_none());
    }

    // ── Any routing ───────────────────────────────────────────────────────────

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
        // Negative: known codeunit, unknown procedure.
        assert!(resolve("Any", "DoesNotExist").is_none());
        assert!(resolve("130500", "RandInt").is_none());
    }
}
