//! Text and Code builtins: length, substring, case, search and padding.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

pub(super) fn builtin_strlen(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] | [Value::Code(s)] => {
            Eval::Normal(Value::Integer(s.chars().count() as i64))
        }
        [v] => eval_error(format!(
            "StrLen expects Text or Code, got {}",
            v.type_name()
        )),
        _ => eval_error("StrLen expects exactly 1 argument"),
    }
}

/// `CopyStr(s, pos[, len])` — extract a substring. 1-based position.
pub(super) fn builtin_copystr(args: &[Value]) -> Eval {
    // Keep `pos` signed so a negative value is rejected explicitly rather than
    // wrapping to a huge usize via an `as usize` cast.
    let (s, pos) = match args {
        [Value::Text(s), Value::Integer(pos)]
        | [Value::Code(s), Value::Integer(pos)]
        | [Value::Text(s), Value::Integer(pos), _]
        | [Value::Code(s), Value::Integer(pos), _] => (s.clone(), *pos),
        _ => return eval_error("CopyStr expects (Text, Integer[, Integer])"),
    };
    let len = match args.get(2) {
        Some(Value::Integer(n)) => {
            if *n < 0 {
                return eval_error(format!("CopyStr: len must be >= 0, got {n}"));
            }
            *n as usize
        }
        None => s.chars().count(),
        Some(v) => {
            return eval_error(format!(
                "CopyStr: len must be Integer, got {}",
                v.type_name()
            ))
        }
    };
    if pos <= 0 {
        return eval_error("CopyStr: position must be >= 1");
    }
    let pos = pos as usize;
    let chars: Vec<char> = s.chars().collect();
    // BC's CopyStr is the *safe* truncating variant: a position beyond the
    // string length returns the empty string (it does not raise an error).
    if pos > chars.len() {
        return Eval::Normal(Value::Text(String::new()));
    }
    let start = pos - 1;
    let end = (start + len).min(chars.len());
    Eval::Normal(Value::Text(chars[start..end].iter().collect()))
}

pub(super) fn builtin_lowercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [Value::Code(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [v] => eval_error(format!(
            "LowerCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => eval_error("LowerCase expects exactly 1 argument"),
    }
}

pub(super) fn builtin_uppercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_uppercase())),
        [Value::Code(s)] => Eval::Normal(Value::Code(s.to_uppercase())),
        [v] => eval_error(format!(
            "UpperCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => eval_error("UpperCase expects exactly 1 argument"),
    }
}

/// `IndexOf(s, needle)` — return the 1-based index of `needle` in `s`,
/// or 0 if not found. Empty needle always returns 0 (AL convention).
pub(super) fn builtin_indexof(args: &[Value]) -> Eval {
    let (s, needle) = match args {
        [Value::Text(s), Value::Text(n)]
        | [Value::Code(s), Value::Text(n)]
        | [Value::Text(s), Value::Code(n)]
        | [Value::Code(s), Value::Code(n)] => (s.as_str(), n.as_str()),
        _ => return eval_error("IndexOf expects (Text, Text)"),
    };
    if needle.is_empty() {
        return Eval::Normal(Value::Integer(0));
    }
    let result = s
        .find(needle)
        .map(|i| s[..i].chars().count() as i64 + 1)
        .unwrap_or(0);
    Eval::Normal(Value::Integer(result))
}

/// `MaxStrLen` requires declared type metadata that `Value` does not carry.
pub(super) fn builtin_maxstrlen(args: &[Value]) -> Eval {
    match args {
        [Value::Text(_)] | [Value::Code(_)] => eval_error(
            "MaxStrLen is unavailable because the interpreter does not retain declared text lengths",
        ),
        [v] => eval_error(format!(
            "MaxStrLen expects Text or Code, got {}",
            v.type_name()
        )),
        _ => eval_error("MaxStrLen expects exactly 1 argument"),
    }
}

