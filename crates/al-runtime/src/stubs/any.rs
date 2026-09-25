//! Native Any test-codeunit procedures.

use std::cell::Cell;

use crate::interpreter::scope::Eval;
use crate::interpreter::value::{Decimal, ErrorInfo, Value};
use rust_decimal::prelude::ToPrimitive;

/// Longest text these generators will build.
///
/// The AL originals loop `for i := 1 to Length` into an unbounded `Text`, so
/// `Any.AlphabeticText(2000000000)` asked for a 2 GB allocation here while BC
/// stops at the variable's declared length. 2048 is the largest a Text or Code
/// table field can declare (Microsoft Learn, Code data type).
const MAX_TEXT_LENGTH: i64 = 2048;

thread_local! {
    /// The seed `SetSeed` last installed.
    ///
    /// Codeunit 130500 keeps `Seed: Integer` next to the RNG and `GetSeed`
    /// returns that field, not the generator's state, so the value survives
    /// every draw. 1 is the resting value because `GetNextValue` calls
    /// `SetSeed(1)` on first use; BC returns 0 before the very first draw,
    /// which is the one case this does not reproduce.
    static SEED: Cell<i64> = const { Cell::new(1) };
}

/// Reject a text length that is negative or past [`MAX_TEXT_LENGTH`].
fn checked_text_length(procedure: &str, length: i64) -> Result<usize, Eval> {
    if length < 0 {
        return Err(err(format!("Any.{procedure}: Length must be \u{2265} 0")));
    }
    if length > MAX_TEXT_LENGTH {
        return Err(err(format!(
            "Any.{procedure}: Length {length} exceeds the {MAX_TEXT_LENGTH}-character maximum"
        )));
    }
    Ok(length as usize)
}

fn next_rand(max: i64) -> i64 {
    super::library_random::next_rand(max)
}

fn err(message: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: message.into(),
        error_type: None,
        source: Some("Any".to_string()),
    })
}

fn ok(v: Value) -> Eval {
    Eval::Normal(v)
}

/// `Any.Boolean(): Boolean`
///
/// Replicates `GetNextValue(2) = 2` — true roughly half the time.
pub fn boolean(args: &[Value]) -> Eval {
    if !args.is_empty() {
        return err("Any.Boolean expects no arguments");
    }
    ok(Value::Boolean(next_rand(2) == 2))
}

/// `Any.IntegerInRange(MaxValue: Integer): Integer`   — single-arg overload
/// `Any.IntegerInRange(Min: Integer; Max: Integer): Integer` — two-arg overload
pub fn integer_in_range(args: &[Value]) -> Eval {
    match args {
        [Value::Integer(max) | Value::BigInteger(max)] => {
            let m = *max;
            if m < 1 {
                return ok(Value::Integer(1));
            }
            ok(Value::Integer(next_rand(m)))
        }
        [Value::Integer(min) | Value::BigInteger(min), Value::Integer(max) | Value::BigInteger(max)] =>
        {
            let (a, b) = (*min, *max);
            if a >= b {
                return ok(Value::Integer(a));
            }
            let span = b - a + 1;
            ok(Value::Integer(a + next_rand(span) - 1))
        }
        _ => err("Any.IntegerInRange expects (Integer) or (Integer, Integer)"),
    }
}

/// `Any.DecimalInRange(MaxValue: Integer; DecimalPlaces: Integer): Decimal`
/// `Any.DecimalInRange(Min: Integer; Max: Integer; DecimalPlaces: Integer): Decimal`
/// `Any.DecimalInRange(Min: Decimal; Max: Decimal; DecimalPlaces: Integer): Decimal`
///
/// The two-argument overload declares `MaxValue: Integer`, but AL converts a
/// Decimal argument to it, so `Any.DecimalInRange(MaxAmount, 2)` with a
/// Decimal `MaxAmount` compiles and runs on BC. It used to error here.
pub fn decimal_in_range(args: &[Value]) -> Eval {
    match args {
        // Two-arg form: DecimalInRange(Max, Places)
        [max, Value::Integer(places) | Value::BigInteger(places)] => {
            let Some(max) = numeric_bound(max) else {
                return err("Any.DecimalInRange expects (Num, Integer) or (Num, Num, Integer)");
            };
            decimal_in_range_impl(Decimal::ZERO, max, *places)
        }
        // Three-arg forms, in any Integer/BigInteger/Decimal mix.
        [min, max, Value::Integer(places) | Value::BigInteger(places)] => {
            let (Some(min), Some(max)) = (numeric_bound(min), numeric_bound(max)) else {
                return err("Any.DecimalInRange expects (Num, Integer) or (Num, Num, Integer)");
            };
            decimal_in_range_impl(min, max, *places)
        }
        _ => err("Any.DecimalInRange expects (Num, Integer) or (Num, Num, Integer)"),
    }
}

