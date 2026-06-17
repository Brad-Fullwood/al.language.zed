//! Generated method-id hashing for `SymbolReference.json`.
//!
//! Each method in a `.app`'s `SymbolReference.json` carries an `Id` that is NOT
//! its declared id - it's a 32-bit hash of the method signature (object
//! independent). This reproduces Microsoft's
//! `MethodSymbol.CalculateMethodIdForNewVersions` (runtime >= Spring2021)
//! exactly, verified against alc 17.0.34 output. Reverse-engineered from the
//! decompiled `Microsoft.Dynamics.Nav.CodeAnalysis`:
//!
//! ```text
//! hash = FNV(Name.ToUpperInvariant())
//! hash = Combine(hash, ReturnType.NavTypeKind.GetHashCode())
//! for (i, p) in params:           // p.kind possibly masked for some kinds
//!     hash = Combine(hash, i, p.IsVar.GetHashCode(), p.kind.GetHashCode())
//! return AdjustIdForSystemCodeunits(hash)
//! ```
//!
//! where `FNV` is FNV-1 over the UTF-16LE bytes and `Combine`/the constants are
//! Microsoft's `Hash` helper.

/// FNV offset bias (`0x811C9DC5` as `i32`).
const FNV_OFFSET_BIAS: i32 = -2128831035;
const FNV_PRIME: i32 = 16777619;

/// Public FNV-1 hash (Microsoft `Hash.GetFNVHashCode(string)`) — used by other
/// emit code needing the same hash (e.g. generated object ids).
pub fn fnv1_hash(s: &str) -> i32 {
    fnv1_utf16(s)
}

/// FNV-1 over the UTF-16LE bytes of `s` - matches `Hash.GetFNVHashCode(string)`
/// (which hashes `Encoding.Unicode.GetBytes(s)`).
fn fnv1_utf16(s: &str) -> i32 {
    let mut num = FNV_OFFSET_BIAS;
    for unit in s.encode_utf16() {
        for b in unit.to_le_bytes() {
            num = (num ^ b as i32).wrapping_mul(FNV_PRIME);
        }
    }
    num
}

/// Microsoft `Hash.Combine(h1, h2)`: `((h1<<5) + h1 + (h1>>27)) ^ h2`, with
/// 32-bit wrapping arithmetic and an arithmetic (signed) right shift.
fn combine(h1: i32, h2: i32) -> i32 {
    h1.wrapping_shl(5).wrapping_add(h1).wrapping_add(h1 >> 27) ^ h2
}

/// `bool.GetHashCode()` - `true` maps to 1, `false` maps to 0.
fn bool_hash(b: bool) -> i32 {
    b as i32
}

/// System codeunits (id >= 2_000_000_000) fold the hash into a positive range;
/// user objects keep the raw hash. Mirrors `AdjustIdForSystemCodeunits`.
fn adjust_for_system_codeunits(hash: i32, object_id: i64) -> i32 {
    if object_id >= 2_000_000_000 {
        // Math.Abs(hash) % 1_250_000_000 - unsigned_abs avoids the i32::MIN UB
        // and the result always fits a positive i32.
        (hash.unsigned_abs() % 1_250_000_000) as i32
    } else {
        hash
    }
}

/// One method parameter, as it affects the generated id. `kind` is the
/// parameter type's `NavTypeKind` id (from `language_data::nav_type_kind_id`) —
/// this module stays free of AL-type knowledge, just hashing the values.
#[derive(Debug, Clone, Copy)]
pub struct ParamSig {
    /// The parameter type's `NavTypeKind` id.
    pub kind: i32,
    /// Whether the parameter is `var` (by-reference).
    pub is_var: bool,
    /// `GetSubTypeHashCodeForNewVersions` for this parameter's type — the
    /// referenced object's id for Record/Enum/Codeunit/Page/…, `FNV(name)` for
    /// an interface, `1` for scalars. Only folded in when `disambiguate` is set.
    pub subtype_hash: i32,
}

