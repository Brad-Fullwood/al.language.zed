//! AL runtime values for the pure-Rust interpreter.
//!
//! **Stability contract.** This enum is the boundary type between the
//! interpreter (Phase 2), the mock BC runtime (Phase 3), and any future
//! consumer (DAP variable inspection, snapshot replay). Variants here are
//! considered stable for parallel work; new variants may be added at the
//! end of the enum but existing variants must not be renamed or reordered.
//!
//! Variant set tracks AL's primitive + structured value space:
//!  * scalars: Integer, Decimal, Boolean, Char, Date, Time, DateTime,
//!    Duration, Guid, Text, Code
//!  * sentinels: Null, Empty (uninitialised vs. cleared)
//!  * structured: Option, Record, RecordRef, Variant, Array, List, Dict,
//!    Blob, Stream, ErrorInfo

use std::collections::BTreeMap;

/// AL `Date` carrier: days since 0001-01-01 (CLR `DateTime.Ticks` style is
/// overkill here — Phase 2 just needs ordering and arithmetic).
pub type AlDate = i64;
/// AL `Time` carrier: milliseconds since midnight, in `[0, 86_400_000)`.
pub type AlTime = i64;
/// AL `DateTime` carrier: milliseconds since the AL epoch (0001-01-01).
pub type AlDateTime = i64;

/// One AL runtime value.
///
/// `PartialEq` is implemented manually (below) to mirror the `Ord` total
/// ordering — `Decimal` uses `f64::total_cmp` so NaN equals NaN, satisfying
/// the `Eq` contract `a == a`. This keeps the three impls (PartialEq / Eq /
/// Ord) consistent and lets `Value` be a valid `BTreeMap` key.
#[derive(Debug, Clone)]
pub enum Value {
    /// Uninitialised slot (before assignment).
    Null,
    /// Cleared / default-initialised value (`Clear(x)` semantics).
    Empty,
    /// AL `Integer` / `BigInteger` — i64 wide enough for both.
    Integer(i64),
    /// AL `Decimal` — fixed-precision via string form for now (Phase 2
    /// uses f64; Phase 3 may upgrade to a proper decimal type).
    Decimal(f64),
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
    /// AL `Guid` — opaque 16-byte identifier (string form for now).
    Guid(String),
    /// AL `Option` — name + ordinal pair.
    Option {
        /// The option-set name (e.g. `Status`).
        type_name: String,
        /// The selected member name (e.g. `Open`).
        member: String,
        ordinal: i64,
    },
    /// AL `Record` — boxed handle into the in-memory table store.
    /// Phase 3 fills in the inner type; Phase 2 uses the placeholder shape.
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
}

/// In-memory record handle. Phase 2 uses an opaque key into a per-thread
/// table store managed by Phase 3's `mock::record::MockRecord`. Until
/// Phase 3 lands, the interpreter constructs these only as placeholders.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordValue {
    pub table_name: String,
    pub table_id: i32,
    /// Opaque handle into the mock table store. `None` means "no current
    /// record" (e.g. after `Reset` and before `FindFirst`).
    pub handle: Option<u64>,
}

// Variants are ordered by their declaration index, then within each variant
// by an obvious natural order. Decimal uses `f64::total_cmp` so NaN sorts
// consistently. `Variant`/`Array`/`List`/`Dict`/`Blob`/`ErrorInfo` are
// never used as primary-key components in BC, so their orderings are
// implementation-defined (length-then-content) — adequate for BTreeMap
// stability without committing to an external contract.

