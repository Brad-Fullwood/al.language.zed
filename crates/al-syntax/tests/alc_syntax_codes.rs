//! The native syntax error code and message follow alc where the condition is
//! the same. Fixtures in tests/fixtures/alc are alc 18.1.43 errorlogs of the
//! journey project with one mutation each (semiif.errorlog.json was recorded
//! with the uri prefix rewritten to /proj like the others), with the mutated source beside them.

use al_syntax::AlParser;

fn alc_issues(log: &str) -> Vec<serde_json::Value> {
    let log: serde_json::Value = serde_json::from_str(log).unwrap();
    log["issues"].as_array().unwrap().clone()
}

fn assert_matches_issue(error: &al_syntax::SyntaxError, issue: &serde_json::Value) {
    let region = &issue["locations"][0]["analysisTarget"][0]["region"];
    let n = |key: &str| region[key].as_u64().unwrap() as usize - 1;
    assert_eq!(error.code, issue["ruleId"].as_str());
    assert_eq!(error.message, issue["fullMessage"].as_str().unwrap());
    assert_eq!(error.range.start_point.row, n("startLine"));
    assert_eq!(error.range.start_point.column, n("startColumn"));
    assert_eq!(error.range.end_point.row, n("endLine"));
    assert_eq!(error.range.end_point.column, n("endColumn"));
}

fn check_matches_alc(source: &str, log: &str) {
    let issues = alc_issues(log);
    assert_eq!(issues.len(), 1, "fixture has one alc issue");
    let errors = AlParser::parse_quick(source).errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_matches_issue(&errors[0], &issues[0]);
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
fn missing_semicolon_before_an_if_statement_matches_alc() {
    // semiif.al: `Insert(true)` without `;` followed by a three line `if` statement.
    // alc reports AL0111 on the `if` keyword only.
    check_matches_alc(
        include_str!("fixtures/alc/semiif.al"),
        include_str!("fixtures/alc/semiif.errorlog.json"),
    );
}

#[test]
fn missing_closing_parenthesis_matches_alc() {
    // alc reports the token that stands where `)` belongs, not a zero width point.
    check_matches_alc(
        include_str!("fixtures/alc/noparen.al"),
        include_str!("fixtures/alc/noparen.errorlog.json"),
    );
}

#[test]
fn missing_end_matches_the_alc_errors() {
    // alc reports two issues, both on the next `procedure` keyword. tree-sitter
    // goes on to raise further errors while it recovers (a cascade that alc does
    // not report). Those stay generic (code None), the first two are alc's.
    let issues = alc_issues(include_str!("fixtures/alc/noend.errorlog.json"));
    assert_eq!(issues.len(), 2);
    let errors = AlParser::parse_quick(include_str!("fixtures/alc/noend.al")).errors;
    assert!(errors.len() >= 2, "{errors:?}");
    assert_matches_issue(&errors[0], &issues[0]);
    assert_matches_issue(&errors[1], &issues[1]);
    assert!(
        errors[2..].iter().all(|error| error.code.is_none()),
        "{errors:?}"
    );
}

#[test]
fn an_unrecorded_missing_end_shape_is_not_mapped_to_al0104() {
    // The same file with `local procedure` after the missing `end`. No alc
    // errorlog exists for this, so the 'end' expected mapping must not fire.
    let source = include_str!("fixtures/alc/noend.al").replacen(
        "procedure CountForCustomer",
        "local procedure CountForCustomer",
        1,
    );
    let errors = AlParser::parse_quick(&source).errors;
    assert!(!errors.is_empty());
    assert!(
        errors
            .iter()
            .all(|error| error.code != Some(al_syntax::ALC_EXPECTED_TOKEN_CODE)),
        "{errors:?}"
    );
}

#[test]
fn a_stray_token_after_a_semicolon_is_not_a_missing_semicolon() {
    let source = "codeunit 50100 T\n{\n    procedure A()\n    begin\n        Foo();\n        Bar Baz\n    end;\n}\n";
    for error in AlParser::parse_quick(source).errors {
        assert_eq!(error.code, None, "{error:?}");
    }
}

#[test]
fn the_quick_fix_position_for_a_missing_semicolon_before_if_is_the_end_of_the_statement() {
    let source = include_str!("fixtures/alc/semiif.al");
    let parsed = AlParser::parse_quick(source);
    let if_start = source.find("if NewQuantity").unwrap();
    let insert = al_syntax::missing_semicolon_insert_byte_at(&parsed.tree, if_start).unwrap();
    assert_eq!(
        &source[..insert].lines().last().unwrap().trim(),
        &"JourneyItem.Insert(true)"
    );
}
