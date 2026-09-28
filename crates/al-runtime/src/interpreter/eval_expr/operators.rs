//! Unary and binary operator evaluation: AL arithmetic (`+ - * / div mod`),
//! Date/Time/Duration arithmetic, and the equality/ordering used by `=` `<>`
//! `< <= > >=` and `in`. `apply_binary` is the single entry point the rest of
//! the interpreter (record filters, CASE matching, `Assert.AreEqual`) also
//! calls for AL value comparison.

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{self, Decimal, ErrorInfo, Value};

use super::entry::eval_expr;
use super::helpers::{named_child, utf8_text};

pub(super) fn eval_unary(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Grammar: unary_expression = (unary_operator unary_expression) | postfix_expression
    //   - 2 named children: [unary_operator, unary_expression]
    //   - 1 named child:    [postfix_expression] — transparent wrapper
    let named_count = node.named_child_count();
    if named_count <= 1 {
        return match named_child(node, 0) {
            Some(inner) => eval_expr(inner, source, stack, ctx),
            None => Eval::Error(error_info("unary expression: empty node")),
        };
    }
    let op_node = match named_child(node, 0) {
        Some(n) => n,
        None => return Eval::Error(error_info("unary expression missing operator")),
    };
    let operand_node = match named_child(node, 1) {
        Some(n) => n,
        None => return Eval::Error(error_info("unary expression missing operand")),
    };
    let operator_text = utf8_text(op_node, source).unwrap_or("").trim();

    let value = match eval_expr(operand_node, source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    match (operator_text.to_ascii_lowercase().as_str(), value) {
        ("+", Value::Integer(n)) => Eval::Normal(Value::Integer(n)),
        ("+", Value::BigInteger(n)) => Eval::Normal(Value::BigInteger(n)),
        ("+", Value::Decimal(n)) => Eval::Normal(Value::Decimal(n)),
        ("+", Value::Option { ordinal, .. }) => Eval::Normal(Value::Integer(ordinal)),
        ("-", Value::Integer(n)) => checked_int(n.checked_neg(), false),
        ("-", Value::BigInteger(n)) => checked_int(n.checked_neg(), true),
        ("-", Value::Decimal(n)) => Eval::Normal(Value::Decimal(-n)),
        ("-", Value::Option { ordinal, .. }) => checked_int(ordinal.checked_neg(), false),
        ("not", Value::Boolean(b)) => Eval::Normal(Value::Boolean(!b)),
        (op, v) => Eval::Error(error_info(format!(
            "unary operator `{op}` not supported on {}",
            v.type_name()
        ))),
    }
}

/// A numeric operand classified for arithmetic: an integer-like value (with a
/// flag for whether it is a `BigInteger`, which widens the overflow trap from
/// i32 to i64) or a `Decimal`.
enum Num {
    Int { val: i64, big: bool },
    Dec(Decimal),
}

impl Num {
    fn to_decimal(&self) -> Decimal {
        match self {
            Num::Int { val, .. } => Decimal::from(*val),
            Num::Dec(d) => *d,
        }
    }
}

fn classify_numeric(v: &Value) -> Option<Num> {
    match v {
        Value::Integer(n) => Some(Num::Int {
            val: *n,
            big: false,
        }),
        Value::BigInteger(n) => Some(Num::Int { val: *n, big: true }),
        Value::Char(value) => Some(Num::Int {
            val: *value as i64,
            big: false,
        }),
        Value::Option { ordinal, .. } => Some(Num::Int {
            val: *ordinal,
            big: false,
        }),
        Value::Decimal(d) => Some(Num::Dec(*d)),
        _ => None,
    }
}

/// Convert an AL integer-like/whole-decimal offset used by Date/Time
/// arithmetic. A `Duration` operand contributes its millisecond carrier.
/// Fractional Decimal offsets are invalid for these operations.
fn whole_offset(value: &Value) -> Result<i64, ErrorInfo> {
    if let Value::Duration(ms) = value {
        return Ok(*ms);
    }
    match classify_numeric(value) {
        Some(Num::Int { val, .. }) => Ok(val),
        Some(Num::Dec(decimal)) if decimal.fract().is_zero() => decimal
            .to_string()
            .parse::<i64>()
            .map_err(|_| error_info("Date/Time arithmetic offset is out of range")),
        Some(Num::Dec(_)) => Err(error_info(
            "Date/Time arithmetic requires a whole-number offset",
        )),
        None => Err(error_info(format!(
            "Date/Time arithmetic does not support {}",
            value.type_name()
        ))),
    }
}

fn apply_temporal_arithmetic(operator: &str, left: &Value, right: &Value) -> Option<Eval> {
    let op = operator.to_ascii_lowercase();
    match (op.as_str(), left, right) {
        // DateTime difference → Duration (milliseconds).
        ("-", Value::DateTime(l), Value::DateTime(r)) => {
            if *l == 0 || *r == 0 {
                return Some(Eval::Error(error_info(
                    "DateTime arithmetic is undefined for the zero DateTime",
                )));
            }
            Some(match l.checked_sub(*r) {
                Some(ms) => Eval::Normal(Value::Duration(ms)),
                None => Eval::Error(error_info("DateTime arithmetic overflow")),
            })
        }
        // DateTime ± Duration/number → DateTime.
        ("+", Value::DateTime(dt), offset) | ("+", offset, Value::DateTime(dt)) => {
            if *dt == 0 {
                return Some(Eval::Error(error_info(
                    "DateTime arithmetic is undefined for the zero DateTime",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    dt.checked_add(offset)
                        .ok_or_else(|| error_info("DateTime arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::DateTime(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("-", Value::DateTime(dt), offset) => {
            if *dt == 0 {
                return Some(Eval::Error(error_info(
                    "DateTime arithmetic is undefined for the zero DateTime",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    dt.checked_sub(offset)
                        .ok_or_else(|| error_info("DateTime arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::DateTime(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("+", Value::Date(date), offset) | ("+", offset, Value::Date(date)) => {
            if *date == 0 {
                return Some(Eval::Error(error_info(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    date.checked_add(offset)
                        .ok_or_else(|| error_info("Date arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Date(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("-", Value::Date(left), Value::Date(right)) => {
            if *left == 0 || *right == 0 {
                return Some(Eval::Error(error_info(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(checked_int(left.checked_sub(*right), false))
        }
        ("-", Value::Date(date), offset) => {
            if *date == 0 {
                return Some(Eval::Error(error_info(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    date.checked_sub(offset)
                        .ok_or_else(|| error_info("Date arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Date(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("+", Value::Time(time), offset) | ("+", offset, Value::Time(time)) => {
            if *time == 0 {
                return Some(Eval::Error(error_info(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    time.checked_add(offset)
                        .filter(|value| (0..value::MS_PER_DAY).contains(value))
                        .ok_or_else(|| error_info("Time arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Time(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("-", Value::Time(left), Value::Time(right)) => {
            if *left == 0 || *right == 0 {
                return Some(Eval::Error(error_info(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            // BC: the difference of two Times is a Duration (milliseconds),
            // not an Integer.
            Some(match left.checked_sub(*right) {
                Some(ms) => Eval::Normal(Value::Duration(ms)),
                None => Eval::Error(error_info("Time arithmetic overflow")),
            })
        }
        ("-", Value::Time(time), offset) => {
            if *time == 0 {
                return Some(Eval::Error(error_info(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    time.checked_sub(offset)
                        .filter(|value| (0..value::MS_PER_DAY).contains(value))
                        .ok_or_else(|| error_info("Time arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Time(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        // Duration ± Duration/number → Duration. These arms must come AFTER
        // the Date/Time arms: their symmetric `("+", offset, Duration)`
        // pattern would otherwise capture `Duration + Date` / `Duration +
        // Time` with the Date/Time operand as `offset`, and `whole_offset`
        // rejects Date/Time operands — the temporal arms above handle those
        // shapes (with the Duration operand contributing its milliseconds).
        ("+", Value::Duration(l), offset) | ("+", offset, Value::Duration(l)) => Some(
            match whole_offset(offset).and_then(|offset| {
                l.checked_add(offset)
                    .ok_or_else(|| error_info("Duration arithmetic overflow"))
            }) {
                Ok(value) => Eval::Normal(Value::Duration(value)),
                Err(error) => Eval::Error(error),
            },
        ),
        ("-", Value::Duration(l), offset) => Some(
            match whole_offset(offset).and_then(|offset| {
                l.checked_sub(offset)
                    .ok_or_else(|| error_info("Duration arithmetic overflow"))
            }) {
                Ok(value) => Eval::Normal(Value::Duration(value)),
                Err(error) => Eval::Error(error),
            },
        ),
        _ => None,
    }
}

/// Wrap an integer arithmetic result at the correct width. BC's `Integer` is
/// 32-bit and traps overflow at runtime; `BigInteger` is 64-bit. The i64
/// carrier means an i64 `checked_*` alone would let `2147483647 * 3` silently
/// produce a value no `Integer` can hold, so an `Integer` result is also
/// range-checked against i32. Either overflow is a runtime error, never a
/// silent wrap.
fn checked_int(result: Option<i64>, big: bool) -> Eval {
    match result {
        Some(n) if big => Eval::Normal(Value::BigInteger(n)),
        Some(n) if (i32::MIN as i64..=i32::MAX as i64).contains(&n) => {
            Eval::Normal(Value::Integer(n))
        }
        _ => Eval::Error(error_info("integer overflow")),
    }
}

/// Wrap a Decimal result. `rust_decimal` has no NaN/infinity; the only failure
/// mode is overflow of the 96-bit range, which `checked_*` reports as `None`.
fn checked_decimal(d: Option<Decimal>) -> Eval {
    match d {
        Some(v) => Eval::Normal(Value::Decimal(v)),
        None => Eval::Error(error_info("decimal arithmetic overflow")),
    }
}

/// Apply `+ - * / div mod` to two classified numeric operands.
///
/// * `/` is AL real division: always `Decimal` (e.g. `Avg := Total / Count`).
/// * `div`/`mod` are integer-only; on any `Decimal` operand they error.
/// * Integer/Integer stays `Integer` (i32 trap); if either side is a
///   `BigInteger` the result is `BigInteger` with an i64 overflow trap.
fn apply_numeric(op: &str, l: Num, r: Num) -> Eval {
    // Real division always promotes to Decimal.
    if op == "/" {
        let (a, b) = (l.to_decimal(), r.to_decimal());
        if b == Decimal::ZERO {
            return Eval::Error(error_info("division by zero"));
        }
        return checked_decimal(a.checked_div(b));
    }
    match (l, r) {
        (Num::Int { val: a, big: ab }, Num::Int { val: b, big: bb }) => {
            let big = ab || bb;
            match op {
                "+" => checked_int(a.checked_add(b), big),
                "-" => checked_int(a.checked_sub(b), big),
                "*" => checked_int(a.checked_mul(b), big),
                "div" => {
                    if b == 0 {
                        Eval::Error(error_info("division by zero"))
                    } else {
                        // i32::MIN / -1 (or i64::MIN / -1) overflows → error.
                        checked_int(a.checked_div(b), big)
                    }
                }
                "mod" => {
                    if b == 0 {
                        Eval::Error(error_info("modulo by zero"))
                    } else {
                        checked_int(a.checked_rem(b), big)
                    }
                }
                _ => unreachable!("op pre-filtered by apply_binary"),
            }
        }
        (l, r) => {
            // At least one Decimal operand → Decimal arithmetic.
            let (a, b) = (l.to_decimal(), r.to_decimal());
            match op {
                "+" => checked_decimal(a.checked_add(b)),
                "-" => checked_decimal(a.checked_sub(b)),
                "*" => checked_decimal(a.checked_mul(b)),
                "div" | "mod" => Eval::Error(error_info(format!(
                    "binary operator `{op}` is integer-only (not supported on Decimal)"
                ))),
                _ => unreachable!("op pre-filtered by apply_binary"),
            }
        }
    }
}

/// Apply a binary operator to two values. Made `pub(crate)` so unit tests
/// so mock dispatch can reuse the operator semantics.
pub(crate) fn apply_binary(operator: &str, left: Value, right: Value) -> Eval {
    let op = operator.to_ascii_lowercase();

    if matches!(op.as_str(), "+" | "-") {
        if let Some(result) = apply_temporal_arithmetic(&op, &left, &right) {
            return result;
        }
    }

    // Numeric arithmetic (Integer, BigInteger, Decimal, and every mix) is
    // handled first, before the value-consuming match, so the operand-type
    // classification lives in one place. Non-arithmetic operations and
    // non-numeric operands fall through to the match below.
    if matches!(op.as_str(), "+" | "-" | "*" | "/" | "div" | "mod") {
        if let (Some(l), Some(r)) = (classify_numeric(&left), classify_numeric(&right)) {
            return apply_numeric(&op, l, r);
        }
    }

    if let ("+", Value::Text(a) | Value::Code(a), Value::Text(b) | Value::Code(b)) =
        (&op[..], &left, &right)
    {
        let size = a.len().saturating_add(b.len());
        if let Err(message) = value::check_text_size("Text concatenation", size) {
            return Eval::Error(error_info(message));
        }
    }
    match (&op[..], left, right) {
        ("+", Value::Text(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Text(a), Value::Code(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Code(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Code(a), Value::Code(b)) => Eval::Normal(Value::Code(format!("{a}{b}"))),

        ("=", a, b) => Eval::Normal(Value::Boolean(values_equal(&a, &b))),
        ("<>", a, b) => Eval::Normal(Value::Boolean(!values_equal(&a, &b))),
        ("<", a, b) => values_cmp(&a, &b, |o| o.is_lt()),
        ("<=", a, b) => values_cmp(&a, &b, |o| o.is_le()),
        (">", a, b) => values_cmp(&a, &b, |o| o.is_gt()),
        (">=", a, b) => values_cmp(&a, &b, |o| o.is_ge()),

        ("and", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a && b)),
        ("or", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a || b)),
        ("xor", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a ^ b)),
        ("..", start, end) => Eval::Normal(Value::Range {
            start: Box::new(start),
            end: Box::new(end),
        }),
        ("in", value, Value::List(members)) => {
            apply_binary("in", value, Value::Array(members.snapshot()))
        }
        ("in", value, Value::Array(members)) => {
            for member in members {
                let matched = match member {
                    Value::Range { start, end } => match value_in_range(&value, &start, &end) {
                        Ok(matched) => matched,
                        Err(error) => return Eval::Error(error),
                    },
                    member => values_equal(&value, &member),
                };
                if matched {
                    return Eval::Normal(Value::Boolean(true));
                }
            }
            Eval::Normal(Value::Boolean(false))
        }

        (op, a, b) => Eval::Error(error_info(format!(
            "binary operator `{op}` not supported on ({}, {})",
            a.type_name(),
            b.type_name()
        ))),
    }
}

/// AL value equality for `=`/`<>` and CASE matching:
/// - `Text = Text` is **case-sensitive**.
/// - `Code = Code` and `Code = Text` are **case-insensitive** (Code is an
///   uppercased, caseless type; a Text on the other side is coerced to Code).
/// - `Integer = Decimal` compares exactly: the Integer is promoted to an
///   exact `Decimal` (infallible — i64 fits the 96-bit range), so `5 = 5.0`
///   holds and `5 = 5.1` does not, with no floating-point rounding.
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    if let (Some(left), Some(right)) = (classify_numeric(a), classify_numeric(b)) {
        return left.to_decimal() == right.to_decimal();
    }
    use Value::*;
    match (a, b) {
        // Code is caseless in BC; a Text compared to a Code is coerced to Code.
        (Code(x), Code(y)) | (Text(x), Code(y)) | (Code(y), Text(x)) => x.eq_ignore_ascii_case(y),
        // Everything else uses structural equality (`Value`'s Eq): Text,
        // Boolean, Decimal, Date/Time/DateTime, Duration, Guid, Char, Option,
        // Null, Empty, … A cross-type non-numeric pair is unequal by variant.
        // (This is what lets `Assert.AreEqual` work on Duration/Guid/Char/Option,
        // which the previous explicit arm list silently treated as never-equal.)
        _ => a == b,
    }
}

fn value_ordering(a: &Value, b: &Value) -> Result<std::cmp::Ordering, ErrorInfo> {
    use Value::*;
    if let (Some(left), Some(right)) = (classify_numeric(a), classify_numeric(b)) {
        return Ok(match (left, right) {
            (Num::Int { val: left, .. }, Num::Int { val: right, .. }) => left.cmp(&right),
            (left, right) => left.to_decimal().cmp(&right.to_decimal()),
        });
    }
    Ok(match (a, b) {
        (Boolean(left), Boolean(right)) => left.cmp(right),
        (Text(x), Text(y)) => x.cmp(y),
        // `Code` is caseless in BC, so relational operators must compare it
        // case-insensitively too — matching `values_equal` and the record
        // filter. A Text/Code mix coerces to caseless Code.
        (Code(x), Code(y)) | (Text(x), Code(y)) | (Code(x), Text(y)) => {
            x.to_ascii_uppercase().cmp(&y.to_ascii_uppercase())
        }
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x.cmp(y),
        (l, r) => {
            return Err(error_info(format!(
                "cannot compare {} and {}",
                l.type_name(),
                r.type_name()
            )));
        }
    })
}

fn values_cmp(a: &Value, b: &Value, predicate: impl Fn(std::cmp::Ordering) -> bool) -> Eval {
    match value_ordering(a, b) {
        Ok(ordering) => Eval::Normal(Value::Boolean(predicate(ordering))),
        Err(error) => Eval::Error(error),
    }
}

/// Inclusive range membership shared by the `in` operator and CASE labels.
pub(crate) fn value_in_range(value: &Value, start: &Value, end: &Value) -> Result<bool, ErrorInfo> {
    Ok(value_ordering(value, start)?.is_ge() && value_ordering(value, end)?.is_le())
}
