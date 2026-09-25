//! Library Assert (codeunit 130 / 130002) — native Rust port.
//!
//! Every procedure follows AL's "raise via `Error(...)`" convention,
//! translated to `Eval::Error(ErrorInfo { message, .. })`.

use crate::interpreter::scope::Eval;
use crate::interpreter::value::{Decimal, ErrorInfo, Value};

fn err(message: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: message.into(),
        error_type: Some("AssertionError".to_string()),
        source: Some("Library Assert".to_string()),
    })
}

fn ok() -> Eval {
    Eval::Normal(Value::Empty)
}

/// `Assert.IsTrue(condition: Boolean; message: Text)` — fail if condition is false.
pub fn is_true(args: &[Value]) -> Eval {
    match args {
        [Value::Boolean(true), _] => ok(),
        [Value::Boolean(false), Value::Text(msg)] | [Value::Boolean(false), Value::Code(msg)] => {
            err(format!("Assert.IsTrue failed: {msg}"))
        }
        [Value::Boolean(false)] => err("Assert.IsTrue failed"),
        _ => err("Assert.IsTrue expects (Boolean, Text)"),
    }
}

/// `Assert.IsFalse(condition: Boolean; message: Text)` — fail if condition is true.
pub fn is_false(args: &[Value]) -> Eval {
    match args {
        [Value::Boolean(false), _] => ok(),
        [Value::Boolean(true), Value::Text(msg)] | [Value::Boolean(true), Value::Code(msg)] => {
            err(format!("Assert.IsFalse failed: {msg}"))
        }
        [Value::Boolean(true)] => err("Assert.IsFalse failed"),
        _ => err("Assert.IsFalse expects (Boolean, Text)"),
    }
}

/// `Assert.AreEqual(expected: Variant; actual: Variant; message: Text)`.
pub fn are_equal(args: &[Value]) -> Eval {
    let (expected, actual, msg) = match args {
        [e, a] => (e, a, String::new()),
        [e, a, Value::Text(m)] | [e, a, Value::Code(m)] => (e, a, m.clone()),
        _ => return err("Assert.AreEqual expects (Variant, Variant[, Text])"),
    };
    if crate::interpreter::eval_expr::values_equal(expected, actual) {
        ok()
    } else {
        let suffix = if msg.is_empty() {
            String::new()
        } else {
            format!(": {msg}")
        };
        err(format!(
            "Assert.AreEqual failed{suffix}: expected {} but got {}",
            render_value(expected),
            render_value(actual)
        ))
    }
}

/// `Assert.AreNotEqual(expected: Variant; actual: Variant; message: Text)`.
pub fn are_not_equal(args: &[Value]) -> Eval {
    let (expected, actual, msg) = match args {
        [e, a] => (e, a, String::new()),
        [e, a, Value::Text(m)] | [e, a, Value::Code(m)] => (e, a, m.clone()),
        _ => return err("Assert.AreNotEqual expects (Variant, Variant[, Text])"),
    };
    if !crate::interpreter::eval_expr::values_equal(expected, actual) {
        ok()
    } else {
        let suffix = if msg.is_empty() {
            String::new()
        } else {
            format!(": {msg}")
        };
        err(format!(
            "Assert.AreNotEqual failed{suffix}: both values were {}",
            render_value(expected),
        ))
    }
}

/// `Assert.AreNearlyEqual(expected: Decimal; actual: Decimal; precision: Decimal; message: Text)`.
pub fn are_nearly_equal(args: &[Value]) -> Eval {
    let (e, a, p, msg) = match args {
        [e, a, p] => (e, a, p, String::new()),
        [e, a, p, Value::Text(m)] | [e, a, p, Value::Code(m)] => (e, a, p, m.clone()),
        _ => return err("Assert.AreNearlyEqual expects (Decimal, Decimal, Decimal[, Text])"),
    };
    let (Some(ef), Some(af), Some(pf)) = (to_decimal(e), to_decimal(a), to_decimal(p)) else {
        return err("Assert.AreNearlyEqual: non-numeric argument");
    };
    // Exact decimals: no NaN/infinity can arise, so the comparison is a plain
    // exact tolerance check.
    if (ef - af).abs() <= pf {
        ok()
    } else {
        let suffix = if msg.is_empty() {
            String::new()
        } else {
            format!(": {msg}")
        };
        err(format!(
            "Assert.AreNearlyEqual failed{suffix}: |{} - {}| > {}",
            ef.normalize(),
            af.normalize(),
            pf.normalize()
        ))
    }
}

