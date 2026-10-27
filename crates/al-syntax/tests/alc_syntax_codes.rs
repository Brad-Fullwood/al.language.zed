//! The native syntax error code and message follow alc where the condition is
//! the same. Fixtures in tests/fixtures/alc are alc 18.1.43 errorlogs of the
//! journey project with one mutation each, with the mutated source beside them.

use al_syntax::AlParser;

fn alc_issues(log: &str) -> Vec<serde_json::Value> {
    let log: serde_json::Value = serde_json::from_str(log).unwrap();
    log["issues"].as_array().unwrap().clone()
}

fn check_matches_alc(source: &str, log: &str) {
    let issues = alc_issues(log);
    assert_eq!(issues.len(), 1, "fixture has one alc issue");
    let issue = &issues[0];
    let region = &issue["locations"][0]["analysisTarget"][0]["region"];
    let n = |key: &str| region[key].as_u64().unwrap() as usize - 1;

    let errors = AlParser::parse_quick(source).errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    let error = &errors[0];
    assert_eq!(error.code, issue["ruleId"].as_str());
    assert_eq!(error.message, issue["fullMessage"].as_str().unwrap());
    assert_eq!(error.range.start_point.row, n("startLine"));
    assert_eq!(error.range.start_point.column, n("startColumn"));
    assert_eq!(error.range.end_point.row, n("endLine"));
    assert_eq!(error.range.end_point.column, n("endColumn"));
}

#[test]
fn missing_semicolon_before_an_assignment_matches_alc() {
    check_matches_alc(
        include_str!("fixtures/alc/semi.al"),
        include_str!("fixtures/alc/semi.errorlog.json"),
    );
}

#[test]
fn missing_semicolon_before_exit_matches_alc() {
    check_matches_alc(
        include_str!("fixtures/alc/semi2.al"),
        include_str!("fixtures/alc/semi2.errorlog.json"),
    );
}

#[test]
fn unmapped_shapes_keep_the_generic_error() {
    // alc says AL0104 for these. The native tree-sitter ranges differ from alc's
    // (zero width for a missing `)`, a cascade of errors for a missing `end`),
    // so they stay unmapped and callers apply their own code.
    for source in [
        include_str!("fixtures/alc/noparen.al"),
        include_str!("fixtures/alc/noend.al"),
    ] {
        let errors = AlParser::parse_quick(source).errors;
        assert!(!errors.is_empty());
        assert!(
            errors.iter().all(|error| error.code.is_none()),
            "{errors:?}"
        );
    }
}

#[test]
fn a_stray_token_after_a_semicolon_is_not_a_missing_semicolon() {
    let source = "codeunit 50100 T\n{\n    procedure A()\n    begin\n        Foo();\n        Bar Baz\n    end;\n}\n";
    for error in AlParser::parse_quick(source).errors {
        assert_eq!(error.code, None, "{error:?}");
    }
}
