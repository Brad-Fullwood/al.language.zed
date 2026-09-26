//! `Format(value, length, '<...>')` picture strings and numbered standard
//! formats, in the en-US culture the rest of the runtime renders in
//! (`MM/DD/YYYY` dates, `,` thousands, `.` decimals).
//!
//! A picture mixes literal text with components: `<Integer Thousand>`,
//! `<Decimals>`, `<Sign>`, `<Precision,2:2>`, `<Year4>`, `<Month,2>`,
//! `<Day,2>`, `<Hours24,2>`, `<Minutes,2>`, `<Filler Character,0>` and the
//! like. A component the runtime does not know is an error, never dropped.

use rust_decimal::prelude::*;

use crate::interpreter::value::{ymd_from_al_days, Value, MS_PER_DAY};

use super::datetime::{iso_week, weekday_of};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// One `<Name,argument>` component or a run of literal text.
enum Part<'a> {
    Literal(&'a str),
    Component {
        name: String,
        argument: Option<&'a str>,
    },
}

fn parts(picture: &str) -> Result<Vec<Part<'_>>, String> {
    let mut parts = Vec::new();
    let mut rest = picture;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let close = rest
                    .find('>')
                    .ok_or_else(|| format!("unclosed component in format '{picture}'"))?;
                let inner = &rest[1..close];
                let (name, argument) = match inner.split_once(',') {
                    Some((name, argument)) => (name, Some(argument)),
                    None => (inner, None),
                };
                parts.push(Part::Component {
                    name: name.trim().to_ascii_lowercase(),
                    argument,
                });
                rest = &rest[close + 1..];
            }
            Some(at) => {
                parts.push(Part::Literal(&rest[..at]));
                rest = &rest[at..];
            }
            None => {
                parts.push(Part::Literal(rest));
                rest = "";
            }
        }
    }
    Ok(parts)
}

/// A number split for rendering: its sign, integer digits and fraction
/// digits (without the separator).
struct Number {
    negative: bool,
    integer: String,
    fraction: String,
}

fn number_of(value: &Value, precision: Option<(u32, u32)>) -> Option<Number> {
    let decimal = match value {
        Value::Integer(n) | Value::BigInteger(n) => Decimal::from(*n),
        Value::Decimal(d) => *d,
        _ => return None,
    };
    let decimal = match precision {
        Some((_, max)) => {
            decimal.round_dp_with_strategy(max, RoundingStrategy::MidpointAwayFromZero)
        }
        None => decimal,
    };
    let text = decimal.abs().normalize().to_string();
    let (integer, fraction) = match text.split_once('.') {
        Some((integer, fraction)) => (integer.to_string(), fraction.to_string()),
        None => (text, String::new()),
    };
    let mut fraction = fraction;
    if let Some((min, _)) = precision {
        while fraction.len() < min as usize {
            fraction.push('0');
        }
    }
    Some(Number {
        negative: decimal.is_sign_negative() && !decimal.is_zero(),
        integer,
        fraction,
    })
}

/// `1234567` → `1,234,567`.
pub(crate) fn group_thousands(digits: &str) -> String {
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The picture a numbered standard format stands for.
fn standard_picture(value: &Value, number: i64) -> Result<&'static str, String> {
    let numeric = matches!(
        value,
        Value::Integer(_) | Value::BigInteger(_) | Value::Decimal(_)
    );
    match (numeric, number) {
        (true, 0) => Ok("<Sign><Integer Thousand><Decimals>"),
        (true, 1) => Ok("<Sign><Integer><Decimals>"),
        (true, 2) => Ok("<Sign><Integer><Decimals>"),
        (true, 3) => Ok("<Integer Thousand><Decimals><Sign>"),
        (true, 4) => Ok("<Integer><Decimals><Sign>"),
        _ => Err(format!(
            "standard format {number} of a {} is not supported by the local runtime",
            value.type_name()
        )),
    }
}

/// Render `value` with numbered standard format `number` (other than the 0
/// and 9 the caller handles for every type).
pub(crate) fn render_standard(value: &Value, number: i64) -> Result<String, String> {
    render_picture(value, standard_picture(value, number)?)
}

