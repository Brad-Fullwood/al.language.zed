//! Assertions shared by the crate's test modules.

/// Assert that `source` parses through tree-sitter-al with no errors.
///
/// Every generator and scaffold template emits AL that a user compiles, so a
/// string-content assertion is not enough: a template that drifts out of sync
/// with the grammar (a renamed property, tightened syntax) still matches the
/// string it always did. Re-parsing is what catches it.
pub(crate) fn assert_al_parses(label: &str, source: &str) {
    let result = al_syntax::parser::AlParser::parse_quick(source);
    assert!(
        result.errors.is_empty(),
        "{label} did not parse cleanly:\n{source}\nerrors: {:?}",
        result.errors
    );
}
