//! Numeric builtins: Abs, Round and Power.
//!
//! Round follows the BC rounding directions, which is why the precision and
//! direction arguments are read before any arithmetic happens.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

/// Numeric view of an argument for the math builtins.
fn arg_decimal(v: &Value) -> Option<crate::interpreter::value::Decimal> {
    v.as_decimal()
}

/// `Abs(n)` — absolute value, preserving the argument's numeric type and its
/// overflow trap (`Abs(-2147483648)` overflows Integer in BC).
pub(super) fn builtin_abs(args: &[Value]) -> Eval {
    match args {
        [Value::Integer(n)] => match n.checked_abs() {
            Some(a) if (i32::MIN as i64..=i32::MAX as i64).contains(&a) => {
                Eval::Normal(Value::Integer(a))
            }
            _ => eval_error("Abs: integer overflow"),
        },
        [Value::BigInteger(n)] => match n.checked_abs() {
            Some(a) => Eval::Normal(Value::BigInteger(a)),
            None => eval_error("Abs: integer overflow"),
        },
        [Value::Decimal(d)] => Eval::Normal(Value::Decimal(d.abs())),
        [v] => eval_error(format!(
            "Abs expects a numeric value, got {}",
            v.type_name()
        )),
        _ => eval_error("Abs expects exactly 1 argument"),
    }
}

/// `Round(Number [, Precision [, Direction]])`.
///
/// Defaults follow BC: precision `0.01`, direction `'='`, which rounds to the
/// nearest multiple and takes a midpoint away from zero. `'<'` rounds the
/// magnitude down and `'>'` rounds it up, so `Round(-1234.56789, 0.001, '<')`
/// is `-1234.567`. The three directions are the example table on the
/// System.Round reference page. Always returns a `Decimal`.
pub(super) fn builtin_round(args: &[Value]) -> Eval {
    use crate::interpreter::value::Decimal;
    if args.is_empty() || args.len() > 3 {
        return eval_error("Round expects 1 to 3 arguments");
    }
    let Some(number) = arg_decimal(&args[0]) else {
        return eval_error(format!(
            "Round expects a numeric value, got {}",
            args[0].type_name()
        ));
    };
    let precision = match args.get(1) {
        None => Decimal::new(1, 2), // 0.01, BC's default rounding precision
        Some(v) => match arg_decimal(v) {
            Some(p) if p > Decimal::ZERO => p,
            Some(_) => return eval_error("Round: precision must be greater than zero"),
            None => {
                return eval_error(format!(
                    "Round: precision must be numeric, got {}",
                    v.type_name()
                ))
            }
        },
    };
    let direction = match args.get(2) {
        None => "=".to_string(),
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => {
            return eval_error(format!(
                "Round: direction must be Text, got {}",
                v.type_name()
            ))
        }
    };
    let Some(quotient) = number.checked_div(precision) else {
        return eval_error("Round: arithmetic overflow");
    };
    // '<' and '>' move the magnitude, not the signed value: the System.Round
    // page rounds -1234.56789 to -1234.567 with '<' and to -1234.568 with '>'.
    let rounded = match direction.as_str() {
        "=" => {
            quotient.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointAwayFromZero)
        }
        "<" => quotient.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero),
        ">" => quotient.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::AwayFromZero),
        other => {
            return eval_error(format!(
                "Round: direction must be '=', '<' or '>', got '{other}'"
            ))
        }
    };
    match rounded.checked_mul(precision) {
        Some(result) => Eval::Normal(Value::Decimal(result.normalize())),
        None => eval_error("Round: arithmetic overflow"),
    }
}

