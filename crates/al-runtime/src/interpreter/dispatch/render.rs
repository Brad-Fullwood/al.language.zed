//! Turning values into the text AL shows, and filling `%1` placeholders.
//!
//! StrSubstNo and Format share the substitution pass, which reads each
//! placeholder from the format string once so a substituted value carrying a
//! `%` is not rescanned.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

/// `StrSubstNo(fmt, arg1, …)` — substitute %1, %2, … in `fmt`.
pub(super) fn builtin_strsubstno(args: &[Value]) -> Eval {
    let fmt = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return eval_error("StrSubstNo requires at least 1 argument"),
    };
    let result = substitute_placeholders(&fmt, &args[1..]);
    Eval::Normal(Value::Text(result))
}

/// `Format(value[, length[, format]])` — convert a value to Text.
///
/// The default (format number 0) and XML (format number 9) renderings are
/// implemented; any other format number or a custom `<...>` format string is
/// an explicit error rather than a silently ignored argument. `length`
/// follows BC: positive → exactly `length` characters (right-padded or
/// truncated), negative → right-justified in `abs(length)` characters, 0 →
/// unconstrained.
pub(super) fn builtin_format(args: &[Value]) -> Eval {
    let Some(value) = args.first() else {
        return eval_error("Format() requires at least 1 argument");
    };
    if args.len() > 3 {
        return eval_error("Format expects at most 3 arguments");
    }
    let rendered = match args.get(2) {
        None | Some(Value::Integer(0)) => render_value(value),
        Some(Value::Integer(9)) => render_value_xml(value),
        Some(Value::Integer(n)) => match super::picture::render_standard(value, *n) {
            Ok(text) => text,
            Err(error) => return eval_error(format!("Format: {error}")),
        },
        Some(Value::Text(s)) | Some(Value::Code(s)) if s.is_empty() => render_value(value),
        Some(Value::Text(s)) | Some(Value::Code(s)) => {
            match super::picture::render_picture(value, s) {
                Ok(text) => text,
                Err(error) => return eval_error(format!("Format: {error}")),
            }
        }
        Some(other) => {
            return eval_error(format!(
                "Format: format argument must be an Integer or Text, got {}",
                other.type_name()
            ))
        }
    };
    let length = match args.get(1) {
        None => 0,
        Some(Value::Integer(n)) => *n,
        Some(other) => {
            return eval_error(format!(
                "Format: length must be an Integer, got {}",
                other.type_name()
            ))
        }
    };
    if length == 0 {
        return Eval::Normal(Value::Text(rendered));
    }
    let width = length.unsigned_abs() as usize;
    let mut chars: Vec<char> = rendered.chars().collect();
    if chars.len() > width {
        chars.truncate(width);
        return Eval::Normal(Value::Text(chars.into_iter().collect()));
    }
    let padding = std::iter::repeat_n(' ', width - chars.len());
    let text: String = if length > 0 {
        chars.into_iter().chain(padding).collect()
    } else {
        padding.chain(chars).collect()
    };
    Eval::Normal(Value::Text(text))
}

/// Render a value with Format's XML format (format number 9).
pub(crate) fn render_value_xml(v: &Value) -> String {
    match v {
        // XML format is culture-invariant: no thousands separators.
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(n) => n.normalize().to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Date(0) | Value::Time(0) | Value::DateTime(0) => String::new(),
        Value::Date(d) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(*d);
            format!("{y:04}-{m:02}-{day:02}")
        }
        Value::Time(t) => render_time_ms(*t),
        Value::DateTime(dt) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(
                dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
            );
            let time = render_time_ms(dt.rem_euclid(crate::interpreter::value::MS_PER_DAY));
            format!("{y:04}-{m:02}-{day:02}T{time}Z")
        }
        other => render_value(other),
    }
}

/// Render a milliseconds-since-midnight carrier as `HH:MM:SS[.fff]`.
fn render_time_ms(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if millis == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}.{millis:03}")
    }
}