/// `StrPos(s, substring)` — 1-based position of the first occurrence, 0 when
/// absent or when the substring is empty (BC convention).
pub(super) fn builtin_strpos(args: &[Value]) -> Eval {
    let (s, needle) = match args {
        [Value::Text(s) | Value::Code(s), Value::Text(n) | Value::Code(n)] => {
            (s.as_str(), n.as_str())
        }
        _ => return eval_error("StrPos expects (Text, Text)"),
    };
    if needle.is_empty() {
        return Eval::Normal(Value::Integer(0));
    }
    let result = s
        .find(needle)
        .map(|i| s[..i].chars().count() as i64 + 1)
        .unwrap_or(0);
    Eval::Normal(Value::Integer(result))
}

/// `DelChr(s [, where [, which]])` — delete characters. `where` is any
/// combination of `<` (leading), `>` (trailing), `=` (everywhere); defaults
/// follow BC: `where` = `'<'`, `which` = `' '` (space).
pub(super) fn builtin_delchr(args: &[Value]) -> Eval {
    let s = match args.first() {
        Some(Value::Text(s) | Value::Code(s)) => s.clone(),
        Some(v) => return eval_error(format!("DelChr expects Text, got {}", v.type_name())),
        None => return eval_error("DelChr expects 1 to 3 arguments"),
    };
    if args.len() > 3 {
        return eval_error("DelChr expects 1 to 3 arguments");
    }
    let where_ = match args.get(1) {
        None => "<".to_string(),
        Some(Value::Text(w) | Value::Code(w)) => w.clone(),
        Some(v) => return eval_error(format!("DelChr: where must be Text, got {}", v.type_name())),
    };
    let which: Vec<char> = match args.get(2) {
        None => vec![' '],
        Some(Value::Text(w) | Value::Code(w)) => w.chars().collect(),
        Some(v) => return eval_error(format!("DelChr: which must be Text, got {}", v.type_name())),
    };
    if let Some(bad) = where_.chars().find(|c| !matches!(c, '<' | '>' | '=')) {
        return eval_error(format!(
            "DelChr: where must contain only '<', '>' or '=', got '{bad}'"
        ));
    }
    let in_set = |c: char| which.contains(&c);
    let mut result: &str = &s;
    let everywhere = where_.contains('=');
    if everywhere {
        return Eval::Normal(Value::Text(
            result.chars().filter(|c| !in_set(*c)).collect(),
        ));
    }
    if where_.contains('<') {
        result = result.trim_start_matches(in_set);
    }
    if where_.contains('>') {
        result = result.trim_end_matches(in_set);
    }
    Eval::Normal(Value::Text(result.to_string()))
}

/// `ConvertStr(s, from, to)` — replace every occurrence of the i-th character
/// of `from` with the i-th character of `to`. Errors when the lengths differ
/// (BC behaviour).
pub(super) fn builtin_convertstr(args: &[Value]) -> Eval {
    let (s, from, to) = match args {
        [Value::Text(s) | Value::Code(s), Value::Text(f) | Value::Code(f), Value::Text(t) | Value::Code(t)] => {
            (s, f, t)
        }
        _ => return eval_error("ConvertStr expects (Text, Text, Text)"),
    };
    let from: Vec<char> = from.chars().collect();
    let to: Vec<char> = to.chars().collect();
    if from.len() != to.len() {
        return eval_error("ConvertStr: FromCharacters and ToCharacters must have the same length");
    }
    let converted: String = s
        .chars()
        .map(|c| match from.iter().position(|f| *f == c) {
            Some(i) => to[i],
            None => c,
        })
        .collect();
    Eval::Normal(Value::Text(converted))
}

