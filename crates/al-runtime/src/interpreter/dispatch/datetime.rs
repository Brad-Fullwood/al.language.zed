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
