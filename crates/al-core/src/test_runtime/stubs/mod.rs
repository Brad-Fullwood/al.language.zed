//! Native Rust ports of BC test libraries — Phase 2.
//!
//! Each library lives in its own submodule and exposes a `resolve` fn
//! that maps a procedure name to its implementation. The dispatch layer
//! consults these tables when the call receiver is the corresponding
//! library codeunit.

pub mod library_assert;

use crate::test_runtime::interpreter::scope::Eval;
use crate::test_runtime::interpreter::value::Value;

/// One library codeunit's stub catalog. Codeunit IDs map across BC
/// versions (e.g. Library Assert is 130 in older BC, 130002 in newer);
/// `codeunit_ids` lists every ID that should resolve to this catalog.
#[derive(Debug, Clone)]
pub struct StubCatalog {
    pub codeunit_name: &'static str,
    pub codeunit_ids: &'static [i32],
    pub resolve: fn(&str) -> Option<fn(&[Value]) -> Eval>,
}

/// All built-in stub catalogs. Phase 2 ships Library Assert; Phase 3
/// adds Library Variable Storage, Any, Library Random, etc.
pub const CATALOGS: &[StubCatalog] = &[StubCatalog {
    codeunit_name: "Library Assert",
    codeunit_ids: &[130, 130002],
    resolve: library_assert::resolve,
}];

/// Look up a `(codeunit_name_or_id, procedure_name)` pair across all
/// catalogs. Returns the first matching procedure, or None.
pub fn resolve(receiver: &str, procedure: &str) -> Option<fn(&[Value]) -> Eval> {
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
}
