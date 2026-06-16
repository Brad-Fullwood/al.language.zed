//! `NavTypeKind` - Microsoft's AL type-kind enum, as it feeds the generated
//! method-id hash.
//!
//! A .NET enum's `GetHashCode()` is its underlying `int` value, so
//! [`NavTypeKind::hash_code`] just returns the discriminant. Values were read
//! directly from `Microsoft.Dynamics.Nav.CodeAnalysis` (alc 17.0.34). This is a
//! curated subset - the scalar/common kinds needed so far; extend from the
//! decompiled enum as more object types are emitted.

/// AL type kind. Discriminants match Microsoft's `NavTypeKind` enum exactly so
/// `hash_code()` reproduces `navTypeKind.GetHashCode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum NavTypeKind {
    /// No type - e.g. a `void` (no return value) method.
    None = 0,
    Boolean = 12451842,
    Byte = 3014659,
    Char = 3014660,
    Integer = 12451845,
    BigInteger = 12451846,
    Decimal = 12451847,
    Option = 12451848,
    Text = 16646153,
    Code = 16646154,
    DateTime = 12451853,
    Time = 12451854,
    Date = 12451855,
    DateFormula = 12451856,
    Duration = 12451857,
    Guid = 12451858,
    Enum = 12451896,
    BigText = 3014703,
    Blob = 3145776,
    RecordId = 12451880,
    RecordRef = 917545,
    FieldRef = 917546,
    Variant = 917554,
}

impl NavTypeKind {
    /// The value `navTypeKind.GetHashCode()` returns - the enum discriminant.
    #[inline]
    pub fn hash_code(self) -> i32 {
        self as i32
    }
}