/// Render a `Value` as AL would show it in StrSubstNo / Format.
///
/// Date/Time/DateTime render as invariant-culture date strings
/// (`MM/DD/YYYY`, `HH:MM:SS`), not their raw integer carriers; the undefined
/// values (0D/0T and the zero DateTime) render as the empty string, matching
/// BC.
pub(crate) fn render_value(v: &Value) -> String {
    match v {
        // BC's standard format 0 for numbers groups thousands:
        // `<Sign><Integer Thousand><Decimals>`.
        Value::Integer(n) | Value::BigInteger(n) => {
            let sign = if *n < 0 { "-" } else { "" };
            format!(
                "{sign}{}",
                super::picture::group_thousands(&n.unsigned_abs().to_string())
            )
        }
        Value::Decimal(n) => {
            let text = n.normalize().to_string();
            let (sign, digits) = match text.strip_prefix('-') {
                Some(digits) => ("-", digits),
                None => ("", text.as_str()),
            };
            match digits.split_once('.') {
                Some((integer, fraction)) => format!(
                    "{sign}{}.{fraction}",
                    super::picture::group_thousands(integer)
                ),
                None => format!("{sign}{}", super::picture::group_thousands(digits)),
            }
        }
        Value::Boolean(true) => "Yes".to_string(),
        Value::Boolean(false) => "No".to_string(),
        Value::Text(s) | Value::Code(s) | Value::TextBuilder(s) => s.clone(),
        Value::Date(0) | Value::Time(0) | Value::DateTime(0) => String::new(),
        Value::Date(d) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(*d);
            format!("{m:02}/{day:02}/{y:04}")
        }
        Value::Time(t) => render_time_ms(*t),
        Value::DateTime(dt) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(
                dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
            );
            let time = render_time_ms(dt.rem_euclid(crate::interpreter::value::MS_PER_DAY));
            format!("{m:02}/{day:02}/{y:04} {time}")
        }
        Value::Duration(d) => render_duration(*d),
        Value::Guid(g) => g.clone(),
        Value::Char(c) => c.to_string(),
        Value::Null => String::new(),
        Value::Empty => String::new(),
        Value::Option { member, .. } => member.clone(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Render a Duration as BC's `Format` does: the non-zero components from days
/// down, each singular or plural.
///
/// `CreateDateTime(20090505D, 133001T) - CreateDateTime(20090101D, 080000T)`
/// formats as `124 days 4 hours 30 minutes 1 second` (Microsoft Learn,
/// Duration data type, Example 1). A Duration carries milliseconds, so a
/// sub-second remainder shows as its own component.
pub(crate) fn render_duration(milliseconds: i64) -> String {
    const MS_PER_SECOND: u64 = 1_000;
    const MS_PER_MINUTE: u64 = 60 * MS_PER_SECOND;
    const MS_PER_HOUR: u64 = 60 * MS_PER_MINUTE;
    const MS_PER_DAY: u64 = 24 * MS_PER_HOUR;

    let mut rest = milliseconds.unsigned_abs();
    let mut parts: Vec<String> = Vec::new();
    for (unit, name) in [
        (MS_PER_DAY, "day"),
        (MS_PER_HOUR, "hour"),
        (MS_PER_MINUTE, "minute"),
        (MS_PER_SECOND, "second"),
        (1, "millisecond"),
    ] {
        let count = rest / unit;
        rest %= unit;
        if count != 0 {
            let plural = if count == 1 { "" } else { "s" };
            parts.push(format!("{count} {name}{plural}"));
        }
    }
    if parts.is_empty() {
        return "0 seconds".to_string();
    }
    let rendered = parts.join(" ");
    if milliseconds < 0 {
        format!("-{rendered}")
    } else {
        rendered
    }
}

/// Substitute `%1`, `%2`, … placeholders in `fmt`, rendering the 1-based
/// argument `n` (for `n` in `1..=arg_count`) through `render`.
///
/// Single left-to-right pass over `fmt`: inserted argument text is never
/// re-scanned, so an argument whose value contains `%1` stays literal (BC
/// behaviour). Digit runs are read maximally (`%10` targets the 10th
/// argument); a placeholder with no matching argument is left verbatim. A
/// `render` error aborts the substitution.
pub(crate) fn substitute_placeholders_with<E>(
    fmt: &str,
    arg_count: usize,
    mut render: impl FnMut(usize) -> Result<String, E>,
) -> Result<String, E> {
    let mut result = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
            digits.push(*d);
            chars.next();
        }
        match digits.parse::<usize>() {
            Ok(n) if n >= 1 && n <= arg_count => {
                result.push_str(&render(n)?);
            }
            _ => {
                result.push('%');
                result.push_str(&digits);
            }
        }
    }
    Ok(result)
}