/// A numeric range bound: Integer, BigInteger or Decimal.
fn numeric_bound(value: &Value) -> Option<Decimal> {
    match value {
        Value::Integer(n) | Value::BigInteger(n) => Some(Decimal::from(*n)),
        Value::Decimal(d) => Some(*d),
        _ => None,
    }
}

fn decimal_in_range_impl(min: Decimal, max: Decimal, places: i64) -> Eval {
    if places < 0 {
        return err("Any.DecimalInRange: DecimalPlaces must be ≥ 0");
    }
    if places > 9 {
        return err("Any.DecimalInRange: DecimalPlaces must be ≤ 9");
    }
    let scale = places as u32;
    let pow = Decimal::from(10_i64.pow(scale));
    // Scale the bounds to whole units of the least significant place, then pick
    // an integer in [min_scaled, max_scaled] and divide back exactly.
    // A bound (or the span) that overflows the decimal/i64 range at this scale
    // is reported rather than panicking (Decimal `*` panics on overflow) or
    // being silently truncated to 0 (which would return an out-of-range value).
    let (Some(min_scaled), Some(max_scaled)) = (
        min.checked_mul(pow).and_then(|v| v.ceil().to_i64()),
        max.checked_mul(pow).and_then(|v| v.floor().to_i64()),
    ) else {
        return err("Any.DecimalInRange: range too large for the requested DecimalPlaces");
    };
    if min_scaled >= max_scaled {
        return ok(Value::Decimal(Decimal::new(min_scaled, scale)));
    }
    let Some(span) = max_scaled
        .checked_sub(min_scaled)
        .and_then(|d| d.checked_add(1))
    else {
        return err("Any.DecimalInRange: range too large for the requested DecimalPlaces");
    };
    let raw = next_rand(span);
    // raw ∈ [1, span]; min_scaled + raw - 1 ∈ [min_scaled, max_scaled], in range.
    ok(Value::Decimal(Decimal::new(min_scaled + raw - 1, scale)))
}

/// `Any.AlphabeticText(Length: Integer): Text`
///
/// Returns a lowercase a–z string of exactly `Length` characters.
/// Replicates the AL loop:
///   `Number := IntegerInRange(97, 122); TextValue[i] := Number;`
pub fn alphabetic_text(args: &[Value]) -> Eval {
    let length = match args {
        [Value::Integer(n) | Value::BigInteger(n)] => *n,
        _ => return err("Any.AlphabeticText expects (Integer)"),
    };
    let length = match checked_text_length("AlphabeticText", length) {
        Ok(length) => length,
        Err(error) => return error,
    };
    let s: String = (0..length)
        .map(|_| {
            // IntegerInRange(97, 122): span = 26
            let code = 96 + next_rand(26); // 97 = 'a'
            char::from_u32(code as u32).unwrap_or('a')
        })
        .collect();
    ok(Value::Text(s))
}

/// `Any.AlphanumericText(Length: Integer): Text`
///
/// Lowercase hex. The AL original loops
/// `GuidTxt += LowerCase(DelChr(Format(GuidValue()), '=', '{}-'))`, so
/// stripping the braces and dashes out of a GUID leaves 0-9 and a-f only.
pub fn alphanumeric_text(args: &[Value]) -> Eval {
    let length = match args {
        [Value::Integer(n) | Value::BigInteger(n)] => *n,
        _ => return err("Any.AlphanumericText expects (Integer)"),
    };
    let length = match checked_text_length("AlphanumericText", length) {
        Ok(length) => length,
        Err(error) => return error,
    };
    const HEX: &[u8] = b"0123456789abcdef";
    let s: String = (0..length)
        .map(|_| HEX[(next_rand(16) - 1) as usize] as char)
        .collect();
    ok(Value::Text(s))
}

