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
#[derive(Debug, Clone, PartialEq)]
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
            "text" | "code" => Some(Value::Text(String::new())),
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
}