/// Substitute %1, %2, … placeholders in `fmt` with rendered arg values
/// (see [`substitute_placeholders_with`] for the scanning rules).
pub(super) fn substitute_placeholders(fmt: &str, args: &[Value]) -> String {
    match substitute_placeholders_with(fmt, args.len(), |n| {
        Ok::<_, std::convert::Infallible>(render_value(&args[n - 1]))
    }) {
        Ok(result) => result,
        Err(infallible) => match infallible {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, ok};

    #[test]
    fn strsubstno_formats_correctly() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("Hello %1, you are %2 years old".into()),
                Value::Text("Alice".into()),
                Value::Integer(30),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "Hello Alice, you are 30 years old"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn strsubstno_no_args_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrSubstNo", vec![], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn format_integer_to_text() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Format", vec![Value::Integer(42)], &mut ctx);
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "42"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn strsubstno_handles_percent10_without_corrupting_percent1() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("%1 and %10".into()),
                Value::Text("FIRST".into()),
                Value::Text("TWO".into()),
                Value::Text("THREE".into()),
                Value::Text("FOUR".into()),
                Value::Text("FIVE".into()),
                Value::Text("SIX".into()),
                Value::Text("SEVEN".into()),
                Value::Text("EIGHT".into()),
                Value::Text("NINE".into()),
                Value::Text("TENTH".into()),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(
                s, "FIRST and TENTH",
                "%%10 must map to 10th arg; got: {s:?}"
            ),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn format_renders_dates_not_raw_carriers() {
        let mut ctx = ctx();
        let date = crate::interpreter::value::al_days_from_ymd(2024, 1, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(date)],
                &mut ctx
            )),
            Value::Text("01/31/2024".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(0)],
                &mut ctx
            )),
            Value::Text(String::new()),
            "the undefined date renders as ''"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Time(6 * 3_600_000 + 30 * 60_000)],
                &mut ctx
            )),
            Value::Text("06:30:00".into())
        );
        // StrSubstNo renders through the same path.
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrSubstNo",
                vec![Value::Text("on %1".into()), Value::Date(date)],
                &mut ctx
            )),
            Value::Text("on 01/31/2024".into())
        );
    }

    #[test]
    fn format_supports_length_standard_formats_and_pictures() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Integer(42), Value::Integer(5)],
                &mut ctx
            )),
            Value::Text("42   ".into()),
            "positive length pads on the right"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Integer(42), Value::Integer(-5)],
                &mut ctx
            )),
            Value::Text("   42".into()),
            "negative length right-justifies"
        );
        let date = crate::interpreter::value::al_days_from_ymd(2024, 1, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(date), Value::Integer(0), Value::Integer(9)],
                &mut ctx
            )),
            Value::Text("2024-01-31".into()),
            "format 9 is the XML rendering"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![
                    Value::Decimal(rust_decimal_macros::dec!(1234.5)),
                    Value::Integer(0),
                    Value::Text("<Precision,2:2><Standard Format,0>".into())
                ],
                &mut ctx
            )),
            Value::Text("1,234.50".into()),
            "a picture string renders its components"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Integer(-1234567)],
                &mut ctx
            )),
            Value::Text("-1,234,567".into()),
            "the standard format groups thousands"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![
                    Value::Decimal(rust_decimal_macros::dec!(1234567.5)),
                    Value::Integer(0),
                    Value::Integer(9)
                ],
                &mut ctx
            )),
            Value::Text("1234567.5".into()),
            "XML format is invariant: no grouping"
        );
        // A format the runtime cannot render must error, not be ignored.
        assert!(dispatch_call(
            None,
            "Format",
            vec![Value::Integer(1), Value::Integer(0), Value::Integer(7)],
            &mut ctx
        )
        .is_error());
        assert!(dispatch_call(
            None,
            "Format",
            vec![
                Value::Integer(1),
                Value::Integer(0),
                Value::Text("<Galaxy>".into())
            ],
            &mut ctx
        )
        .is_error());
    }

    #[test]
    fn strsubstno_does_not_rescan_substituted_values() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("%2 %1".into()),
                Value::Text("A".into()),
                Value::Text("x%1y".into()),
            ],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Text("x%1y A".into()),
            "a %1 inside a substituted value must stay literal"
        );
    }
}
