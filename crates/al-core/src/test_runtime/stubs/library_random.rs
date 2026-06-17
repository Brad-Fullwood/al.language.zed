//! Library Random (codeunit 130440) — native Rust port.
//!
//! Library Random is a Microsoft test library shipped in the
//! `Microsoft_Test Libraries_*.app` NuGet package.  Its AL source is not
//! available in either the BCApps or ALAppExtensions open-source repos, so
//! this stub is implemented directly from the published API contract, using
//! the same fast-path approach as `library_assert.rs`: the dispatch layer
//! recognises calls by codeunit name / ID and invokes the Rust equivalent.
//!
//! ## Determinism
//!
//! AL tests that use `LibraryRandom` need deterministic output when running
//! under the offline interpreter.  `Random(n)` in BC uses a seeded LCG.
//! We replicate this with a thread-local LCG seeded to 1 by default (the
//! same default BC uses when `Randomize` is not called).  Callers can
//! inject a known seed by calling `SetSeed` (see `resolve`).
//!
//! The LCG parameters mirror the classic Microsoft C runtime:
//!   `state = state * 214013 + 2531011`
//!   `value = (state >> 16) & 0x7FFF`   — gives values in [1, 32767]
//!
//! To satisfy `Random(N)` semantics (returns integer in [1, N]):
//!   `result = (state_value % N) + 1`
//!
//! ## Procedure set
//!
//! | AL procedure          | Description                         |
//! |-----------------------|-------------------------------------|
//! | `RandInt(Max)`        | Integer in \[1, Max\]               |
//! | `RandIntInRange(Min, Max)` | Integer in \[Min, Max\]         |
//! | `RandDec(Max, Places)`| Decimal in \[0.01, Max\] rounded    |
//! | `RandText([Max])`     | Text of up to `Max` (≤250) chars    |
//! | `SetSeed(Seed)`       | Seed the thread-local RNG           |
//! | `RandDateFrom(Date, Max)` | Date in \[Date, Date+Max days\] |

use std::cell::Cell;

use crate::test_runtime::interpreter::scope::Eval;
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};

thread_local! {
    /// Current LCG state.  Initial value 1 matches BC's default when
    /// `Randomize` has not been called.
    static LCG_STATE: Cell<u64> = const { Cell::new(1) };
}

/// Advance the LCG and return the next value in [1, max].
///
/// Uses the classic Microsoft C runtime LCG:
///   state = state × 214013 + 2531011
///   rand  = (state >> 16) & 0x7FFF   →  range [0, 32767]
///
/// The final result is `(rand_value % max) + 1`, giving a value in [1, max].
/// When `max` is 1, this always returns 1.
fn next_rand(max: i64) -> i64 {
    if max <= 1 {
        return 1;
    }
    LCG_STATE.with(|cell| {
        let next = cell.get().wrapping_mul(214013).wrapping_add(2531011);
        cell.set(next);
        let r = ((next >> 16) & 0x7FFF) as i64;
        (r % max) + 1
    })
}

fn err(message: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: message.into(),
        error_type: None,
        source: Some("Library Random".to_string()),
    })
}

fn ok(v: Value) -> Eval {
    Eval::Normal(v)
}

/// `LibraryRandom.RandInt(MaxValue: Integer): Integer`
///
/// Returns a pseudo-random integer in [1, MaxValue].
/// If MaxValue < 1, returns 1 (matches BC behaviour).
pub fn rand_int(args: &[Value]) -> Eval {
    let max = match args {
        [Value::Integer(n)] => *n,
        [] => return err("LibraryRandom.RandInt requires 1 argument"),
        _ => return err("LibraryRandom.RandInt expects (Integer)"),
    };
    if max < 1 {
        return ok(Value::Integer(1));
    }
    ok(Value::Integer(next_rand(max)))
}

/// `LibraryRandom.RandIntInRange(MinValue: Integer; MaxValue: Integer): Integer`
///
/// Returns a pseudo-random integer in [MinValue, MaxValue].
/// If MinValue ≥ MaxValue, returns MinValue.
pub fn rand_int_in_range(args: &[Value]) -> Eval {
    let (min, max) = match args {
        [Value::Integer(a), Value::Integer(b)] => (*a, *b),
        _ => return err("LibraryRandom.RandIntInRange expects (Integer, Integer)"),
    };
    if min >= max {
        return ok(Value::Integer(min));
    }
    let span = max - min + 1;
    ok(Value::Integer(min + next_rand(span) - 1))
}