/// `Assert.Fail(message: Text)` — unconditional failure.
pub fn fail(args: &[Value]) -> Eval {
    let msg = match args {
        [] => "Assert.Fail invoked".to_string(),
        [Value::Text(m)] | [Value::Code(m)] => format!("Assert.Fail: {m}"),
        _ => "Assert.Fail expects (Text)".to_string(),
    };
    err(msg)
}

/// `Assert.ExpectedError(Expected: Text)` — the last error's text must contain
/// `Expected`.
///
/// Ported from the Library Assert codeunit in BCApps, which compares with
/// `StrPos(GetLastErrorText(), Expected) = 0` and reports
/// `Assert.ExpectedError failed. Expected: %1. Actual: %2.` on a miss. An empty
/// `Expected` with no error raised is the codeunit's "The error has not been
/// thrown." case; this runtime has no error callstack, so an absent
/// `ctx.last_error` stands in for an empty one.
///
/// Unlike the other members this one reads the interpreter context, so it is
/// dispatched in `dispatch_call_scoped` rather than through a `StubFn`.
pub fn expected_error(args: &[Value], last_error: Option<&ErrorInfo>) -> Eval {
    let expected = match args {
        [Value::Text(expected)] | [Value::Code(expected)] => expected.as_str(),
        [] => return err("Assert.ExpectedError expects (Text)"),
        _ => return err("Assert.ExpectedError expects (Text)"),
    };
    let actual = last_error.map(|error| error.message.as_str()).unwrap_or("");
    if actual.is_empty() && expected.is_empty() {
        return match last_error {
            Some(_) => ok(),
            None => err("The error has not been thrown."),
        };
    }
    if actual.contains(expected) {
        ok()
    } else {
        err(format!(
            "Assert.ExpectedError failed. Expected: {expected}. Actual: {actual}."
        ))
    }
}

fn render_value(v: &Value) -> String {
    use Value::*;
    match v {
        Integer(n) => n.to_string(),
        Decimal(n) => n.normalize().to_string(),
        Boolean(b) => b.to_string(),
        Text(s) | Code(s) => format!("\"{s}\""),
        // A second renderer used to print these as their day and millisecond
        // carriers, so `expected 739068 but got 739069` named no date.
        Date(_) | Time(_) | DateTime(_) | Duration(_) | Option { .. } => {
            crate::interpreter::dispatch::render_value(v)
        }
        Null => "null".into(),
        Empty => "<empty>".into(),
        other => format!("<{}>", other.type_name()),
    }
}

fn to_decimal(v: &Value) -> Option<Decimal> {
    v.as_decimal()
}

