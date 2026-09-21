//! Property tests for the `Round` builtin against integer reference arithmetic.
//!
//! BC's `Round(Number, Precision, Direction)` returns the multiple of `Precision`
//! nearest `Number` in the requested direction. The reference here computes that with
//! scaled integers, which have no rounding of their own, so a disagreement is the
//! builtin's.
//!
//! Case count follows `PROPTEST_CASES` (default 128).

use al_runtime::interpreter::dispatch::dispatch_call;
use al_runtime::interpreter::DispatchCtx;
use al_runtime::{Eval, Value};
use proptest::prelude::*;
use rust_decimal::Decimal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `Round` reads nothing from the workspace, so an empty procedure source is enough.
struct NoSource;

impl al_types::ProcedureSource for NoSource {
    fn find_by_object_name(&self, _name: &str) -> Option<PathBuf> {
        None
    }
    fn iter_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn get_cached_parse(&self, _p: &Path) -> Option<(String, tree_sitter::Tree)> {
        None
    }
    fn object_name(&self, _p: &Path) -> Option<String> {
        None
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        ..ProptestConfig::default()
    }
}

fn round(number: Decimal, precision: Decimal, direction: &str) -> Result<Decimal, String> {
    let mut ctx = DispatchCtx::new_pure(Arc::new(NoSource));
    let args = vec![
        Value::Decimal(number),
        Value::Decimal(precision),
        Value::Text(direction.to_string()),
    ];
    match dispatch_call(None, "Round", args, &mut ctx) {
        Eval::Normal(Value::Decimal(d)) => Ok(d),
        Eval::Error(e) => Err(e.message),
        other => Err(format!("unexpected result {other:?}")),
    }
}

/// `number` and `precision` as integers scaled by 10^`scale`, so the reference can work
/// in `i128` without any rounding of its own. The wide arm reaches quotients around
/// 10^18, where a 96-bit decimal division starts to matter.
fn scaled() -> impl Strategy<Value = (i128, i128, u32)> {
    prop_oneof![
        3 => (-1_000_000i128..1_000_000, 1i128..10_000, 0u32..5),
        1 => (-1_000_000_000_000_000_000i128..1_000_000_000_000_000_000, 1i128..1_000_000, 0u32..12),
    ]
}

fn dec(units: i128, scale: u32) -> Decimal {
    Decimal::from_i128_with_scale(units, scale)
}

/// The multiple of `p` nearest `n`, both as integers, in the given direction. BC moves
/// the magnitude, not the signed value, so all three directions work on `|n|`.
fn reference(n: i128, p: i128, direction: &str) -> i128 {
    let (mag, sign) = (n.abs(), n.signum());
    let (q, r) = (mag / p, mag % p);
    let steps = match direction {
        "<" => q,
        ">" => {
            if r == 0 {
                q
            } else {
                q + 1
            }
        }
        // Nearest, midpoints away from zero.
        _ => {
            if r * 2 >= p {
                q + 1
            } else {
                q
            }
        }
    };
    steps * p * sign
}

proptest! {
    #![proptest_config(config())]

    /// `Round` agrees with integer arithmetic at every direction.
    #[test]
    fn round_agrees_with_integer_reference(
        (n, p, scale) in scaled(),
        direction in prop::sample::select(vec!["=", "<", ">"]),
    ) {
        let number = dec(n, scale);
        let precision = dec(p, scale);
        let got = round(number, precision, direction);
        let want = dec(reference(n, p, direction), scale);
        prop_assert_eq!(
            got.as_ref().map(Decimal::normalize).map_err(String::as_str),
            Ok(want.normalize()),
            "Round({}, {}, '{}')",
            number, precision, direction
        );
    }

    /// The result is an exact multiple of the precision.
    #[test]
    fn result_is_a_multiple_of_the_precision(
        (n, p, scale) in scaled(),
        direction in prop::sample::select(vec!["=", "<", ">"]),
    ) {
        let (number, precision) = (dec(n, scale), dec(p, scale));
        let Ok(got) = round(number, precision, direction) else { return Ok(()); };
        let quotient = got / precision;
        prop_assert_eq!(
            quotient.normalize().fract(),
            Decimal::ZERO,
            "Round({}, {}, '{}') = {} is not a multiple of the precision",
            number, precision, direction, got
        );
    }

    /// The result never moves further than one precision step.
    #[test]
    fn result_stays_within_one_precision_step(
        (n, p, scale) in scaled(),
        direction in prop::sample::select(vec!["=", "<", ">"]),
    ) {
        let (number, precision) = (dec(n, scale), dec(p, scale));
        let Ok(got) = round(number, precision, direction) else { return Ok(()); };
        prop_assert!(
            (got - number).abs() < precision,
            "Round({}, {}, '{}') = {} moved by {} >= {}",
            number, precision, direction, got, (got - number).abs(), precision
        );
    }

    /// Rounding an already-rounded value changes nothing.
    #[test]
    fn round_is_idempotent(
        (n, p, scale) in scaled(),
        direction in prop::sample::select(vec!["=", "<", ">"]),
    ) {
        let (number, precision) = (dec(n, scale), dec(p, scale));
        let Ok(once) = round(number, precision, direction) else { return Ok(()); };
        let Ok(twice) = round(once, precision, direction) else { return Ok(()); };
        prop_assert_eq!(
            once.normalize(),
            twice.normalize(),
            "Round({}, {}, '{}') is not idempotent",
            number, precision, direction
        );
    }

    /// Every direction is odd in the sign of the number: `Round(-x) == -Round(x)`.
    #[test]
    fn round_is_sign_symmetric(
        (n, p, scale) in scaled(),
        direction in prop::sample::select(vec!["=", "<", ">"]),
    ) {
        let (number, precision) = (dec(n, scale), dec(p, scale));
        let Ok(pos) = round(number, precision, direction) else { return Ok(()); };
        let Ok(neg) = round(-number, precision, direction) else { return Ok(()); };
        prop_assert_eq!(
            pos.normalize(),
            (-neg).normalize(),
            "Round({}, {}, '{}') and its negation disagree",
            number, precision, direction
        );
    }

    /// `<` never increases the magnitude, `>` never decreases it.
    #[test]
    fn directions_move_the_magnitude_the_documented_way((n, p, scale) in scaled()) {
        let (number, precision) = (dec(n, scale), dec(p, scale));
        if let Ok(down) = round(number, precision, "<") {
            prop_assert!(
                down.abs() <= number.abs(),
                "'<' raised the magnitude of {}: {}",
                number, down
            );
        }
        if let Ok(up) = round(number, precision, ">") {
            prop_assert!(
                up.abs() >= number.abs(),
                "'>' lowered the magnitude of {}: {}",
                number, up
            );
        }
    }

    /// A precision of zero or below is rejected, not silently accepted.
    #[test]
    fn non_positive_precision_is_an_error(n in -1000i64..1000, p in -1000i64..=0) {
        let r = round(Decimal::from(n), Decimal::from(p), "=");
        prop_assert!(r.is_err(), "Round with precision {} was accepted", p);
    }

    /// An unknown direction is rejected.
    #[test]
    fn unknown_direction_is_an_error(
        n in -1000i64..1000,
        direction in prop::sample::select(vec!["", "x", "==", "<>", "up", "≥"]),
    ) {
        let r = round(Decimal::from(n), Decimal::ONE, direction);
        prop_assert!(r.is_err(), "direction {:?} was accepted", direction);
    }
}
