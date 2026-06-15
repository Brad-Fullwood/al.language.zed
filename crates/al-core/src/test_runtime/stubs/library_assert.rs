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

use crate::test_runtime::interpreter::scope::Eval;
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};

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
    let (Some(ef), Some(af), Some(pf)) = (to_f64(e), to_f64(a), to_f64(p)) else {
        return err("Assert.AreNearlyEqual: non-numeric argument");
    };
    // Two identical NaN sentinels are treated as equal — they represent
    // the same invalid state (matches the Value::Eq impl which uses
    // total_cmp). Mixed NaN / non-NaN pairs are an error.
    if ef.is_nan() || af.is_nan() {
        return if ef.is_nan() && af.is_nan() {
            ok()
        } else {
            err("Assert.AreNearlyEqual: cannot compare NaN with a numeric value")
        };
    }
    if pf.is_nan() {
        return err("Assert.AreNearlyEqual: precision must not be NaN");
    }
    // Same-signed Inf is treated as equal; opposite-signed or one-sided
    // Inf is an error.
    if ef.is_infinite() || af.is_infinite() {
        return if ef == af {
            ok()
        } else {
            err(format!(
                "Assert.AreNearlyEqual: cannot compare {ef} and {af} (infinity)"
            ))
        };
    }
    if (ef - af).abs() <= pf {
        ok()
    } else {
        let suffix = if msg.is_empty() {
            String::new()
        } else {
            format!(": {msg}")
        };
        err(format!(
            "Assert.AreNearlyEqual failed{suffix}: |{ef} - {af}| > {pf}"
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
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x)) => (*x as f64) == *y,
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
        Decimal(n) => n.to_string(),
        Boolean(b) => b.to_string(),
        Text(s) | Code(s) => format!("\"{s}\""),
        Date(d) | Time(d) | DateTime(d) => d.to_string(),
        Null => "null".into(),
        Empty => "<empty>".into(),
        other => format!("<{}>", other.type_name()),
    }
}

fn to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(n) => Some(*n as f64),
        Value::Decimal(n) => Some(*n),
        _ => None,
    }
}

/// Resolve a procedure name (case-insensitive) to its Rust impl. The
/// dispatch layer calls this when the receiver of a member-call is a
/// Library Assert codeunit (or when the bare procedure name is known
/// to belong to Library Assert). Returns `None` if the name is unknown.
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

    fn assert_pass(eval: Eval) {
        match eval {
            Eval::Normal(_) => {}
            Eval::Error(e) => panic!("expected pass, got error: {}", e.message),
            Eval::Exit(_) => panic!("expected pass, got exit"),
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
        // Negative: false condition produces an Eval::Error containing
        // the user-supplied message.
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
        assert_pass(are_equal(&[Value::Integer(5), Value::Decimal(5.0)]));
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
            Value::Decimal(1.0),
            Value::Decimal(1.0001),
            Value::Decimal(0.001),
        ]));
        assert_fail_contains(
            are_nearly_equal(&[
                Value::Decimal(1.0),
                Value::Decimal(1.5),
                Value::Decimal(0.001),
            ]),
            "|1 - 1.5| > 0.001",
        );
    }

    #[test]
    fn fail_always_fails() {
        // Negative: Fail unconditionally produces Eval::Error.
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
        // Negative: wrong arg count produces a clear error rather than
        // panicking or silently succeeding.
        assert_fail_contains(is_true(&[Value::Integer(1)]), "Assert.IsTrue expects");
        assert_fail_contains(are_equal(&[Value::Integer(1)]), "Assert.AreEqual expects");
    }

    // ── Adversarial tests (adversarial-h) ─────────────────────────────────────

    #[test]
    fn are_nearly_equal_nan_inputs_adversarial_h_11() {
        // FINDING P1 wrong-result: AreNearlyEqual(NaN, NaN, 0.001) fails with
        // "AreNearlyEqual failed: |NaN - NaN| > 0.001" because (NaN-NaN).abs()
        // = NaN and NaN <= precision is false. Two identical NaN sentinels
        // should be treated as equal — they represent the same invalid state.
        // Expected: Normal (pass)
        // Observed: Error("AreNearlyEqual failed: |NaN - NaN| > 0.001")
        let result = are_nearly_equal(&[
            Value::Decimal(f64::NAN),
            Value::Decimal(f64::NAN),
            Value::Decimal(0.001),
        ]);
        assert!(
            matches!(result, Eval::Normal(_)),
            "AreNearlyEqual(NaN, NaN, ..) should pass: NaN == NaN sentinel, got: {:?}",
            result
        );
    }

    #[test]
    fn are_nearly_equal_inf_inf_adversarial_h_12() {
        // FINDING P1 wrong-result: AreNearlyEqual(Inf, Inf, 0.001) fails with
        // "AreNearlyEqual failed: |inf - inf| > 0.001" because (Inf-Inf).abs()
        // = NaN and NaN <= 0.001 is false. But Inf == Inf exactly.
        // Expected: Normal (pass)
        // Observed: Error("AreNearlyEqual failed: |inf - inf| > 0.001")
        let result = are_nearly_equal(&[
            Value::Decimal(f64::INFINITY),
            Value::Decimal(f64::INFINITY),
            Value::Decimal(0.001),
        ]);
        assert!(
            matches!(result, Eval::Normal(_)),
            "AreNearlyEqual(Inf, Inf, ..) should pass: Inf == Inf exactly, got: {:?}",
            result
        );
    }
}