pub fn resolve(procedure: &str) -> Option<fn(&[Value]) -> Eval> {
    match procedure.to_ascii_lowercase().as_str() {
        "istrue" => Some(is_true),
        "isfalse" => Some(is_false),
        "areequal" => Some(are_equal),
        "arenotequal" => Some(are_not_equal),
        "arenearlyequal" => Some(are_nearly_equal),
        "fail" => Some(fail),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn assert_pass(eval: Eval) {
        match eval {
            Eval::Normal(_) => {}
            Eval::Error(e) => panic!("expected pass, got error: {}", e.message),
            Eval::Exit(_) => panic!("expected pass, got exit"),
            Eval::Break | Eval::Continue => panic!("expected pass, got break/continue"),
        }
    }

    fn assert_fail_contains(eval: Eval, needle: &str) {
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains(needle),
                "expected message containing `{needle}`, got: {}",
                e.message
            ),
            other => panic!("expected fail, got {other:?}"),
        }
    }

    #[test]
    fn istrue_passes_for_true() {
        assert_pass(is_true(&[Value::Boolean(true), Value::Text("msg".into())]));
    }

    #[test]
    fn istrue_fails_for_false_with_message() {
        assert_fail_contains(
            is_true(&[Value::Boolean(false), Value::Text("expected truth".into())]),
            "expected truth",
        );
    }

    #[test]
    fn isfalse_inverse_of_istrue() {
        assert_pass(is_false(&[Value::Boolean(false), Value::Text("ok".into())]));
        assert_fail_contains(
            is_false(&[Value::Boolean(true), Value::Text("nope".into())]),
            "nope",
        );
    }

    #[test]
    fn areequal_integers() {
        assert_pass(are_equal(&[Value::Integer(5), Value::Integer(5)]));
        assert_fail_contains(
            are_equal(&[Value::Integer(5), Value::Integer(6)]),
            "expected 5",
        );
    }

    #[test]
    fn areequal_text() {
        assert_pass(are_equal(&[
            Value::Text("hello".into()),
            Value::Text("hello".into()),
        ]));
        assert_fail_contains(
            are_equal(&[
                Value::Text("hello".into()),
                Value::Text("world".into()),
                Value::Text("greeting".into()),
            ]),
            "greeting",
        );
    }

    #[test]
    fn areequal_mixed_numeric() {
        assert_pass(are_equal(&[Value::Integer(5), Value::Decimal(dec!(5.0))]));
    }

    #[test]
    fn arenotequal_works() {
        assert_pass(are_not_equal(&[Value::Integer(1), Value::Integer(2)]));
        assert_fail_contains(
            are_not_equal(&[Value::Integer(7), Value::Integer(7)]),
            "both values were 7",
        );
    }

    #[test]
    fn arenearlyequal_within_precision() {
        assert_pass(are_nearly_equal(&[
            Value::Decimal(dec!(1.0)),
            Value::Decimal(dec!(1.0001)),
            Value::Decimal(dec!(0.001)),
        ]));
        assert_fail_contains(
            are_nearly_equal(&[
                Value::Decimal(dec!(1.0)),
                Value::Decimal(dec!(1.5)),
                Value::Decimal(dec!(0.001)),
            ]),
            "|1 - 1.5| > 0.001",
        );
    }

    #[test]
    fn fail_always_fails() {
        assert_fail_contains(fail(&[Value::Text("kaboom".into())]), "kaboom");
        assert_fail_contains(fail(&[]), "Assert.Fail invoked");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert!(resolve("AreEqual").is_some());
        assert!(resolve("AREEQUAL").is_some());
        assert!(resolve("areequal").is_some());
        assert!(resolve("DoesNotExist").is_none());
    }

    #[test]
    fn arity_mismatch_is_error() {
        assert_fail_contains(is_true(&[Value::Integer(1)]), "Assert.IsTrue expects");
        assert_fail_contains(are_equal(&[Value::Integer(1)]), "Assert.AreEqual expects");
    }

    #[test]
    fn are_nearly_equal_is_exact_no_float_drift() {
        let sum = dec!(0.1) + dec!(0.2);
        let result = are_nearly_equal(&[
            Value::Decimal(sum),
            Value::Decimal(dec!(0.3)),
            Value::Decimal(Decimal::ZERO),
        ]);
        assert!(
            matches!(result, Eval::Normal(_)),
            "0.1 + 0.2 must be exactly 0.3 (zero tolerance), got: {result:?}"
        );
    }

    #[test]
    fn are_nearly_equal_respects_precision_band() {
        assert_pass(are_nearly_equal(&[
            Value::Decimal(dec!(1.0)),
            Value::Decimal(dec!(1.0009)),
            Value::Decimal(dec!(0.001)),
        ]));
        assert_fail_contains(
            are_nearly_equal(&[
                Value::Decimal(dec!(1.0)),
                Value::Decimal(dec!(1.01)),
                Value::Decimal(dec!(0.001)),
            ]),
            "AreNearlyEqual failed",
        );
    }

    #[test]
    fn failure_messages_name_dates_not_day_carriers() {
        let july_first = crate::interpreter::value::al_days_from_ymd(2024, 7, 1);
        assert_fail_contains(
            are_equal(&[
                Value::Date(july_first),
                Value::Date(july_first + 1),
                Value::Text("date".into()),
            ]),
            "07/01/2024",
        );
        assert_fail_contains(
            are_equal(&[Value::Time(45_296_000), Value::Time(0)]),
            "12:34:56",
        );
    }

    #[test]
    fn expected_error_matches_a_substring_and_reports_both_texts() {
        let raised = ErrorInfo {
            message: "The order must have a customer".to_string(),
            error_type: None,
            source: None,
        };
        assert_pass(expected_error(
            &[Value::Text("must have a customer".into())],
            Some(&raised),
        ));
        assert_fail_contains(
            expected_error(&[Value::Text("something else".into())], Some(&raised)),
            "Assert.ExpectedError failed. Expected: something else. Actual: The order must have a \
             customer.",
        );
        assert_fail_contains(
            expected_error(&[Value::Text(String::new())], None),
            "The error has not been thrown.",
        );
    }
}