fn pad_left(text: String, width: Option<usize>, filler: char) -> String {
    match width {
        Some(width) if text.chars().count() < width => {
            let mut padded: String =
                std::iter::repeat_n(filler, width - text.chars().count()).collect();
            padded.push_str(&text);
            padded
        }
        _ => text,
    }
}

/// Render `value` with picture string `picture`.
pub(crate) fn render_picture(value: &Value, picture: &str) -> Result<String, String> {
    let parts = parts(picture)?;
    // The filler applies to every padded component, wherever it is declared
    // (`<Integer,4><Filler Character,0>` is the usual spelling).
    let mut filler = ' ';
    let mut precision: Option<(u32, u32)> = None;
    for part in &parts {
        if let Part::Component { name, argument } = part {
            match name.as_str() {
                "filler character" => {
                    filler = argument.and_then(|a| a.chars().next()).unwrap_or(' ');
                }
                "precision" => {
                    let argument = argument.unwrap_or_default();
                    let (min, max) = argument.split_once(':').unwrap_or((argument, argument));
                    precision = Some((
                        min.trim()
                            .parse()
                            .map_err(|_| format!("invalid precision '{argument}'"))?,
                        max.trim()
                            .parse()
                            .map_err(|_| format!("invalid precision '{argument}'"))?,
                    ));
                }
                _ => {}
            }
        }
    }
    let date = match value {
        Value::Date(d) => Some(*d),
        Value::DateTime(dt) => Some(dt.div_euclid(MS_PER_DAY)),
        _ => None,
    };
    let time = match value {
        Value::Time(t) => Some(*t),
        Value::DateTime(dt) => Some(dt.rem_euclid(MS_PER_DAY)),
        _ => None,
    };
    let mut out = String::new();
    for part in parts {
        let (name, argument) = match part {
            Part::Literal(text) => {
                out.push_str(text);
                continue;
            }
            Part::Component { name, argument } => (name, argument),
        };
        let width = argument.and_then(|a| a.trim().parse::<usize>().ok());
        let unsupported = || {
            format!(
                "format component <{name}> of a {} is not supported by the local runtime",
                value.type_name()
            )
        };
        match name.as_str() {
            "filler character" | "precision" => {}
            "standard format" => {
                let number = argument
                    .and_then(|a| a.trim().parse::<i64>().ok())
                    .ok_or_else(unsupported)?;
                let rendered = match number {
                    9 => super::render::render_value_xml(value),
                    _ => match number_of(value, precision) {
                        Some(_) => {
                            let standard = standard_picture(value, number)?;
                            let with_precision = match precision {
                                Some((min, max)) => format!("<Precision,{min}:{max}>{standard}"),
                                None => standard.to_string(),
                            };
                            render_picture(value, &with_precision)?
                        }
                        None if number == 0 => super::render::render_value(value),
                        None => return Err(unsupported()),
                    },
                };
                out.push_str(&rendered);
            }
            "sign" => {
                let number = number_of(value, precision).ok_or_else(unsupported)?;
                if number.negative {
                    out.push('-');
                }
            }
            "integer" | "integer thousand" => {
                let number = number_of(value, precision).ok_or_else(unsupported)?;
                let digits = if name == "integer thousand" {
                    group_thousands(&number.integer)
                } else {
                    number.integer
                };
                out.push_str(&pad_left(digits, width, filler));
            }
            "decimals" => {
                // `<Decimals,3>` is the separator and two digits.
                let precision = match (precision, width) {
                    (None, Some(width)) => {
                        let digits = width.saturating_sub(1) as u32;
                        Some((digits, digits))
                    }
                    (precision, _) => precision,
                };
                let number = number_of(value, precision).ok_or_else(unsupported)?;
                if !number.fraction.is_empty() {
                    out.push('.');
                    out.push_str(&number.fraction);
                }
            }
            "day" => {
                let (_, _, day) = ymd_from_al_days(date.ok_or_else(unsupported)?);
                out.push_str(&pad_left(day.to_string(), width, '0'));
            }
            "month" => {
                let (_, month, _) = ymd_from_al_days(date.ok_or_else(unsupported)?);
                out.push_str(&pad_left(month.to_string(), width, '0'));
            }
            "month text" => {
                let (_, month, _) = ymd_from_al_days(date.ok_or_else(unsupported)?);
                let text = MONTHS[(month - 1) as usize];
                out.push_str(
                    &text
                        .chars()
                        .take(width.unwrap_or(text.len()))
                        .collect::<String>(),
                );
            }
            "year" => {
                let (year, _, _) = ymd_from_al_days(date.ok_or_else(unsupported)?);
                out.push_str(&format!("{:02}", year.rem_euclid(100)));
            }
            "year4" => {
                let (year, _, _) = ymd_from_al_days(date.ok_or_else(unsupported)?);
                out.push_str(&format!("{year:04}"));
            }
            "weekday" => {
                let weekday = weekday_of(date.ok_or_else(unsupported)?);
                out.push_str(&weekday.to_string());
            }
            "weekday text" => {
                let weekday = weekday_of(date.ok_or_else(unsupported)?);
                let text = WEEKDAYS[(weekday - 1) as usize];
                out.push_str(
                    &text
                        .chars()
                        .take(width.unwrap_or(text.len()))
                        .collect::<String>(),
                );
            }
            "week" => {
                let (week, _) = iso_week(date.ok_or_else(unsupported)?);
                out.push_str(&pad_left(week.to_string(), width, '0'));
            }
            // Local dates are never closing dates.
            "closing" => {
                date.ok_or_else(unsupported)?;
            }
            "hours24" | "hours12" | "minutes" | "seconds" | "thousands" | "am/pm" => {
                let ms = time.ok_or_else(unsupported)?;
                let seconds = ms.div_euclid(1000);
                let hours = seconds / 3600;
                let text = match name.as_str() {
                    "hours24" => hours.to_string(),
                    "hours12" => (if hours % 12 == 0 { 12 } else { hours % 12 }).to_string(),
                    "minutes" => ((seconds / 60) % 60).to_string(),
                    "seconds" => (seconds % 60).to_string(),
                    "thousands" => format!("{:03}", ms.rem_euclid(1000)),
                    _ => (if hours < 12 { "AM" } else { "PM" }).to_string(),
                };
                out.push_str(&pad_left(text, width, '0'));
            }
            "text" => out.push_str(&super::render::render_value(value)),
            _ => return Err(unsupported()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::value::al_days_from_ymd;
    use rust_decimal_macros::dec;

    #[test]
    fn numbers_render_sign_grouping_precision_and_filler() {
        let render = |value: Value, picture: &str| render_picture(&value, picture).unwrap();
        assert_eq!(
            render(Value::Integer(1234567), "<Integer Thousand>"),
            "1,234,567"
        );
        assert_eq!(
            render(Value::Integer(42), "<Integer,4><Filler Character,0>"),
            "0042"
        );
        assert_eq!(
            render(
                Value::Decimal(dec!(1.5)),
                "<Precision,2:2><Standard Format,0>"
            ),
            "1.50"
        );
        assert_eq!(
            render(
                Value::Decimal(dec!(-1234.567)),
                "<Sign><Integer Thousand><Decimals,3>"
            ),
            "-1,234.57"
        );
        assert_eq!(render(Value::Decimal(dec!(1234.5)), "<Integer>"), "1234");
    }

    #[test]
    fn dates_and_times_render_their_components() {
        let date = Value::Date(al_days_from_ymd(2026, 9, 5));
        let render = |value: &Value, picture: &str| render_picture(value, picture).unwrap();
        assert_eq!(render(&date, "<Year4>-<Month,2>-<Day,2>"), "2026-09-05");
        assert_eq!(render(&date, "<Day>. <Month Text,3> <Year>"), "5. Sep 26");
        assert_eq!(
            render(&date, "<Weekday Text>, week <Week>"),
            "Saturday, week 36"
        );
        let time = Value::Time((13 * 3600 + 5 * 60 + 9) * 1000 + 7);
        assert_eq!(
            render(
                &time,
                "<Hours12>:<Minutes,2>:<Seconds,2>.<Thousands> <AM/PM>"
            ),
            "1:05:09.007 PM"
        );
    }

    #[test]
    fn unknown_components_and_mismatched_types_are_errors() {
        assert!(render_picture(&Value::Integer(1), "<Day>").is_err());
        assert!(render_picture(&Value::Integer(1), "<Galaxy>").is_err());
    }
}