/// `Any.UnicodeText(Length: Integer): Text`
///
/// Returns a string of `Length` Cyrillic characters (U+0430–U+045F),
/// replicating the AL `IntegerInRange(1072, 1103)` loop.
pub fn unicode_text(args: &[Value]) -> Eval {
    let length = match args {
        [Value::Integer(n) | Value::BigInteger(n)] => *n,
        _ => return err("Any.UnicodeText expects (Integer)"),
    };
    let length = match checked_text_length("UnicodeText", length) {
        Ok(length) => length,
        Err(error) => return error,
    };
    // Cyrillic code points 0x0430 (1072) to 0x044F (1103): span = 32.
    let s: String = (0..length)
        .map(|_| {
            let code = 1071 + next_rand(32); // 1072 = 'а'
            char::from_u32(code as u32).unwrap_or('а')
        })
        .collect();
    ok(Value::Text(s))
}

/// `Any.Email(): Text`
/// `Any.Email(LocalPartLength: Integer; DomainLength: Integer): Text`
///
/// Returns a pseudo-random email address.
/// Default lengths are 20+20 (matching the AL source).
pub fn email(args: &[Value]) -> Eval {
    let (local_len, domain_len) = match args {
        [] => (20i64, 20i64),
        [Value::Integer(l) | Value::BigInteger(l), Value::Integer(d) | Value::BigInteger(d)] => {
            (*l, *d)
        }
        _ => return err("Any.Email expects () or (Integer, Integer)"),
    };
    if local_len < 1 || domain_len < 1 {
        return err("Any.Email: lengths must be ≥ 1");
    }
    if local_len > MAX_TEXT_LENGTH || domain_len > MAX_TEXT_LENGTH {
        return err(format!("Any.Email: lengths must be ≤ {MAX_TEXT_LENGTH}"));
    }
    let local: String = (0..local_len as usize)
        .map(|_| {
            const HEX: &[u8] = b"0123456789abcdef";
            HEX[(next_rand(16) - 1) as usize] as char
        })
        .collect();
    let domain: String = (0..domain_len as usize)
        .map(|_| char::from_u32((96 + next_rand(26)) as u32).unwrap_or('a'))
        .collect();
    let tld: String = (0..3)
        .map(|_| char::from_u32((96 + next_rand(26)) as u32).unwrap_or('a'))
        .collect();
    ok(Value::Text(format!("{local}@{domain}.{tld}")))
}

/// `Any.GuidValue(): Guid`
///
/// Returns a random GUID string. Uses the OS entropy via `getrandom`.
/// Note: AL `GuidValue()` explicitly states it is NOT seeded (random, not
/// pseudo-random), so we do not use the LCG here.
pub fn guid_value(args: &[Value]) -> Eval {
    if !args.is_empty() {
        return err("Any.GuidValue expects no arguments");
    }
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        return err("Any.GuidValue: failed to obtain random bytes");
    }
    // RFC 4122 version 4 / variant bits
    buf[6] = (buf[6] & 0x0F) | 0x40;
    buf[8] = (buf[8] & 0x3F) | 0x80;
    let guid = format!(
        "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        buf[0], buf[1], buf[2], buf[3],
        buf[4], buf[5],
        buf[6], buf[7],
        buf[8], buf[9],
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15]
    );
    ok(Value::Guid(guid))
}

/// `Any.SetSeed(NewSeed: Integer)`
///
/// Seeds the thread-local RNG.  Seed 0 → treated as 1 (BC convention).
pub fn set_seed(args: &[Value]) -> Eval {
    let seed = match args {
        [Value::Integer(n) | Value::BigInteger(n)] => *n,
        [] => return err("Any.SetSeed requires 1 argument"),
        _ => return err("Any.SetSeed expects (Integer)"),
    };
    SEED.with(|cell| cell.set(seed));
    super::library_random::set_lcg_seed(seed as u64);
    ok(Value::Empty)
}

/// `Any.GetSeed(): Integer`
///
/// Returns the seed `SetSeed` installed, unchanged by the draws since. The
/// generator's state advances on every call and is not the seed.
pub fn get_seed(args: &[Value]) -> Eval {
    if !args.is_empty() {
        return err("Any.GetSeed expects no arguments");
    }
    ok(Value::Integer(SEED.with(|cell| cell.get())))
}

/// `Any.SetDefaultSeed()`
///
pub fn set_default_seed(args: &[Value]) -> Eval {
    if !args.is_empty() {
        return err("Any.SetDefaultSeed expects no arguments");
    }
    let seed = crate::interpreter::dispatch::clock_time();
    SEED.with(|cell| cell.set(seed));
    super::library_random::set_lcg_seed(seed as u64);
    ok(Value::Empty)
}

