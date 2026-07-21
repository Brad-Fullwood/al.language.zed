//! AL runtime values for the pure-Rust interpreter.
//!
//! **Stability contract.** This enum is the boundary type between the
//! interpreter, mock BC runtime, and consumers such as DAP variable inspection
//! and test execution. Variants here are considered stable for parallel work;
//! new variants may be appended, but existing variants must not be renamed or
//! reordered.
//!
//! Variant set tracks AL's primitive + structured value space:
//!  * scalars: Integer, Decimal, Boolean, Char, Date, Time, DateTime,
//!    Duration, Guid, Text, Code
//!  * sentinels: Null, Empty (uninitialised vs. cleared)
//!  * structured: Option, Record, RecordRef, Variant, Array, List, Dict,
//!    Blob, Stream, ErrorInfo

use std::collections::BTreeMap;

pub use rust_decimal::Decimal;

/// AL `Date` carrier: days since 0001-01-01.
pub type AlDate = i64;
/// AL `Time` carrier: milliseconds since midnight, in `[0, 86_400_000)`.
pub type AlTime = i64;
/// AL `DateTime` carrier: milliseconds since the AL epoch (0001-01-01).
pub type AlDateTime = i64;

/// Milliseconds in one day — the conversion factor between the `Date` (day)
/// and `DateTime` (millisecond) carriers.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Days from the AL epoch (0001-01-01) to the Unix epoch (1970-01-01).
/// Used to bridge the system clock (Unix-based) and the AL `Date`/`DateTime`
/// carriers (0001-01-01-based). The value is the standard proleptic-Gregorian
/// offset (`days_from_civil(1,1,1) == -719162`).
pub const AL_EPOCH_TO_UNIX_DAYS: i64 = 719_162;

/// Days since the Unix epoch (1970-01-01) for a proleptic-Gregorian
/// year/month/day. Howard Hinnant's `days_from_civil` algorithm — exact for
/// all `i64` years, no leap-year edge cases. `month` is 1..=12, `day` 1..=31.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Days since the AL epoch (0001-01-01) for a year/month/day. `0001-01-01`
/// maps to day 0; `1970-01-01` maps to [`AL_EPOCH_TO_UNIX_DAYS`].
pub fn al_days_from_ymd(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) + AL_EPOCH_TO_UNIX_DAYS
}

/// One AL runtime value.
///
/// `PartialEq` is implemented manually (below) to mirror the `Ord` total
/// ordering. `Decimal` is an exact 96-bit decimal (`rust_decimal`) with a
/// total `Ord` and no NaN, so equality is exact and the three impls
/// (PartialEq / Eq / Ord) stay consistent, letting `Value` be a valid
/// `BTreeMap` key.
#[derive(Debug, Clone)]
pub enum Value {
    /// Uninitialised slot (before assignment).
    Null,
    /// Cleared / default-initialised value (`Clear(x)` semantics).
    Empty,
    /// AL `Integer` — 32-bit signed. Stored in an i64 carrier, but arithmetic
    /// traps at the i32 range to match BC (see `apply_binary`). Distinct from
    /// [`Value::BigInteger`] so the interpreter can apply the right overflow
    /// width; the two compare equal by numeric value.
    Integer(i64),
    /// AL `Decimal` — exact 96-bit decimal (`rust_decimal::Decimal`), matching
    /// BC's `System.Decimal`: no binary-float drift and no NaN or infinity.
    Decimal(Decimal),
    Boolean(bool),
    /// AL `Char` — single Unicode code point.
    Char(char),
    /// AL `Text[N]` — variable-length string, length cap not enforced here.
    Text(String),
    /// AL `Code[N]` — uppercase string, length cap not enforced here.
    Code(String),
    /// AL `Date` — days since AL epoch (0001-01-01).
    Date(AlDate),
    /// AL `Time` — milliseconds since midnight.
    Time(AlTime),
    /// AL `DateTime` — milliseconds since AL epoch.
    DateTime(AlDateTime),
    /// AL `Duration` — milliseconds.
    Duration(i64),
    /// AL `Guid` — opaque identifier stored in string form.
    Guid(String),
    /// AL `Option` — name + ordinal pair.
    Option {
        /// The option-set name (e.g. `Status`).
        type_name: String,
        /// The selected member name (e.g. `Open`).
        member: String,
        ordinal: i64,
    },
    /// AL `Record` — handle into the in-memory table store.
    Record(RecordValue),
    RecordRef(RecordValue),
    /// AL `Variant` — tagged any-value.
    Variant(Box<Value>),
    /// AL `array[N]` of homogeneous values.
    Array(Vec<Value>),
    List(Vec<Value>),
    /// AL `Dictionary of [K, V]` — keyed by serialised K.
    Dict(BTreeMap<String, Value>),
    /// AL `Blob` / `InStream` / `OutStream` — raw bytes.
    Blob(Vec<u8>),
    /// AL `ErrorInfo` — structured error captured by `asserterror` / `Error`.
    ErrorInfo(Box<ErrorInfo>),
    /// AL `Codeunit <Subtype>` instance handle. The interpreter is BC-free, so
    /// a codeunit variable carries only the declared subtype's object name; a
    /// method call on it (`MyCu.DoStuff(...)`) is dispatched to that workspace
    /// object's procedure. Added at the end of the enum to preserve the
    /// variant-ordering stability contract documented above.
    Codeunit {
        /// The declared subtype object name (e.g. `"Library - Sales"`).
        object_name: String,
    },
    /// AL `BigInteger` — 64-bit signed. Appended to the enum to preserve the
    /// variant-ordering stability contract; it is treated as the same numeric
    /// class as [`Value::Integer`] for comparison (see `variant_index`), and
    /// arithmetic on it traps only at the i64 range.
    BigInteger(i64),
}