/// Compute the generated method id alc writes into `SymbolReference.json`
/// (runtime >= Spring2021). `return_kind` and each `ParamSig.kind` are
/// `NavTypeKind` ids. `disambiguate` mirrors alc's
/// `RequiresRuntimeOverloadDisambiguation` — when set, every parameter also
/// folds in its `subtype_hash`. `containing_object_id` only matters for system
/// codeunits (id >= 2_000_000_000).
pub fn method_id(
    name: &str,
    return_kind: i32,
    params: &[ParamSig],
    disambiguate: bool,
    containing_object_id: i64,
) -> i32 {
    // ToUpperInvariant: AL identifiers are effectively ASCII; uppercase is a
    // close match. (Quoted Unicode identifiers are a known edge case.)
    let mut h = fnv1_utf16(&name.to_uppercase());
    h = combine(h, return_kind);
    for (i, p) in params.iter().enumerate() {
        h = combine(combine(combine(h, i as i32), bool_hash(p.is_var)), p.kind);
        if disambiguate {
            h = combine(h, p.subtype_hash);
        }
    }
    adjust_for_system_codeunits(h, containing_object_id)
}

/// `Hash.Combine(h1, h2)`, exposed for other emit code (e.g. object-id hashing).
pub fn combine_hash(h1: i32, h2: i32) -> i32 {
    combine(h1, h2)
}

/// `IdSpace.GetMemberId(s)` — `Math.Abs(FNV(s))`, with the `i32::MIN` edge
/// mapped to `i32::MAX`. Used for page-control / query-column / query-element
/// ids (the caller composes `s`: `Name`, or `objectId + Name` for page controls).
pub fn member_id(s: &str) -> i32 {
    let h = fnv1_utf16(s);
    if h == i32::MIN {
        i32::MAX
    } else {
        h.abs()
    }
}

/// FNV-1 over raw bytes (`Hash.GetFNVHashCode(byte[])`) — e.g. for a GUID's
/// `ToByteArray()` bytes in object-id hashing.
pub fn fnv1_hash_bytes(data: &[u8]) -> i32 {
    let mut num = FNV_OFFSET_BIAS;
    for &b in data {
        num = (num ^ b as i32).wrapping_mul(FNV_PRIME);
    }
    num
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NavTypeKind id by name, from the generated data (no hardcoded values).
    fn k(name: &str) -> i32 {
        crate::syntax::language_data::nav_type_kind_id(name).unwrap()
    }

    fn p(kind: &str, is_var: bool) -> ParamSig {
        ParamSig { kind: k(kind), is_var, subtype_hash: 1 }
    }

    /// Vectors captured from real `alc` 17.0.34 output (a user codeunit, id
    /// 50100). Each is `(name, return_kind, params, expected_id)`. If alc's
    /// algorithm changes in a future toolchain, these break loudly.
    #[test]
    fn reproduces_alc_method_ids() {
        let cases: Vec<(&str, i32, Vec<ParamSig>, i32)> = vec![
            ("M0", k("None"), vec![], -2118248254),
            ("M1", k("Integer"), vec![], 1756487754),
            ("M2", k("Text"), vec![], 1319086004),
            ("M3", k("None"), vec![p("Integer", false)], 409225226),
            (
                "M4",
                k("None"),
                vec![p("Integer", false), p("Text", false)],
                11450251,
            ),
            ("M5", k("None"), vec![p("Integer", true)], 363343141),
            ("M6", k("Boolean"), vec![p("Boolean", false)], 56457289),
            ("M7", k("None"), vec![p("Code", false)], 296523416),
        ];
        for (name, ret, params, expected) in cases {
            let got = method_id(name, ret, &params, false, 50100);
            assert_eq!(
                got, expected,
                "method {name}: got {got}, expected {expected}"
            );
        }
    }

    /// Independent vectors from a separate codeunit confirm the id is a pure
    /// function of the signature (object-independent) - `Greet()`/void was
    /// `1462331901` in two different codeunits.
    #[test]
    fn id_is_object_independent_void_methods() {
        let none = k("None");
        for (name, expected) in [
            ("Greet", 1462331901),
            ("A", -981080143),
            ("AB", 1554881421),
            ("Hello", 267291819),
        ] {
            assert_eq!(method_id(name, none, &[], false, 50100), expected, "{name}");
            // Same signature in a different object id gives the same id.
            assert_eq!(
                method_id(name, none, &[], false, 50101),
                expected,
                "{name} obj 50101"
            );
        }
    }

    #[test]
    fn name_is_uppercased_before_hashing() {
        let none = k("None");
        assert_eq!(
            method_id("greet", none, &[], false, 50100),
            method_id("GREET", none, &[], false, 50100)
        );
    }
}
