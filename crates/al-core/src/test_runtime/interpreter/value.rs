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
    /// AL `Boolean`.
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
        /// The numeric ordinal.
        ordinal: i64,
    },
    /// AL `Record` — boxed handle into the in-memory table store.
    /// Phase 3 fills in the inner type; Phase 2 uses the placeholder shape.
    Record(RecordValue),
    /// AL `RecordRef` — dynamic record reference.
    RecordRef(RecordValue),
    /// AL `Variant` — tagged any-value.
    Variant(Box<Value>),
    /// AL `array[N]` of homogeneous values.
    Array(Vec<Value>),
    /// AL `List of [T]`.
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
    /// AL table object name.
    pub table_name: String,
    /// AL table object ID.
    pub table_id: i32,
    /// Opaque handle into the mock table store. `None` means "no current
    /// record" (e.g. after `Reset` and before `FindFirst`).
    pub handle: Option<u64>,
}

// ---------------------------------------------------------------------------
// Total ordering for BTreeMap keys.
//
// Variants are ordered by their declaration index, then within each variant
// by an obvious natural order. Decimal uses `f64::total_cmp` so NaN sorts
// consistently. `Variant`/`Array`/`List`/`Dict`/`Blob`/`ErrorInfo` are
// never used as primary-key components in BC, so their orderings are
// implementation-defined (length-then-content) — adequate for BTreeMap
// stability without committing to an external contract.
// ---------------------------------------------------------------------------

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
    /// The error message (formatted).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // ── Adversarial tests (adversarial-h) ─────────────────────────────────────

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
}