/// In-memory record handle backed by `mock::record::MockRecord`.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordValue {
    pub table_name: String,
    pub table_id: i32,
    /// Opaque handle into the mock table store. `None` means "no current
    /// record" (e.g. after `Reset` and before `FindFirst`).
    pub handle: Option<u64>,
}

// Variants are ordered by their declaration index, then within each variant
// by an obvious natural order. `Decimal` has an exact total `Ord`.
// `Variant`/`Array`/`List`/`Dict`/`Blob`/`ErrorInfo` are never used as
// primary-key components in BC, so their orderings are implementation-defined
// (length-then-content) — adequate for BTreeMap stability without committing
// to an external contract.

/// Manual `PartialEq` mirroring the `Ord` impl so the three trait impls
/// stay consistent.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Value {}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        use Value::*;
        fn variant_index(v: &Value) -> u8 {
            match v {
                Null => 0,
                Empty => 1,
                // Integer and BigInteger are one numeric class: same index so
                // they order by value, and `5 = 5L` holds.
                Integer(_) | BigInteger(_) => 2,
                Decimal(_) => 3,
                Boolean(_) => 4,
                Char(_) => 5,
                Text(_) => 6,
                Code(_) => 7,
                Date(_) => 8,
                Time(_) => 9,
                DateTime(_) => 10,
                Duration(_) => 11,
                Guid(_) => 12,
                Option { .. } => 13,
                Record(_) => 14,
                RecordRef(_) => 15,
                Variant(_) => 16,
                Array(_) => 17,
                List(_) => 18,
                Dict(_) => 19,
                Blob(_) => 20,
                ErrorInfo(_) => 21,
                Codeunit { .. } => 22,
            }
        }
        let mine = variant_index(self);
        let theirs = variant_index(other);
        if mine != theirs {
            return mine.cmp(&theirs);
        }
        match (self, other) {
            (Null, Null) | (Empty, Empty) => Ordering::Equal,
            (Integer(a) | BigInteger(a), Integer(b) | BigInteger(b)) => a.cmp(b),
            (Decimal(a), Decimal(b)) => a.cmp(b),
            (Boolean(a), Boolean(b)) => a.cmp(b),
            (Char(a), Char(b)) => a.cmp(b),
            (Text(a), Text(b)) | (Code(a), Code(b)) => a.cmp(b),
            (Date(a), Date(b)) | (Time(a), Time(b)) | (DateTime(a), DateTime(b)) => a.cmp(b),
            (Duration(a), Duration(b)) => a.cmp(b),
            (Guid(a), Guid(b)) => a.cmp(b),
            (
                Option {
                    type_name: at,
                    member: am,
                    ordinal: ao,
                },
                Option {
                    type_name: bt,
                    member: bm,
                    ordinal: bo,
                },
            ) => (ao, at, am).cmp(&(bo, bt, bm)),
            (Record(a), Record(b)) | (RecordRef(a), RecordRef(b)) => {
                (a.table_id, &a.table_name, a.handle).cmp(&(b.table_id, &b.table_name, b.handle))
            }
            (Variant(a), Variant(b)) => a.cmp(b),
            (Array(a), Array(b)) | (List(a), List(b)) => a.cmp(b),
            (Dict(a), Dict(b)) => a
                .iter()
                .collect::<Vec<_>>()
                .cmp(&b.iter().collect::<Vec<_>>()),
            (Blob(a), Blob(b)) => a.cmp(b),
            (ErrorInfo(a), ErrorInfo(b)) => a.message.cmp(&b.message),
            (Codeunit { object_name: a }, Codeunit { object_name: b }) => a.cmp(b),
            // Different variants handled by the index check above.
            _ => Ordering::Equal,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Captured `Error()` / `asserterror` payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ErrorInfo {
    pub message: String,
    /// Optional `ErrorType` enum member name (e.g. `Internal`).
    pub error_type: Option<String>,
    /// Optional source codeunit/procedure for diagnostic purposes.
    pub source: Option<String>,
}

impl Value {
    /// Truthiness for AL `IF`/`WHILE` semantics: only `Boolean(true)` is
    /// truthy. Numbers, strings, etc. are NOT auto-coerced (AL is strict).
    pub fn is_truthy(&self) -> bool {
        matches!(self, Value::Boolean(true))
    }

    /// The i64 payload of an `Integer` or `BigInteger`, else `None`. Use this
    /// wherever a consumer treats the two integer types interchangeably
    /// (filters, field compares, coercions) rather than matching each variant.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Integer(n) | Value::BigInteger(n) => Some(*n),
            _ => None,
        }
    }

    /// Numeric view as an exact `Decimal`: `Integer`/`BigInteger` promote
    /// losslessly (i64 fits the 96-bit range), `Decimal` passes through, and
    /// everything else is `None`. The single conversion used by numeric
    /// comparison, FlowField aggregation, and the assert helpers.
    pub fn as_decimal(&self) -> Option<Decimal> {
        match self {
            Value::Integer(n) | Value::BigInteger(n) => Some(Decimal::from(*n)),
            Value::Decimal(d) => Some(*d),
            _ => None,
        }
    }

    /// Coerce `incoming` to the declared type of the `slot` it is being assigned
    /// into. AL variables have a fixed type, so an assignment preserves the
    /// slot's type rather than adopting the RHS's:
    ///
    /// * a `Code` slot uppercases a string RHS and stays caseless `Code`;
    /// * an `Integer`/`BigInteger` slot keeps its width so later arithmetic uses
    ///   the right overflow trap.
    ///
    /// Any other combination overwrites as-is. Shared by both assignment paths
    /// (`eval_assignment` and the expression-form handler in `eval_expr`).
    pub(crate) fn coerce_into_slot(slot: &Value, incoming: Value) -> Value {
        match (slot, &incoming) {
            (Value::Code(_), Value::Text(s) | Value::Code(s)) => Value::Code(s.to_uppercase()),
            (Value::BigInteger(_), Value::Integer(n) | Value::BigInteger(n)) => {
                Value::BigInteger(*n)
            }
            (Value::Integer(_), Value::Integer(n) | Value::BigInteger(n)) => Value::Integer(*n),
            _ => incoming,
        }
    }

    /// Default value for the named AL type. Returns `None` if the type
    /// name is unknown to the interpreter.
    pub fn default_for(type_name: &str) -> Option<Value> {
        match type_name.to_ascii_lowercase().as_str() {
            "integer" => Some(Value::Integer(0)),
            "biginteger" => Some(Value::BigInteger(0)),
            "decimal" => Some(Value::Decimal(Decimal::ZERO)),
            "boolean" => Some(Value::Boolean(false)),
            "text" => Some(Value::Text(String::new())),
            "code" => Some(Value::Code(String::new())),
            "date" => Some(Value::Date(0)),
            "time" => Some(Value::Time(0)),
            "datetime" => Some(Value::DateTime(0)),
            "duration" => Some(Value::Duration(0)),
            "guid" => Some(Value::Guid("00000000-0000-0000-0000-000000000000".into())),
            "char" => Some(Value::Char('\0')),
            _ => None,
        }
    }

    /// Short type-name for diagnostic output. Stable identifiers; do not
    /// depend on these for parsing.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Empty => "Empty",
            Value::Integer(_) => "Integer",
            Value::BigInteger(_) => "BigInteger",
            Value::Decimal(_) => "Decimal",
            Value::Boolean(_) => "Boolean",
            Value::Char(_) => "Char",
            Value::Text(_) => "Text",
            Value::Code(_) => "Code",
            Value::Date(_) => "Date",
            Value::Time(_) => "Time",
            Value::DateTime(_) => "DateTime",
            Value::Duration(_) => "Duration",
            Value::Guid(_) => "Guid",
            Value::Option { .. } => "Option",
            Value::Record(_) => "Record",
            Value::RecordRef(_) => "RecordRef",
            Value::Variant(_) => "Variant",
            Value::Array(_) => "Array",
            Value::List(_) => "List",
            Value::Dict(_) => "Dict",
            Value::Blob(_) => "Blob",
            Value::ErrorInfo(_) => "ErrorInfo",
            Value::Codeunit { .. } => "Codeunit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn truthiness_is_strict() {
        assert!(Value::Boolean(true).is_truthy());
        assert!(!Value::Boolean(false).is_truthy());
        assert!(!Value::Integer(1).is_truthy());
        assert!(!Value::Text("anything".into()).is_truthy());
        assert!(!Value::Null.is_truthy());
    }

    #[test]
    fn default_for_returns_zero_value() {
        assert_eq!(Value::default_for("Integer"), Some(Value::Integer(0)));
        assert_eq!(Value::default_for("integer"), Some(Value::Integer(0)));
        assert_eq!(Value::default_for("Boolean"), Some(Value::Boolean(false)));
        assert!(matches!(Value::default_for("Text"), Some(Value::Text(s)) if s.is_empty()));
    }

    #[test]
    fn default_for_unknown_returns_none() {
        assert!(Value::default_for("DefinitelyNotAnALType").is_none());
        assert!(Value::default_for("").is_none());
    }

    #[test]
    fn type_name_is_stable() {
        assert_eq!(Value::Integer(0).type_name(), "Integer");
        assert_eq!(Value::Boolean(true).type_name(), "Boolean");
        assert_eq!(
            Value::Option {
                type_name: "S".into(),
                member: "X".into(),
                ordinal: 0
            }
            .type_name(),
            "Option"
        );
    }

    #[test]
    fn default_for_code_returns_code_variant() {
        assert!(
            matches!(Value::default_for("Code"), Some(Value::Code(s)) if s.is_empty()),
            "default_for(\"Code\") must return Value::Code, got: {:?}",
            Value::default_for("Code")
        );
    }

    #[test]
    fn decimal_is_exact_no_binary_float_drift() {
        assert_eq!(
            Value::Decimal(dec!(0.1) + dec!(0.2)),
            Value::Decimal(dec!(0.3)),
            "0.1 + 0.2 must equal 0.3 exactly"
        );
        let one = Value::Decimal(dec!(1.0));
        let two = Value::Decimal(dec!(2.0));
        assert!(one < two);
        assert_eq!(
            one.cmp(&Value::Decimal(dec!(1.0))),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn default_for_covers_every_supported_type() {
        use std::cmp::Ordering;
        assert_eq!(Value::default_for("biginteger"), Some(Value::BigInteger(0)));
        assert_eq!(Value::BigInteger(0), Value::Integer(0));
        assert_eq!(
            Value::default_for("decimal"),
            Some(Value::Decimal(Decimal::ZERO))
        );
        assert_eq!(Value::default_for("date"), Some(Value::Date(0)));
        assert_eq!(Value::default_for("time"), Some(Value::Time(0)));
        assert_eq!(Value::default_for("datetime"), Some(Value::DateTime(0)));
        assert_eq!(Value::default_for("duration"), Some(Value::Duration(0)));
        assert_eq!(Value::default_for("char"), Some(Value::Char('\0')));
        assert!(matches!(Value::default_for("code"), Some(Value::Code(s)) if s.is_empty()));
        match Value::default_for("guid") {
            Some(Value::Guid(g)) => {
                assert_eq!(g, "00000000-0000-0000-0000-000000000000");
            }
            other => panic!("guid default wrong: {other:?}"),
        }
        assert_eq!(
            Value::default_for("DateTime").cmp(&Value::default_for("datetime")),
            Ordering::Equal
        );
    }

    #[test]
    fn cross_variant_order_follows_declaration_index() {
        let huge_int = Value::Integer(i64::MAX);
        let tiny_dec = Value::Decimal(dec!(-1000000));
        assert!(
            huge_int < tiny_dec,
            "Integer variant precedes Decimal variant"
        );

        let ascending = vec![
            Value::Null,
            Value::Empty,
            Value::Integer(0),
            Value::Decimal(Decimal::ZERO),
            Value::Boolean(false),
            Value::Char('\0'),
            Value::Text(String::new()),
            Value::Code(String::new()),
            Value::Date(0),
            Value::Time(0),
            Value::DateTime(0),
            Value::Duration(0),
            Value::Guid(String::new()),
            Value::Option {
                type_name: String::new(),
                member: String::new(),
                ordinal: 0,
            },
            Value::Record(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
            }),
            Value::RecordRef(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
            }),
            Value::Variant(Box::new(Value::Null)),
            Value::Array(vec![]),
            Value::List(vec![]),
            Value::Dict(BTreeMap::new()),
            Value::Blob(vec![]),
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: String::new(),
                error_type: None,
                source: None,
            })),
        ];
        for win in ascending.windows(2) {
            assert!(
                win[0] < win[1],
                "{} must sort before {}",
                win[0].type_name(),
                win[1].type_name()
            );
        }
    }

    #[test]
    fn within_variant_scalar_ordering() {
        assert!(Value::Integer(-5) < Value::Integer(5));
        assert!(Value::Decimal(dec!(1.5)) < Value::Decimal(dec!(2.5)));
        assert!(Value::Boolean(false) < Value::Boolean(true));
        assert!(Value::Char('a') < Value::Char('z'));
        assert!(Value::Text("apple".into()) < Value::Text("banana".into()));
        assert!(Value::Code("AAA".into()) < Value::Code("AAB".into()));
        assert!(Value::Date(1) < Value::Date(2));
        assert!(Value::Time(100) < Value::Time(200));
        assert!(Value::DateTime(10) < Value::DateTime(20));
        assert!(Value::Duration(-1) < Value::Duration(1));
        assert!(Value::Guid("a".into()) < Value::Guid("b".into()));
    }

    #[test]
    fn option_orders_by_ordinal_first() {
        let opt = |t: &str, m: &str, o: i64| Value::Option {
            type_name: t.into(),
            member: m.into(),
            ordinal: o,
        };
        assert!(opt("Status", "Zzz", 0) < opt("Status", "Aaa", 1));
        assert!(opt("AStatus", "X", 5) < opt("BStatus", "X", 5));
        assert!(opt("Status", "Aaa", 5) < opt("Status", "Bbb", 5));
        assert_eq!(opt("S", "M", 3), opt("S", "M", 3));
    }

    #[test]
    fn record_orders_by_table_id_then_name_then_handle() {
        let rec = |id: i32, name: &str, handle: Option<u64>| {
            Value::Record(RecordValue {
                table_name: name.into(),
                table_id: id,
                handle,
            })
        };
        assert!(rec(18, "Zebra", Some(99)) < rec(27, "Aardvark", Some(1)));
        assert!(rec(18, "Apple", None) < rec(18, "Banana", None));
        assert!(rec(18, "Customer", None) < rec(18, "Customer", Some(0)));
        assert!(rec(18, "Customer", Some(1)) < rec(18, "Customer", Some(2)));
        let rref = |id: i32| {
            Value::RecordRef(RecordValue {
                table_name: "T".into(),
                table_id: id,
                handle: None,
            })
        };
        assert!(rref(1) < rref(2));
    }

    #[test]
    fn structured_collection_ordering() {
        assert!(
            Value::Variant(Box::new(Value::Integer(1)))
                < Value::Variant(Box::new(Value::Integer(2)))
        );
        assert!(
            Value::Array(vec![Value::Integer(1)])
                < Value::Array(vec![Value::Integer(1), Value::Integer(0)])
        );
        assert!(Value::List(vec![Value::Integer(1)]) < Value::List(vec![Value::Integer(2)]));
        assert!(Value::Blob(vec![1, 2]) < Value::Blob(vec![1, 3]));
        assert!(Value::Blob(vec![1]) < Value::Blob(vec![1, 0]));
    }

    #[test]
    fn dict_ordering_compares_entry_sequences() {
        let mut a = BTreeMap::new();
        a.insert("k1".to_string(), Value::Integer(1));
        let mut b = BTreeMap::new();
        b.insert("k1".to_string(), Value::Integer(2));
        assert!(Value::Dict(a.clone()) < Value::Dict(b));
        let mut c = a.clone();
        c.insert("k2".to_string(), Value::Integer(0));
        assert!(Value::Dict(a) < Value::Dict(c));
    }

    #[test]
    fn error_info_orders_by_message() {
        let err = |msg: &str| {
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: msg.into(),
                error_type: Some("Internal".into()),
                source: Some("CU 50000".into()),
            }))
        };
        assert!(err("aaa") < err("bbb"));
        assert_eq!(err("same"), err("same"));
    }

    #[test]
    fn eq_mirrors_cmp_across_variants() {
        let d = Value::Decimal(dec!(1.25));
        assert_eq!(d, d.clone(), "a decimal must equal itself");
        assert_ne!(Value::Integer(0), Value::Decimal(Decimal::ZERO));
        assert_eq!(
            Value::Integer(1).partial_cmp(&Value::Integer(2)),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn type_name_covers_structured_variants() {
        assert_eq!(Value::Null.type_name(), "Null");
        assert_eq!(Value::Empty.type_name(), "Empty");
        assert_eq!(Value::Decimal(Decimal::ZERO).type_name(), "Decimal");
        assert_eq!(Value::Char('x').type_name(), "Char");
        assert_eq!(Value::Text(String::new()).type_name(), "Text");
        assert_eq!(Value::Code(String::new()).type_name(), "Code");
        assert_eq!(Value::Date(0).type_name(), "Date");
        assert_eq!(Value::Time(0).type_name(), "Time");
        assert_eq!(Value::DateTime(0).type_name(), "DateTime");
        assert_eq!(Value::Duration(0).type_name(), "Duration");
        assert_eq!(Value::Guid(String::new()).type_name(), "Guid");
        assert_eq!(Value::Variant(Box::new(Value::Null)).type_name(), "Variant");
        assert_eq!(Value::Array(vec![]).type_name(), "Array");
        assert_eq!(Value::List(vec![]).type_name(), "List");
        assert_eq!(Value::Dict(BTreeMap::new()).type_name(), "Dict");
        assert_eq!(Value::Blob(vec![]).type_name(), "Blob");
        assert_eq!(
            Value::Record(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
            })
            .type_name(),
            "Record"
        );
        assert_eq!(
            Value::RecordRef(RecordValue {
                table_name: String::new(),
                table_id: 0,
                handle: None,
            })
            .type_name(),
            "RecordRef"
        );
        assert_eq!(
            Value::ErrorInfo(Box::new(ErrorInfo {
                message: String::new(),
                error_type: None,
                source: None,
            }))
            .type_name(),
            "ErrorInfo"
        );
    }

    #[test]
    fn al_date_math_known_anchors() {
        assert_eq!(al_days_from_ymd(1, 1, 1), 0);
        assert_eq!(al_days_from_ymd(1970, 1, 1), AL_EPOCH_TO_UNIX_DAYS);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1, 1, 1), -AL_EPOCH_TO_UNIX_DAYS);
    }

    #[test]
    fn al_date_math_month_lengths_and_leap_years() {
        assert_eq!(
            al_days_from_ymd(2024, 7, 1) - al_days_from_ymd(2024, 6, 1),
            30
        );
        assert_eq!(
            al_days_from_ymd(2024, 3, 1) - al_days_from_ymd(2024, 2, 1),
            29
        );
        assert_eq!(
            al_days_from_ymd(2023, 3, 1) - al_days_from_ymd(2023, 2, 1),
            28
        );
        assert_eq!(
            al_days_from_ymd(2024, 1, 1) - al_days_from_ymd(2023, 1, 1),
            365
        );
    }

    #[test]
    fn value_is_usable_as_btreemap_key() {
        let mut map: BTreeMap<Value, &str> = BTreeMap::new();
        map.insert(Value::Integer(2), "two");
        map.insert(Value::Integer(1), "one");
        map.insert(Value::Decimal(dec!(3.5)), "dec");
        assert_eq!(map.get(&Value::Integer(1)), Some(&"one"));
        assert_eq!(map.get(&Value::Decimal(dec!(3.5))), Some(&"dec"));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert!(keys[0] < keys[1]);
    }
}
