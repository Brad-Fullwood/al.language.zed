//! Date, Time and DateTime builtins, and the clock they read.
//!
//! The clock functions are the only place the interpreter reads wall time, so
//! a test can pin WORKDATE without pinning the host clock.

use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

use super::DispatchCtx;

/// `CreateDateTime(date, time)` — combine a Date and Time into a DateTime.
///
/// Carriers: `Date` is days since the AL epoch, `Time` is milliseconds since
/// midnight, `DateTime` is milliseconds since the AL epoch — so the result is
/// `days * MS_PER_DAY + time_ms`.
pub(super) fn builtin_createdatetime(args: &[Value]) -> Eval {
    match args {
        [Value::Date(d), Value::Time(t)] => {
            match d
                .checked_mul(crate::interpreter::value::MS_PER_DAY)
                .and_then(|ms| ms.checked_add(*t))
            {
                Some(dt) => Eval::Normal(Value::DateTime(dt)),
                None => eval_error("CreateDateTime: datetime overflow"),
            }
        }
        [a, b] => eval_error(format!(
            "CreateDateTime expects (Date, Time), got ({}, {})",
            a.type_name(),
            b.type_name()
        )),
        _ => eval_error("CreateDateTime expects exactly 2 arguments"),
    }
}

/// `Date2DMY(date, what)` — extract day (1), month (2) or year (3).
pub(super) fn builtin_date2dmy(args: &[Value]) -> Eval {
    let (date, what) = match args {
        [Value::Date(d), Value::Integer(w)] => (*d, *w),
        _ => return eval_error("Date2DMY expects (Date, Integer)"),
    };
    if date == 0 {
        return eval_error("Date2DMY is undefined for 0D");
    }
    let (year, month, day) = crate::interpreter::value::ymd_from_al_days(date);
    let part = match what {
        1 => day,
        2 => month,
        3 => year,
        other => {
            return eval_error(format!(
                "Date2DMY: the what argument must be 1 (day), 2 (month) or 3 (year), got {other}"
            ))
        }
    };
    Eval::Normal(Value::Integer(part))
}

/// `DMY2Date(day [, month [, year]])` — build a Date; omitted month/year come
/// from the session work date (BC behaviour).
pub(super) fn builtin_dmy2date(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    if args.is_empty() || args.len() > 3 {
        return eval_error("DMY2Date expects 1 to 3 arguments");
    }
    let mut parts = [0i64; 3];
    for (i, arg) in args.iter().enumerate() {
        match arg {
            Value::Integer(n) => parts[i] = *n,
            other => {
                return eval_error(format!(
                    "DMY2Date expects Integer arguments, got {}",
                    other.type_name()
                ))
            }
        }
    }
    let work = current_work_date(ctx);
    let (work_year, work_month, _) = crate::interpreter::value::ymd_from_al_days(work);
    let day = parts[0];
    let month = if args.len() >= 2 {
        parts[1]
    } else {
        work_month
    };
    let year = if args.len() >= 3 { parts[2] } else { work_year };
    match checked_al_date(year, month, day) {
        Some(date) => Eval::Normal(Value::Date(date)),
        None => eval_error(format!(
            "DMY2Date: {day}/{month}/{year} is not a valid date"
        )),
    }
}

/// Validate a year/month/day and convert it to the AL day carrier.
fn checked_al_date(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        _ => 28,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    Some(crate::interpreter::value::al_days_from_ymd(
        year, month, day,
    ))
}

/// `DT2Date(datetime)` — the date part of a DateTime.
pub(super) fn builtin_dt2date(args: &[Value]) -> Eval {
    match args {
        [Value::DateTime(dt)] => Eval::Normal(Value::Date(
            dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
        )),
        _ => eval_error("DT2Date expects exactly 1 DateTime argument"),
    }
}

/// `DT2Time(datetime)` — the time part of a DateTime.
pub(super) fn builtin_dt2time(args: &[Value]) -> Eval {
    match args {
        [Value::DateTime(dt)] => Eval::Normal(Value::Time(
            dt.rem_euclid(crate::interpreter::value::MS_PER_DAY),
        )),
        _ => eval_error("DT2Time expects exactly 1 DateTime argument"),
    }
}

/// The effective session work date: the value set through `WorkDate(d)`, or
/// today (BC's session default) when never set.
fn current_work_date(ctx: &DispatchCtx) -> i64 {
    ctx.work_date.unwrap_or_else(clock_today)
}