/// `LibraryRandom.RandDec(MaxValue: Integer; DecimalPlaces: Integer): Decimal`
///
/// Returns a pseudo-random decimal in the range (0, MaxValue] with the
/// specified number of decimal places.  Replicates the BC implementation:
///   integer part = RandInt(MaxValue × 10^Places), then divide by 10^Places.
pub fn rand_dec(args: &[Value]) -> Eval {
    let (max_val, places) = match args {
        [Value::Integer(m), Value::Integer(p)] => (*m, *p),
        [Value::Decimal(m), Value::Integer(p)] => (*m as i64, *p),
        _ => return err("LibraryRandom.RandDec expects (Integer, Integer)"),
    };
    if places < 0 {
        return err("LibraryRandom.RandDec: DecimalPlaces must be ≥ 0");
    }
    if places > 9 {
        return err("LibraryRandom.RandDec: DecimalPlaces must be ≤ 9");
    }
    let pow = 10_i64.pow(places as u32);
    let scaled_max = max_val.saturating_mul(pow).max(1);
    let raw = next_rand(scaled_max);
    ok(Value::Decimal(raw as f64 / pow as f64))
}

/// `LibraryRandom.RandText([MaxLength: Integer]): Text`
///
/// Returns a pseudo-random lowercase alphabetic text string of length
/// `RandInt(MaxLength.min(250))`.  Default MaxLength is 30 when omitted
/// (matches Library Random's typical default of up to 30 chars).
pub fn rand_text(args: &[Value]) -> Eval {
    let max_len: i64 = match args {
        [] => 30,
        [Value::Integer(n)] => (*n).clamp(1, 250),
        _ => return err("LibraryRandom.RandText expects ([Integer])"),
    };
    let length = next_rand(max_len) as usize;
    let s: String = (0..length)
        .map(|_| {
            let code = 97 + (next_rand(26) - 1); // 97 = 'a'
            char::from_u32(code as u32).unwrap_or('a')
        })
        .collect();
    ok(Value::Text(s))
}

/// Reset the thread-local LCG to its initial state (seed 1, matching BC
/// behaviour before `Randomize` is called). Called between test runs so
/// one test's `SetSeed(42)` doesn't bleed into the next test's
/// expectations on the same thread.
pub fn reset_lcg() {
    LCG_STATE.with(|cell| cell.set(1));
}

/// `LibraryRandom.SetSeed(Seed: Integer)`
///
/// Seeds the thread-local RNG.  A seed of 0 is treated as 1 (BC convention).
pub fn set_seed(args: &[Value]) -> Eval {
    let seed = match args {
        [Value::Integer(n)] => *n as u64,
        [] => return err("LibraryRandom.SetSeed requires 1 argument"),
        _ => return err("LibraryRandom.SetSeed expects (Integer)"),
    };
    let effective = if seed == 0 { 1 } else { seed };
    LCG_STATE.with(|cell| cell.set(effective));
    ok(Value::Empty)
}

/// `LibraryRandom.RandDateFrom(StartDate: Date; MaxNumberOfDays: Integer): Date`
///
/// Returns a pseudo-random date in [StartDate, StartDate + MaxNumberOfDays].
pub fn rand_date_from(args: &[Value]) -> Eval {
    let (start, max_days) = match args {
        [Value::Date(d), Value::Integer(n)] => (*d, *n),
        _ => return err("LibraryRandom.RandDateFrom expects (Date, Integer)"),
    };
    if max_days <= 0 {
        return ok(Value::Date(start));
    }
    let offset = next_rand(max_days + 1) - 1;
    ok(Value::Date(start + offset))
}