/// Resolve a procedure name (case-insensitive) to its Rust implementation.
pub fn resolve(procedure: &str) -> Option<fn(&[Value]) -> Eval> {
    match procedure.to_ascii_lowercase().as_str() {
        "boolean" => Some(boolean),
        "integerinrange" => Some(integer_in_range),
        "decimalinrange" => Some(decimal_in_range),
        "alphabetictext" => Some(alphabetic_text),
        "alphanumerictext" => Some(alphanumeric_text),
        "unicodetext" => Some(unicode_text),
        "email" => Some(email),
        "guidvalue" => Some(guid_value),
        "setseed" => Some(set_seed),
        "getseed" => Some(get_seed),
        "setdefaultseed" => Some(set_default_seed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(n: u64) {
        super::super::library_random::set_lcg_seed(n);
    }

    fn ok_val(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Exit(v) => v,
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    fn is_err(eval: Eval) -> String {
        match eval {
            Eval::Error(e) => e.message,
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn boolean_returns_bool() {
        seed(1);
        for _ in 0..20 {
            match boolean(&[]) {
                Eval::Normal(Value::Boolean(_)) => {}
                other => panic!("expected Boolean, got {other:?}"),
            }
        }
    }

    #[test]
    fn boolean_is_deterministic_with_seed() {
        seed(42);
        let a = ok_val(boolean(&[]));
        seed(42);
        let b = ok_val(boolean(&[]));
        assert_eq!(a, b, "same seed must produce same boolean");
    }

    #[test]
    fn boolean_no_args_required() {
        let msg = is_err(boolean(&[Value::Integer(1)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn integer_in_range_single_arg_in_range() {
        seed(3);
        for _ in 0..50 {
            match integer_in_range(&[Value::Integer(10)]) {
                Eval::Normal(Value::Integer(n)) => {
                    assert!((1..=10).contains(&n), "out of range: {n}")
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn integer_in_range_two_args_in_range() {
        seed(9);
        for _ in 0..50 {
            match integer_in_range(&[Value::Integer(5), Value::Integer(15)]) {
                Eval::Normal(Value::Integer(n)) => {
                    assert!((5..=15).contains(&n), "out of range: {n}")
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn integer_in_range_seeded_is_deterministic() {
        seed(55);
        let a = ok_val(integer_in_range(&[Value::Integer(100)]));
        seed(55);
        let b = ok_val(integer_in_range(&[Value::Integer(100)]));
        assert_eq!(a, b);
    }

    #[test]
    fn integer_in_range_max_below_one_returns_one() {
        assert_eq!(
            ok_val(integer_in_range(&[Value::Integer(0)])),
            Value::Integer(1)
        );
    }

    #[test]
    fn integer_in_range_wrong_args_is_error() {
        let msg = is_err(integer_in_range(&[Value::Boolean(true)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn decimal_in_range_two_args_in_range() {
        seed(17);
        for _ in 0..50 {
            match decimal_in_range(&[Value::Integer(100), Value::Integer(2)]) {
                Eval::Normal(Value::Decimal(d)) => {
                    assert!(
                        d > Decimal::ZERO && d <= Decimal::from(100),
                        "out of range: {d}"
                    )
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn decimal_in_range_three_args_in_range() {
        seed(19);
        for _ in 0..50 {
            match decimal_in_range(&[Value::Integer(10), Value::Integer(20), Value::Integer(2)]) {
                Eval::Normal(Value::Decimal(d)) => {
                    assert!(
                        d >= Decimal::from(10) && d <= Decimal::from(20),
                        "out of range: {d}"
                    )
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }

    #[test]
    fn decimal_in_range_seeded_is_deterministic() {
        seed(33);
        let a = ok_val(decimal_in_range(&[Value::Integer(50), Value::Integer(2)]));
        seed(33);
        let b = ok_val(decimal_in_range(&[Value::Integer(50), Value::Integer(2)]));
        assert_eq!(a, b);
    }

    #[test]
    fn decimal_in_range_negative_places_is_error() {
        let msg = is_err(decimal_in_range(&[Value::Integer(10), Value::Integer(-1)]));
        assert!(msg.contains("≥ 0"), "got: {msg}");
    }

    #[test]
    fn alphabetic_text_returns_correct_length() {
        seed(13);
        match alphabetic_text(&[Value::Integer(5)]) {
            Eval::Normal(Value::Text(s)) => {
                assert_eq!(s.len(), 5, "expected length 5, got {}", s.len());
                assert!(
                    s.chars().all(|c| c.is_ascii_lowercase()),
                    "expected lowercase alpha: {s:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn alphabetic_text_seeded_is_deterministic() {
        seed(66);
        let a = ok_val(alphabetic_text(&[Value::Integer(10)]));
        seed(66);
        let b = ok_val(alphabetic_text(&[Value::Integer(10)]));
        assert_eq!(a, b, "same seed must produce same text");
    }

    #[test]
    fn alphabetic_text_wrong_args_is_error() {
        let msg = is_err(alphabetic_text(&[Value::Integer(5), Value::Integer(10)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    /// The AL signature takes an Integer, and AL converts a Decimal argument
    /// to it, so the two-argument call with a Decimal max runs on BC.
    #[test]
    fn decimal_in_range_two_args_accepts_a_decimal_max() {
        seed(7);
        match decimal_in_range(&[Value::Decimal(Decimal::from(100)), Value::Integer(2)]) {
            Eval::Normal(Value::Decimal(d)) => {
                assert!(
                    d >= Decimal::ZERO && d <= Decimal::from(100),
                    "out of range: {d}"
                );
            }
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn numeric_arguments_accept_big_integer() {
        seed(7);
        assert!(matches!(
            integer_in_range(&[Value::BigInteger(10)]),
            Eval::Normal(Value::Integer(_))
        ));
        assert!(matches!(
            decimal_in_range(&[Value::BigInteger(10), Value::BigInteger(2)]),
            Eval::Normal(Value::Decimal(_))
        ));
        assert!(matches!(
            alphabetic_text(&[Value::BigInteger(4)]),
            Eval::Normal(Value::Text(_))
        ));
    }

    /// The AL loop writes into a sized Text, so BC stops at its length. These
    /// generators used to allocate whatever was asked for: AlphabeticText with
    /// two billion took the runner down.
    #[test]
    fn a_text_length_past_the_cap_is_an_error_not_an_allocation() {
        seed(7);
        for procedure in [alphabetic_text, alphanumeric_text, unicode_text] {
            let msg = is_err(procedure(&[Value::Integer(2_000_000_000)]));
            assert!(msg.contains("2048"), "got: {msg}");
        }
        let msg = is_err(email(&[Value::Integer(2_000_000_000), Value::Integer(20)]));
        assert!(msg.contains("2048"), "got: {msg}");

        // The cap itself is still allowed.
        match alphabetic_text(&[Value::Integer(2048)]) {
            Eval::Normal(Value::Text(text)) => assert_eq!(text.chars().count(), 2048),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn alphanumeric_text_is_lowercase_hex() {
        seed(7);
        match alphanumeric_text(&[Value::Integer(64)]) {
            Eval::Normal(Value::Text(text)) => assert!(
                text.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "the GUID-derived original yields hex only, got: {text}"
            ),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn alphabetic_text_negative_length_is_error() {
        let msg = is_err(alphabetic_text(&[Value::Integer(-1)]));
        assert!(msg.contains("≥ 0"), "got: {msg}");
    }

    #[test]
    fn alphanumeric_text_returns_correct_length() {
        seed(8);
        match alphanumeric_text(&[Value::Integer(8)]) {
            Eval::Normal(Value::Text(s)) => {
                assert_eq!(s.len(), 8, "expected length 8, got {}", s.len());
                assert!(
                    s.chars()
                        .all(|c| c.is_ascii_alphanumeric() && !c.is_uppercase()),
                    "expected lowercase alnum: {s:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn alphanumeric_text_seeded_is_deterministic() {
        seed(44);
        let a = ok_val(alphanumeric_text(&[Value::Integer(12)]));
        seed(44);
        let b = ok_val(alphanumeric_text(&[Value::Integer(12)]));
        assert_eq!(a, b);
    }

    #[test]
    fn unicode_text_returns_correct_length() {
        seed(2);
        match unicode_text(&[Value::Integer(6)]) {
            Eval::Normal(Value::Text(s)) => {
                let char_count = s.chars().count();
                assert_eq!(char_count, 6, "expected 6 chars, got {char_count}");
                assert!(
                    s.chars().all(|c| ('\u{0430}'..='\u{044F}').contains(&c)),
                    "expected Cyrillic: {s:?}"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unicode_text_seeded_is_deterministic() {
        seed(88);
        let a = ok_val(unicode_text(&[Value::Integer(10)]));
        seed(88);
        let b = ok_val(unicode_text(&[Value::Integer(10)]));
        assert_eq!(a, b);
    }

    #[test]
    fn unicode_text_negative_length_is_error() {
        let msg = is_err(unicode_text(&[Value::Integer(-3)]));
        assert!(msg.contains("≥ 0"), "got: {msg}");
    }

    #[test]
    fn email_contains_at_sign() {
        seed(4);
        match email(&[]) {
            Eval::Normal(Value::Text(s)) => {
                assert!(s.contains('@'), "email missing @: {s:?}");
                assert!(s.contains('.'), "email missing dot: {s:?}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn email_seeded_is_deterministic() {
        seed(101);
        let a = ok_val(email(&[]));
        seed(101);
        let b = ok_val(email(&[]));
        assert_eq!(a, b);
    }

    #[test]
    fn email_wrong_args_is_error() {
        let msg = is_err(email(&[Value::Integer(10)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn guid_value_returns_guid_string() {
        match guid_value(&[]) {
            Eval::Normal(Value::Guid(g)) => {
                // RFC 4122 v4 GUID format: {XXXXXXXX-XXXX-4XXX-YXXX-XXXXXXXXXXXX}
                assert!(g.starts_with('{') && g.ends_with('}'), "bad GUID: {g}");
                assert_eq!(g.len(), 38, "expected 38-char GUID, got {}", g.len());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn guid_value_no_args_required() {
        let msg = is_err(guid_value(&[Value::Integer(1)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    /// Codeunit 130500 stores the seed in a field and `GetSeed` returns it,
    /// so draws in between must not move it.
    #[test]
    fn set_seed_then_get_seed_round_trips() {
        ok_val(set_seed(&[Value::Integer(999)]));
        assert_eq!(ok_val(get_seed(&[])), Value::Integer(999));
        ok_val(integer_in_range(&[Value::Integer(100)]));
        ok_val(integer_in_range(&[Value::Integer(100)]));
        assert_eq!(
            ok_val(get_seed(&[])),
            Value::Integer(999),
            "GetSeed returns the seed, not the generator state"
        );
    }

    #[test]
    fn set_seed_zero_treated_as_one() {
        ok_val(set_seed(&[Value::Integer(0)]));
        let r0 = ok_val(integer_in_range(&[Value::Integer(100)]));

        seed(1);
        let r1 = ok_val(integer_in_range(&[Value::Integer(100)]));

        assert_eq!(r0, r1, "seed 0 should behave like seed 1");
    }

    #[test]
    fn set_seed_no_args_is_error() {
        let msg = is_err(set_seed(&[]));
        assert!(msg.contains("requires"), "got: {msg}");
    }

    #[test]
    fn get_seed_no_args_required() {
        let msg = is_err(get_seed(&[Value::Integer(1)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn set_default_seed_uses_time_of_day() {
        seed(42);
        let before = crate::interpreter::dispatch::clock_time().max(1) as u64;
        ok_val(set_default_seed(&[]));
        let after = crate::interpreter::dispatch::clock_time().max(1) as u64;
        let seed = match get_seed(&[]) {
            Eval::Normal(Value::Integer(seed)) => seed as u64,
            other => panic!("expected Integer from GetSeed, got {other:?}"),
        };
        if before <= after {
            assert!((before..=after).contains(&seed));
        } else {
            assert!(seed >= before || seed <= after, "seed={seed}");
        }
    }

    #[test]
    fn set_default_seed_no_args_required() {
        let msg = is_err(set_default_seed(&[Value::Integer(1)]));
        assert!(msg.contains("expects"), "got: {msg}");
    }

    #[test]
    fn resolve_is_case_insensitive() {
        assert!(resolve("AlphabeticText").is_some());
        assert!(resolve("ALPHABETICTEXT").is_some());
        assert!(resolve("alphabetictext").is_some());
        assert!(resolve("IntegerInRange").is_some());
        assert!(resolve("SetSeed").is_some());
        assert!(resolve("GuidValue").is_some());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        assert!(resolve("DoesNotExist").is_none());
        assert!(resolve("").is_none());
    }
}