/// `WorkDate([newdate])` — read or set the session work date.
pub(super) fn builtin_workdate(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    match args {
        [] => Eval::Normal(Value::Date(current_work_date(ctx))),
        [Value::Date(d)] => {
            ctx.work_date = Some(*d);
            Eval::Normal(Value::Date(*d))
        }
        [v] => eval_error(format!("WorkDate expects a Date, got {}", v.type_name())),
        _ => eval_error("WorkDate expects at most 1 argument"),
    }
}

/// Signed milliseconds since the Unix epoch.
fn unix_now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).expect("system time exceeds i64"),
        Err(error) => {
            -i64::try_from(error.duration().as_millis()).expect("system time exceeds i64")
        }
    }
}

/// Current date as days since the AL epoch (0001-01-01).
pub(crate) fn clock_today() -> i64 {
    unix_now_ms() / crate::interpreter::value::MS_PER_DAY
        + crate::interpreter::value::AL_EPOCH_TO_UNIX_DAYS
}

/// Current wall-clock time as milliseconds since midnight UTC.
pub(crate) fn clock_time() -> i64 {
    unix_now_ms().rem_euclid(crate::interpreter::value::MS_PER_DAY)
}

/// Current date-time as milliseconds since the AL epoch (0001-01-01).
pub(crate) fn clock_current_datetime() -> i64 {
    unix_now_ms()
        + crate::interpreter::value::AL_EPOCH_TO_UNIX_DAYS * crate::interpreter::value::MS_PER_DAY
}

/// Monday = 1 … Sunday = 7 for an AL day count.
pub(super) fn weekday_of(date: i64) -> i64 {
    // 1970-01-01, day AL_EPOCH_TO_UNIX_DAYS, was a Thursday.
    let unix = date - crate::interpreter::value::AL_EPOCH_TO_UNIX_DAYS;
    (unix.rem_euclid(7) + 3) % 7 + 1
}

/// The ISO week number of an AL day count and the year that week belongs
/// to: the year of its Thursday.
pub(super) fn iso_week(date: i64) -> (i64, i64) {
    let thursday = date - weekday_of(date) + 4;
    let (year, _, _) = crate::interpreter::value::ymd_from_al_days(thursday);
    let week = (thursday - crate::interpreter::value::al_days_from_ymd(year, 1, 1)) / 7 + 1;
    (week, year)
}

/// `Date2DWY(date, what)` — 1: weekday (Monday = 1), 2: ISO week number,
/// 3: the year that week belongs to.
pub(super) fn builtin_date2dwy(args: &[Value]) -> Eval {
    let (date, what) = match args {
        [Value::Date(d), Value::Integer(w)] => (*d, *w),
        _ => return eval_error("Date2DWY expects (Date, Integer)"),
    };
    if date == 0 {
        return eval_error("Date2DWY is undefined for 0D");
    }
    let weekday = weekday_of(date);
    let (week, year) = iso_week(date);
    match what {
        1 => Eval::Normal(Value::Integer(weekday)),
        2 => Eval::Normal(Value::Integer(week)),
        3 => Eval::Normal(Value::Integer(year)),
        other => eval_error(format!(
            "Date2DWY: the what argument must be 1, 2 or 3, got {other}"
        )),
    }
}

/// `CalcDate(formula[, date])` for the invariant date formula language:
/// terms such as `+1D`, `-2W`, `3M`, `1Q`, `1Y`, and `CD`/`CW`/`CM`/`CQ`/`CY`
/// for the end of the current period (`-CM` for its start), applied left to
/// right (`<CM+1D>` is the first of next month). Without `date`, today.
pub(super) fn builtin_calcdate(args: &[Value]) -> Eval {
    let (formula, date) = match args {
        [Value::Text(f) | Value::Code(f)] => (f.as_str(), clock_today()),
        [Value::Text(f) | Value::Code(f), Value::Date(d)] => (f.as_str(), *d),
        _ => return eval_error("CalcDate expects (Text[, Date])"),
    };
    if date == 0 {
        return eval_error("CalcDate is undefined for 0D");
    }
    match calc_date(formula, date) {
        Ok(result) => Eval::Normal(Value::Date(result)),
        Err(error) => eval_error(format!("CalcDate: {error}")),
    }
}