/// `PadStr(s, length [, fill])` — return exactly `length` characters: truncate
/// when too long, pad on the right with `fill` (default space) when too short.
pub(super) fn builtin_padstr(args: &[Value]) -> Eval {
    let (s, length) = match args {
        [Value::Text(s) | Value::Code(s), Value::Integer(n)]
        | [Value::Text(s) | Value::Code(s), Value::Integer(n), _] => (s.clone(), *n),
        _ => return eval_error("PadStr expects (Text, Integer[, Text])"),
    };
    if length < 0 {
        return eval_error("PadStr: length must be >= 0");
    }
    let fill = match args.get(2) {
        None => ' ',
        Some(Value::Text(f) | Value::Code(f)) => match f.chars().next() {
            Some(c) => c,
            None => return eval_error("PadStr: fill character cannot be empty"),
        },
        Some(v) => {
            return eval_error(format!(
                "PadStr: fill character must be Text, got {}",
                v.type_name()
            ))
        }
    };
    let length = length as usize;
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() > length {
        chars.truncate(length);
    } else {
        chars.resize(length, fill);
    }
    Eval::Normal(Value::Text(chars.into_iter().collect()))
}

/// `SelectStr(index, commaString)` — the 1-based `index`-th comma-separated
/// element; errors when the index is out of range (BC behaviour).
pub(super) fn builtin_selectstr(args: &[Value]) -> Eval {
    let (index, list) = match args {
        [Value::Integer(n), Value::Text(s) | Value::Code(s)] => (*n, s.as_str()),
        _ => return eval_error("SelectStr expects (Integer, Text)"),
    };
    if index < 1 {
        return eval_error("SelectStr: index must be >= 1");
    }
    match list.split(',').nth(index as usize - 1) {
        Some(part) => Eval::Normal(Value::Text(part.to_string())),
        None => eval_error(format!(
            "SelectStr: index {index} is beyond the number of elements in '{list}'"
        )),
    }
}

/// `IncStr(s)` — increment the last number embedded in the string, preserving
/// its zero-padded width. Returns `''` when the string contains no digits
/// (BC behaviour).
pub(super) fn builtin_incstr(args: &[Value]) -> Eval {
    let s = match args {
        [Value::Text(s) | Value::Code(s)] => s.clone(),
        _ => return eval_error("IncStr expects exactly 1 Text argument"),
    };
    let chars: Vec<char> = s.chars().collect();
    let mut end = None;
    for (i, c) in chars.iter().enumerate().rev() {
        if c.is_ascii_digit() {
            end = Some(i + 1);
            break;
        }
    }
    let Some(end) = end else {
        return Eval::Normal(Value::Text(String::new()));
    };
    let mut start = end;
    while start > 0 && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let digits: String = chars[start..end].iter().collect();
    let width = digits.len();
    let Some(number) = digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_add(1))
    else {
        return eval_error(format!("IncStr: number '{digits}' is out of range"));
    };
    let incremented = format!("{number:0width$}");
    let mut result: String = chars[..start].iter().collect();
    result.push_str(&incremented);
    result.extend(&chars[end..]);
    Eval::Normal(Value::Text(result))
}

/// `DelStr(s, pos[, len])` — `s` without `len` characters from `pos`
/// (1-based); without `len`, everything from `pos` on.
pub(super) fn builtin_delstr(args: &[Value]) -> Eval {
    let (s, pos) = match args {
        [Value::Text(s) | Value::Code(s), Value::Integer(pos), ..] if args.len() <= 3 => {
            (s.clone(), *pos)
        }
        _ => return eval_error("DelStr expects (Text, Integer[, Integer])"),
    };
    if pos < 1 {
        return eval_error("DelStr: position must be >= 1");
    }
    let chars: Vec<char> = s.chars().collect();
    let start = usize::try_from(pos - 1)
        .unwrap_or(usize::MAX)
        .min(chars.len());
    let end = match args.get(2) {
        None => chars.len(),
        Some(Value::Integer(len)) if *len >= 0 => start
            .saturating_add(usize::try_from(*len).unwrap_or(usize::MAX))
            .min(chars.len()),
        Some(_) => return eval_error("DelStr: length must be a non-negative Integer"),
    };
    let kept: String = chars[..start].iter().chain(&chars[end..]).collect();
    Eval::Normal(match &args[0] {
        Value::Code(_) => Value::Code(kept),
        _ => Value::Text(kept),
    })
}