/// Manual `PartialEq` mirroring the `Ord` impl so the three trait impls
/// stay consistent. `Decimal(NaN) == Decimal(NaN)` is `true` here (via
/// `total_cmp`), satisfying the `Eq` contract `a == a`.
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
                Integer(_) => 2,
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
            }
        }
        let mine = variant_index(self);
        let theirs = variant_index(other);
        if mine != theirs {
            return mine.cmp(&theirs);
        }
        match (self, other) {
            (Null, Null) | (Empty, Empty) => Ordering::Equal,
            (Integer(a), Integer(b)) => a.cmp(b),
            (Decimal(a), Decimal(b)) => a.total_cmp(b),
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

    /// Default value for the named AL type. Returns `None` if the type
    /// name is unknown to the interpreter.
    pub fn default_for(type_name: &str) -> Option<Value> {
        match type_name.to_ascii_lowercase().as_str() {
            "integer" | "biginteger" => Some(Value::Integer(0)),
            "decimal" => Some(Value::Decimal(0.0)),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_is_strict() {
        assert!(Value::Boolean(true).is_truthy());
        assert!(!Value::Boolean(false).is_truthy());
        // AL does not auto-coerce; even non-zero integer is NOT truthy.
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
        // Negative: an unknown type name yields None — caller decides
        // whether to error or fall back.
        assert!(Value::default_for("DefinitelyNotAnALType").is_none());
        assert!(Value::default_for("").is_none());
    }

    #[test]
    fn type_name_is_stable() {
        // The strings here are part of the stability contract: they
        // appear in DAP variable inspection and error messages.
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
    fn default_for_code_returns_code_variant_adversarial_h_1() {
        // FINDING P2 wrong-result: default_for("code") returns Value::Text,
        // not Value::Code. Match arm at value.rs:222:
        //   "text" | "code" => Some(Value::Text(String::new()))
        // The Code arm should produce Value::Code, not Value::Text.
        // Expected: Some(Value::Code(""))
        // Observed: Some(Value::Text(""))
        assert!(
            matches!(Value::default_for("Code"), Some(Value::Code(s)) if s.is_empty()),
            "default_for(\"Code\") must return Value::Code, got: {:?}",
            Value::default_for("Code")
        );
    }

    #[test]
    fn decimal_ord_transitivity_total_cmp_no_bug_adversarial_h_2() {
        // AUDIT (no bug): Decimal Ord uses f64::total_cmp — a TOTAL ORDER.
        // IEEE 754-2008 totalOrder places positive NaN AFTER all finite values
        // (NaN > +Inf > ... > +0 > -0 > ... > -Inf > negative NaN).
        // So Value::Decimal(NaN) > Value::Decimal(1.0). Transitivity holds.
        // Kill attempt confirmed: no Ord violation exists.
        let nan = Value::Decimal(f64::NAN);
        let one = Value::Decimal(1.0);
        let two = Value::Decimal(2.0);
        // total_cmp: NaN > all finite values (NaN sorts as maximum)
        assert!(nan > one, "NaN > 1.0 under total_cmp (NaN is largest)");
        assert!(one < two);
        // transitivity: nan > two && two > one => nan > one (already checked)
        assert!(nan > two, "transitivity: NaN > two && two > one");
        assert_eq!(
            nan.cmp(&Value::Decimal(f64::NAN)),
            std::cmp::Ordering::Equal,
            "NaN == NaN under total_cmp (consistent sentinel)"
        );
    }

    #[test]
    fn default_for_covers_every_supported_type() {
        use std::cmp::Ordering;
        // biginteger aliases integer.
        assert_eq!(Value::default_for("biginteger"), Some(Value::Integer(0)));
        // Decimal default is 0.0 (not NaN, not unset).
        assert_eq!(Value::default_for("decimal"), Some(Value::Decimal(0.0)));
        assert_eq!(Value::default_for("date"), Some(Value::Date(0)));
        assert_eq!(Value::default_for("time"), Some(Value::Time(0)));
        assert_eq!(Value::default_for("datetime"), Some(Value::DateTime(0)));
        assert_eq!(Value::default_for("duration"), Some(Value::Duration(0)));
        assert_eq!(Value::default_for("char"), Some(Value::Char('\0')));
        assert!(matches!(Value::default_for("code"), Some(Value::Code(s)) if s.is_empty()));
        // Guid default is the AL nil GUID.
        match Value::default_for("guid") {
            Some(Value::Guid(g)) => {
                assert_eq!(g, "00000000-0000-0000-0000-000000000000");
            }
            other => panic!("guid default wrong: {other:?}"),
        }
        // Case-insensitive: mixed-case names resolve identically.
        assert_eq!(
            Value::default_for("DateTime").cmp(&Value::default_for("datetime")),
            Ordering::Equal
        );
    }

    #[test]
    fn cross_variant_order_follows_declaration_index() {
        // Variants are ordered by their declaration index regardless of inner
        // payload. Null(0) < Empty(1) < Integer(2) < ... < ErrorInfo(21).
        // A huge integer must still sort BELOW a tiny decimal, because the
        // variant index dominates the inner value.
        let huge_int = Value::Integer(i64::MAX);
        let tiny_dec = Value::Decimal(-1.0e300);
        assert!(
            huge_int < tiny_dec,
            "Integer variant precedes Decimal variant"
        );

        let ascending = vec![
            Value::Null,
            Value::Empty,
            Value::Integer(0),
            Value::Decimal(0.0),
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
        assert!(Value::Decimal(1.5) < Value::Decimal(2.5));
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
        // Ordinal dominates: a "Zzz" member with ordinal 0 sorts before an
        // "Aaa" member with ordinal 1, because ordinal is the primary key.
        assert!(opt("Status", "Zzz", 0) < opt("Status", "Aaa", 1));
        // Same ordinal: fall back to type_name, then member.
        assert!(opt("AStatus", "X", 5) < opt("BStatus", "X", 5));
        assert!(opt("Status", "Aaa", 5) < opt("Status", "Bbb", 5));
        // Equal triple => Equal.
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
        // table_id is primary: lower id sorts first even with a "later" name.
        assert!(rec(18, "Zebra", Some(99)) < rec(27, "Aardvark", Some(1)));
        // Same id: compare table_name.
        assert!(rec(18, "Apple", None) < rec(18, "Banana", None));
        // Same id + name: compare handle (None < Some).
        assert!(rec(18, "Customer", None) < rec(18, "Customer", Some(0)));
        assert!(rec(18, "Customer", Some(1)) < rec(18, "Customer", Some(2)));
        // RecordRef uses the identical comparison path.
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
        // Array / List use lexicographic Vec ordering.
        assert!(
            Value::Array(vec![Value::Integer(1)])
                < Value::Array(vec![Value::Integer(1), Value::Integer(0)])
        );
        assert!(Value::List(vec![Value::Integer(1)]) < Value::List(vec![Value::Integer(2)]));
        // Blob uses byte-vector ordering.
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
        // Adding a second entry makes the longer sequence sort after.
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
        // Only the message participates in ordering; error_type/source ignored.
        assert!(err("aaa") < err("bbb"));
        assert_eq!(err("same"), err("same"));
    }

    #[test]
    fn eq_mirrors_cmp_including_nan_self_equality() {
        // Eq contract a == a must hold even for NaN decimals (total_cmp).
        let nan = Value::Decimal(f64::NAN);
        #[allow(clippy::eq_op)]
        let nan_self_eq = nan == nan;
        assert!(nan_self_eq, "Decimal(NaN) must equal itself to satisfy Eq");
        assert_ne!(Value::Integer(0), Value::Decimal(0.0));
        assert_eq!(
            Value::Integer(1).partial_cmp(&Value::Integer(2)),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn type_name_covers_structured_variants() {
        assert_eq!(Value::Null.type_name(), "Null");
        assert_eq!(Value::Empty.type_name(), "Empty");
        assert_eq!(Value::Decimal(0.0).type_name(), "Decimal");
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
    fn value_is_usable_as_btreemap_key() {
        // The whole point of the total Ord impl: Value must work as a key.
        let mut map: BTreeMap<Value, &str> = BTreeMap::new();
        map.insert(Value::Integer(2), "two");
        map.insert(Value::Integer(1), "one");
        map.insert(Value::Decimal(f64::NAN), "nan");
        // Lookups round-trip, including the NaN key (total_cmp makes it stable).
        assert_eq!(map.get(&Value::Integer(1)), Some(&"one"));
        assert_eq!(map.get(&Value::Decimal(f64::NAN)), Some(&"nan"));
        let keys: Vec<_> = map.keys().cloned().collect();
        assert!(keys[0] < keys[1]);
    }
}