/// `Power(base, exponent)` — returns `Decimal`, like BC's `Power`.
pub(super) fn builtin_power(args: &[Value]) -> Eval {
    use rust_decimal::MathematicalOps;
    let (base, exponent) = match args {
        [a, b] => match (arg_decimal(a), arg_decimal(b)) {
            (Some(base), Some(exponent)) => (base, exponent),
            _ => {
                return eval_error(format!(
                    "Power expects numeric arguments, got ({}, {})",
                    a.type_name(),
                    b.type_name()
                ))
            }
        },
        _ => return eval_error("Power expects exactly 2 arguments"),
    };
    match base.checked_powd(exponent) {
        Some(result) => Eval::Normal(Value::Decimal(result.normalize())),
        None => eval_error("Power: arithmetic overflow or undefined result"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, ok};
    use crate::interpreter::dispatch::DispatchCtx;

    #[test]
    fn abs_preserves_numeric_type_and_traps_overflow() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Abs",
                vec![Value::Integer(-5)],
                &mut ctx
            )),
            Value::Integer(5)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Abs",
                vec![Value::Decimal(rust_decimal_macros::dec!(-1.25))],
                &mut ctx
            )),
            Value::Decimal(rust_decimal_macros::dec!(1.25))
        );
        assert!(
            dispatch_call(None, "Abs", vec![Value::Integer(i32::MIN as i64)], &mut ctx).is_error()
        );
    }

    #[test]
    fn round_matches_the_documented_bc_directions() {
        use rust_decimal_macros::dec;
        let mut ctx = ctx();
        let round =
            |ctx: &mut DispatchCtx, args: Vec<Value>| dispatch_call(None, "Round", args, ctx);

        // Default precision 0.01: 2.675 → 2.68.
        assert_eq!(
            ok(round(&mut ctx, vec![Value::Decimal(dec!(2.675))])),
            Value::Decimal(dec!(2.68))
        );
        // '=' takes midpoints away from zero: 2.5 → 3, 1.5 → 2, -2.5 → -3.
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(2.5)), Value::Integer(1)]
            )),
            Value::Decimal(dec!(3))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(1.5)), Value::Integer(1)]
            )),
            Value::Decimal(dec!(2))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(-2.5)), Value::Integer(1)]
            )),
            Value::Decimal(dec!(-3))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(0.125)), Value::Decimal(dec!(0.01))]
            )),
            Value::Decimal(dec!(0.13))
        );
        // Every row of the example table on the System.Round reference page,
        // except Round(-1234.56789, 1, '='), which the page prints as -1234
        // while every neighbouring row rounds the magnitude away from zero.
        for (number, precision, direction, expected) in [
            (dec!(1234.56789), dec!(100), "=", dec!(1200)),
            (dec!(1234.56789), dec!(10), "=", dec!(1230)),
            (dec!(1234.56789), dec!(1), "=", dec!(1235)),
            (dec!(1234.56789), dec!(0.1), "=", dec!(1234.6)),
            (dec!(1234.56789), dec!(0.001), "=", dec!(1234.568)),
            (dec!(1234.56789), dec!(0.001), "<", dec!(1234.567)),
            (dec!(1234.56789), dec!(0.001), ">", dec!(1234.568)),
            (dec!(-1234.56789), dec!(100), "=", dec!(-1200)),
            (dec!(-1234.56789), dec!(10), "=", dec!(-1230)),
            (dec!(-1234.56789), dec!(0.1), "=", dec!(-1234.6)),
            (dec!(-1234.56789), dec!(0.001), "=", dec!(-1234.568)),
            (dec!(-1234.56789), dec!(0.001), "<", dec!(-1234.567)),
            (dec!(-1234.56789), dec!(0.001), ">", dec!(-1234.568)),
        ] {
            assert_eq!(
                ok(round(
                    &mut ctx,
                    vec![
                        Value::Decimal(number),
                        Value::Decimal(precision),
                        Value::Text(direction.into())
                    ]
                )),
                Value::Decimal(expected),
                "Round({number}, {precision}, '{direction}')"
            );
        }
        // Explicit directions.
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![
                    Value::Decimal(dec!(2.1)),
                    Value::Integer(1),
                    Value::Text("<".into())
                ]
            )),
            Value::Decimal(dec!(2))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![
                    Value::Decimal(dec!(2.1)),
                    Value::Integer(1),
                    Value::Text(">".into())
                ]
            )),
            Value::Decimal(dec!(3))
        );
        assert!(round(
            &mut ctx,
            vec![
                Value::Decimal(dec!(1.0)),
                Value::Integer(1),
                Value::Text("?".into())
            ]
        )
        .is_error());
    }
}