/// `Evaluate(var x, text)`: the value `text` spells, of `x`'s type. The new
/// value goes back to `x` through the call's var write-back; the result is
/// whether it parsed. Formats are the invariant ones (`12.5`, `true`).
pub(super) fn builtin_evaluate(args: &[Value]) -> Result<(bool, Option<Value>), String> {
    let (target, raw) = match args {
        [target, Value::Text(raw) | Value::Code(raw)] => (target, raw.as_str()),
        [_, other] => {
            return Err(format!(
                "Evaluate expects Text as its second argument, got {}",
                other.type_name()
            ))
        }
        _ => return Err("Evaluate expects (var Variable, Text)".to_string()),
    };
    let text = raw.trim();
    // Numbers as Format shows them group thousands: `1,234.5`.
    let number = text.replace(',', "");
    let parsed = match target {
        Value::Integer(_) => number.parse::<i64>().ok().map(Value::Integer),
        Value::BigInteger(_) => number
            .trim_end_matches(['l', 'L'])
            .parse::<i64>()
            .ok()
            .map(Value::BigInteger),
        Value::Decimal(_) => number
            .parse::<rust_decimal::Decimal>()
            .ok()
            .map(Value::Decimal),
        Value::Boolean(_) => match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(Value::Boolean(true)),
            "false" | "no" | "0" => Some(Value::Boolean(false)),
            _ => None,
        },
        Value::Text(_) => Some(Value::Text(raw.to_string())),
        Value::Code(_) => Some(Value::Code(text.to_uppercase())),
        other => {
            return Err(format!(
                "Evaluate into {} is not supported by the local runtime",
                other.type_name()
            ))
        }
    };
    Ok((parsed.is_some(), parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, error_of, ok};

    #[test]
    fn strlen_returns_char_count() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Text("hello".into())], &mut ctx);
        assert_eq!(ok(result), Value::Integer(5));
    }

    #[test]
    fn strlen_non_text_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Integer(123)], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn copystr_extracts_substring() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("Hello World".into()),
                Value::Integer(1),
                Value::Integer(5),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("Hello".into()));
    }

    #[test]
    fn indexof_finds_needle() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![
                Value::Text("Hello World".into()),
                Value::Text("World".into()),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(7));
    }

    #[test]
    fn indexof_missing_returns_zero() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("Hello".into()), Value::Text("xyz".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(0));
    }

    #[test]
    fn lowercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "LowerCase",
            vec![Value::Text("HELLO".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("hello".into()));
    }

    #[test]
    fn uppercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "UpperCase",
            vec![Value::Text("hello".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("HELLO".into()));
    }

    #[test]
    fn copystr_position_beyond_string_length_returns_empty() {
        // BC's CopyStr is the safe truncating variant: past-the-end positions
        // yield '' rather than a runtime error.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("abc".into()),
                Value::Integer(4),
                Value::Integer(1),
            ],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Text(String::new()),
            "CopyStr pos > string length must return the empty string"
        );
    }

    #[test]
    fn copystr_negative_len_errors_not_silent_truncate() {
        // Regression: CopyStr("hello", 1, -3) must raise an error. A naive
        // `*n as usize` cast wraps -3 to 2^64-3, which then clamps to the
        // string end and silently returns "hello" instead of erroring.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("hello".into()),
                Value::Integer(1),
                Value::Integer(-3),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr with negative len must error, got: {:?}",
            result
        );
    }

    #[test]
    fn copystr_negative_pos_errors() {
        // Regression: CopyStr("hello", -1, 2) must raise an error. A naive
        // `*pos as usize` cast wraps -1 to 2^64-1; the primary `pos <= 0`
        // guard must reject it directly.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("hello".into()),
                Value::Integer(-1),
                Value::Integer(2),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr with negative pos must error, got: {:?}",
            result
        );
    }

    #[test]
    fn indexof_empty_needle_returns_zero() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("ab".into()), Value::Text(String::new())],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Integer(0),
            "IndexOf with empty needle should return 0"
        );
    }

    #[test]
    fn power_strpos_and_string_builtins() {
        use rust_decimal_macros::dec;
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Power",
                vec![Value::Integer(2), Value::Integer(10)],
                &mut ctx
            )),
            Value::Decimal(dec!(1024))
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrPos",
                vec![
                    Value::Text("Hello World".into()),
                    Value::Text("World".into())
                ],
                &mut ctx
            )),
            Value::Integer(7)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrPos",
                vec![Value::Text("abc".into()), Value::Text("zz".into())],
                &mut ctx
            )),
            Value::Integer(0)
        );
        // DelChr default: trim leading spaces only.
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![Value::Text("  x  ".into())],
                &mut ctx
            )),
            Value::Text("x  ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![
                    Value::Text(" a,b, c ".into()),
                    Value::Text("=".into()),
                    Value::Text(",".into())
                ],
                &mut ctx
            )),
            Value::Text(" ab c ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![
                    Value::Text("  x  ".into()),
                    Value::Text("<>".into()),
                    Value::Text(" ".into())
                ],
                &mut ctx
            )),
            Value::Text("x".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "ConvertStr",
                vec![
                    Value::Text("a-b-c".into()),
                    Value::Text("-".into()),
                    Value::Text("_".into())
                ],
                &mut ctx
            )),
            Value::Text("a_b_c".into())
        );
        assert!(dispatch_call(
            None,
            "ConvertStr",
            vec![
                Value::Text("abc".into()),
                Value::Text("ab".into()),
                Value::Text("x".into())
            ],
            &mut ctx
        )
        .is_error());
        assert_eq!(
            ok(dispatch_call(
                None,
                "PadStr",
                vec![Value::Text("ab".into()), Value::Integer(5)],
                &mut ctx
            )),
            Value::Text("ab   ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "PadStr",
                vec![
                    Value::Text("abcdef".into()),
                    Value::Integer(3),
                    Value::Text("*".into())
                ],
                &mut ctx
            )),
            Value::Text("abc".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "SelectStr",
                vec![Value::Integer(2), Value::Text("one,two,three".into())],
                &mut ctx
            )),
            Value::Text("two".into())
        );
        assert!(dispatch_call(
            None,
            "SelectStr",
            vec![Value::Integer(9), Value::Text("one,two".into())],
            &mut ctx
        )
        .is_error());
        assert_eq!(
            ok(dispatch_call(
                None,
                "IncStr",
                vec![Value::Text("INV-009".into())],
                &mut ctx
            )),
            Value::Text("INV-010".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "IncStr",
                vec![Value::Text("nodigits".into())],
                &mut ctx
            )),
            Value::Text(String::new())
        );
    }

    #[test]
    fn incstr_u64_max_errors_instead_of_overflowing() {
        let mut ctx = ctx();
        // u64::MAX parses, but incrementing it must be a range error, not a
        // wrap or panic.
        let error = error_of(dispatch_call(
            None,
            "IncStr",
            vec![Value::Text("X18446744073709551615".into())],
            &mut ctx,
        ));
        assert_eq!(
            error.message,
            "IncStr: number '18446744073709551615' is out of range"
        );
        // One below u64::MAX still increments normally.
        assert_eq!(
            ok(dispatch_call(
                None,
                "IncStr",
                vec![Value::Text("X18446744073709551614".into())],
                &mut ctx
            )),
            Value::Text("X18446744073709551615".into())
        );
    }
}