fn calc_date(formula: &str, mut date: i64) -> Result<i64, String> {
    use crate::interpreter::value::{al_days_from_ymd, ymd_from_al_days};
    let text: String = formula
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if text.is_empty() {
        return Err(format!("'{formula}' is not a date formula"));
    }
    let days_in_month = |year: i64, month: i64| {
        let (next_year, next_month) = if month == 12 {
            (year + 1, 1)
        } else {
            (year, month + 1)
        };
        al_days_from_ymd(next_year, next_month, 1) - al_days_from_ymd(year, month, 1)
    };
    let add_months = |date: i64, months: i64| {
        let (year, month, day) = ymd_from_al_days(date);
        let index = year * 12 + (month - 1) + months;
        let (year, month) = (index.div_euclid(12), index.rem_euclid(12) + 1);
        al_days_from_ymd(year, month, day.min(days_in_month(year, month)))
    };
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let mut sign = 1;
        if chars[index] == '+' || chars[index] == '-' {
            if chars[index] == '-' {
                sign = -1;
            }
            index += 1;
        }
        let current = chars.get(index) == Some(&'C');
        if current {
            index += 1;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        let count: i64 = if start == index {
            1
        } else {
            chars[start..index]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        };
        let Some(&unit) = chars.get(index) else {
            return Err(format!("'{formula}' ends without a unit"));
        };
        index += 1;
        let (year, month, _) = ymd_from_al_days(date);
        date = if current {
            let forward = sign > 0;
            match unit {
                'D' => date,
                'W' => {
                    let weekday = weekday_of(date);
                    if forward {
                        date + (7 - weekday)
                    } else {
                        date - (weekday - 1)
                    }
                }
                'M' => {
                    if forward {
                        al_days_from_ymd(year, month, days_in_month(year, month))
                    } else {
                        al_days_from_ymd(year, month, 1)
                    }
                }
                'Q' => {
                    let first = (month - 1) / 3 * 3 + 1;
                    if forward {
                        let last = first + 2;
                        al_days_from_ymd(year, last, days_in_month(year, last))
                    } else {
                        al_days_from_ymd(year, first, 1)
                    }
                }
                'Y' => {
                    if forward {
                        al_days_from_ymd(year, 12, 31)
                    } else {
                        al_days_from_ymd(year, 1, 1)
                    }
                }
                other => return Err(format!("unsupported period 'C{other}' in '{formula}'")),
            }
        } else {
            let count = sign * count;
            match unit {
                'D' => date + count,
                'W' => date + 7 * count,
                'M' => add_months(date, count),
                'Q' => add_months(date, 3 * count),
                'Y' => add_months(date, 12 * count),
                other => return Err(format!("unsupported unit '{other}' in '{formula}'")),
            }
        };
    }
    Ok(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::interpreter::dispatch::test_support::{ctx, ok};

    #[test]
    fn date_builtins_round_trip() {
        let mut ctx = ctx();
        let date = crate::interpreter::value::al_days_from_ymd(2024, 7, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(1)],
                &mut ctx
            )),
            Value::Integer(31)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(2)],
                &mut ctx
            )),
            Value::Integer(7)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(3)],
                &mut ctx
            )),
            Value::Integer(2024)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DMY2Date",
                vec![Value::Integer(31), Value::Integer(7), Value::Integer(2024)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert!(dispatch_call(
            None,
            "DMY2Date",
            vec![Value::Integer(31), Value::Integer(2), Value::Integer(2024)],
            &mut ctx
        )
        .is_error());

        let dt = date * crate::interpreter::value::MS_PER_DAY + 3_600_000;
        assert_eq!(
            ok(dispatch_call(
                None,
                "DT2Date",
                vec![Value::DateTime(dt)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DT2Time",
                vec![Value::DateTime(dt)],
                &mut ctx
            )),
            Value::Time(3_600_000)
        );
    }

    #[test]
    fn workdate_defaults_to_today_and_is_settable() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(None, "WorkDate", vec![], &mut ctx)),
            Value::Date(clock_today()),
            "the session work date defaults to Today"
        );
        let date = crate::interpreter::value::al_days_from_ymd(2025, 1, 2);
        assert_eq!(
            ok(dispatch_call(
                None,
                "WorkDate",
                vec![Value::Date(date)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert_eq!(
            ok(dispatch_call(None, "WorkDate", vec![], &mut ctx)),
            Value::Date(date)
        );
    }
}