pub fn resolve(procedure: &str) -> Option<fn(&[Value]) -> Eval> {
    match procedure.to_ascii_lowercase().as_str() {
        "randint" => Some(rand_int),
        "randinginrange" | "randintinrange" => Some(rand_int_in_range),
        "randdec" => Some(rand_dec),
        "randtext" => Some(rand_text),
        "setseed" => Some(set_seed),
        "randdatefrom" => Some(rand_date_from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(n: u64) {
        LCG_STATE.with(|c| c.set(n));
    }

    fn ok_val(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Exit(v) => v,
        }
    }

    fn is_err(eval: Eval) -> String {
        match eval {
            Eval::Error(e) => e.message,
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn rand_int_returns_value_in_range() {
        seed(1);
        for _ in 0..50 {
            match rand_int(&[Value::Integer(10)]) {
                Eval::Normal(Value::Integer(n)) => {
                    assert!((1..=10).contains(&n), "out of range: {n}")
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn rand_int_seeded_is_deterministic() {
        seed(42);
        let a = ok_val(rand_int(&[Value::Integer(100)]));
        seed(42);
        let b = ok_val(rand_int(&[Value::Integer(100)]));
        assert_eq!(a, b, "same seed must produce same result");
    }

    #[test]
    fn rand_int_max_below_one_returns_one() {
        assert_eq!(
            ok_val(rand_int(&[Value::Integer(0)])),
            Value::Integer(1),
            "max < 1 should return 1"
        );
        assert_eq!(ok_val(rand_int(&[Value::Integer(-5)])), Value::Integer(1));
    }

    #[test]
    fn rand_int_no_args_is_error() {
        let msg = is_err(rand_int(&[]));
        assert!(msg.contains("requires"), "got: {msg}");
    }

    #[test]
    fn rand_int_wrong_arg_type_is_error() {
        let msg = is_err(rand_int(&[Value::Text("bad".into())]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn rand_int_in_range_returns_value_in_range() {
        seed(7);
        for _ in 0..50 {
            match rand_int_in_range(&[Value::Integer(5), Value::Integer(15)]) {
                Eval::Normal(Value::Integer(n)) => {
                    assert!((5..=15).contains(&n), "out of range: {n}")
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn rand_int_in_range_min_eq_max_returns_min() {
        let result = ok_val(rand_int_in_range(&[Value::Integer(7), Value::Integer(7)]));
        assert_eq!(result, Value::Integer(7));
    }

    #[test]
    fn rand_int_in_range_wrong_args_is_error() {
        let msg = is_err(rand_int_in_range(&[Value::Integer(1)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn rand_dec_returns_decimal_in_range() {
        seed(99);
        for _ in 0..50 {
            match rand_dec(&[Value::Integer(100), Value::Integer(2)]) {
                Eval::Normal(Value::Decimal(d)) => {
                    assert!(d > 0.0 && d <= 100.0, "decimal out of range (0, 100]: {d}");
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn rand_dec_seeded_is_deterministic() {
        seed(11);
        let a = ok_val(rand_dec(&[Value::Integer(50), Value::Integer(2)]));
        seed(11);
        let b = ok_val(rand_dec(&[Value::Integer(50), Value::Integer(2)]));
        assert_eq!(a, b, "same seed must produce same decimal");
    }

    #[test]
    fn rand_dec_negative_places_is_error() {
        let msg = is_err(rand_dec(&[Value::Integer(10), Value::Integer(-1)]));
        assert!(msg.contains("≥ 0"), "got: {msg}");
    }

    #[test]
    fn rand_dec_excessive_places_is_error() {
        let msg = is_err(rand_dec(&[Value::Integer(10), Value::Integer(10)]));
        assert!(msg.contains("≤ 9"), "got: {msg}");
    }

    #[test]
    fn rand_text_returns_alphabetic_string() {
        seed(5);
        match rand_text(&[Value::Integer(10)]) {
            Eval::Normal(Value::Text(s)) => {
                assert!(!s.is_empty() && s.len() <= 10, "unexpected length: {s:?}");
                assert!(
                    s.chars().all(|c| c.is_ascii_lowercase()),
                    "expected lowercase alpha, got: {s:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rand_text_seeded_is_deterministic() {
        seed(77);
        let a = ok_val(rand_text(&[Value::Integer(20)]));
        seed(77);
        let b = ok_val(rand_text(&[Value::Integer(20)]));
        assert_eq!(a, b, "same seed must produce same text");
    }

    #[test]
    fn rand_text_no_args_uses_default_max() {
        seed(3);
        match rand_text(&[]) {
            Eval::Normal(Value::Text(s)) => {
                assert!(s.len() <= 30, "default max is 30, got len {}", s.len())
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rand_text_wrong_args_is_error() {
        let msg = is_err(rand_text(&[Value::Integer(1), Value::Integer(2)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn set_seed_changes_output() {
        seed(1);
        let before = ok_val(rand_int(&[Value::Integer(1000)]));

        ok_val(set_seed(&[Value::Integer(12345)]));
        let after = ok_val(rand_int(&[Value::Integer(1000)]));

        // With different seeds the first output should differ (overwhelmingly
        // likely — fail rate 1 in 1000).
        assert_ne!(
            before, after,
            "different seeds should produce different output"
        );
    }

    #[test]
    fn set_seed_zero_treated_as_one() {
        ok_val(set_seed(&[Value::Integer(0)]));
        let r0 = ok_val(rand_int(&[Value::Integer(100)]));

        seed(1);
        let r1 = ok_val(rand_int(&[Value::Integer(100)]));

        assert_eq!(r0, r1, "seed 0 should behave like seed 1");
    }

    #[test]
    fn set_seed_no_args_is_error() {
        let msg = is_err(set_seed(&[]));
        assert!(msg.contains("requires"), "got: {msg}");
    }

    #[test]
    fn rand_date_from_returns_date_in_range() {
        seed(22);
        let start = 1000i64; // arbitrary AL day offset
        let max_days = 30i64;
        match rand_date_from(&[Value::Date(start), Value::Integer(max_days)]) {
            Eval::Normal(Value::Date(d)) => {
                assert!(
                    d >= start && d <= start + max_days,
                    "date {d} not in [{start}, {}]",
                    start + max_days
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rand_date_from_zero_days_returns_start() {
        let start = 500i64;
        let result = ok_val(rand_date_from(&[Value::Date(start), Value::Integer(0)]));
        assert_eq!(result, Value::Date(start));
    }

    #[test]
    fn rand_date_from_wrong_args_is_error() {
        let msg = is_err(rand_date_from(&[Value::Integer(100), Value::Integer(30)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert!(resolve("RandInt").is_some());
        assert!(resolve("RANDINT").is_some());
        assert!(resolve("randint").is_some());
        assert!(resolve("RandDec").is_some());
        assert!(resolve("SetSeed").is_some());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(resolve("DoesNotExist").is_none());
        assert!(resolve("").is_none());
    }
}
