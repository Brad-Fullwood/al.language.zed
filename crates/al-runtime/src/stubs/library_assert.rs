//! Library Assert (codeunit 130 / 130002) — native Rust port.
//!
//! AL test source for the real codeunit lives in BCApps at:
//!   `tests/.repos/BCApps/src/Tools/Test Framework/Test Libraries/Assert/src/LibraryAssert.Codeunit.al`
//!
//! Phase 2 takes the **fast-path** approach: instead of interpreting the
//! AL source of Library Assert, the dispatch layer recognises calls to
//! Library Assert procedure names and invokes the equivalent Rust code
//! here. Same external semantics; far less interpreter surface to maintain.
//!
//! Every procedure follows AL's "raise via `Error(...)`" convention,
//! which we translate to `Eval::Error(ErrorInfo { message, .. })`. AL
//! `asserterror` blocks (Phase 3 work) catch these; until then a failing
//! assertion propagates as an `Eval::Error` and aborts the test method.

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
    if values_equal(expected, actual) {
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
    if !values_equal(expected, actual) {
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
    // exact tolerance check (C3).
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

fn values_equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        (Decimal(x), Decimal(y)) => x == y,
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x)) => {
            rust_decimal::Decimal::from(*x) == *y
        }
        (Boolean(x), Boolean(y)) => x == y,
        (Text(x), Text(y)) | (Code(x), Code(y)) => x == y,
        (Text(x), Code(y)) | (Code(y), Text(x)) => x == y,
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x == y,
        (Null, Null) | (Empty, Empty) => true,
        _ => false,
    }
}

fn render_value(v: &Value) -> String {
    use Value::*;
    match v {
        Integer(n) => n.to_string(),
        Decimal(n) => n.normalize().to_string(),
        Boolean(b) => b.to_string(),
        Text(s) | Code(s) => format!("\"{s}\""),
        Date(d) | Time(d) | DateTime(d) => d.to_string(),
        Null => "null".into(),
        Empty => "<empty>".into(),
        other => format!("<{}>", other.type_name()),
    }
}

fn to_decimal(v: &Value) -> Option<Decimal> {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => Some(Decimal::from(*n)),
        Value::Decimal(n) => Some(*n),
        _ => None,
    }
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
        // 5 == 5.0 across Integer/Decimal — Library Assert's variant
        // semantics treat these as equal.
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
        // C3: exact decimals mean a computed sum has no binary-float residue,
        // so AreNearlyEqual with ZERO tolerance still passes for 0.1 + 0.2 = 0.3
        // (which fails under f64). NaN/infinity can no longer be constructed.
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
        // Just inside the band passes; just outside fails.
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
}
