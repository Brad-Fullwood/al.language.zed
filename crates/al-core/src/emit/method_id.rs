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

use super::nav_type_kind::NavTypeKind;

/// FNV offset bias (`0x811C9DC5` as `i32`).
const FNV_OFFSET_BIAS: i32 = -2128831035;
/// FNV prime.
const FNV_PRIME: i32 = 16777619;

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

/// One method parameter, as it affects the generated id.
#[derive(Debug, Clone, Copy)]
pub struct ParamSig {
    /// The parameter's AL type kind.
    pub kind: NavTypeKind,
    /// Whether the parameter is `var` (by-reference).
    pub is_var: bool,
}

/// Compute the generated method id alc writes into `SymbolReference.json`
/// (runtime >= Spring2021). `containing_object_id` is the owning object's id
/// (only matters for system codeunits, id >= 2_000_000_000).
///
/// Exact for scalar signatures. Parameters whose type carries a *subtype*
/// (Record / Codeunit / Enum / ...) trigger an extra disambiguation hash in
/// alc (`RequiresRuntimeOverloadDisambiguation`) that is not yet modelled here;
/// such methods are tracked as follow-up. Triggers (vs procedures) also fold in
/// the trigger name - likewise not yet modelled.
pub fn method_id(
    name: &str,
    return_kind: NavTypeKind,
    params: &[ParamSig],
    containing_object_id: i64,
) -> i32 {
    // ToUpperInvariant: AL identifiers are effectively ASCII; uppercase is a
    // close match. (Quoted Unicode identifiers are a known edge case.)
    let mut h = fnv1_utf16(&name.to_uppercase());
    h = combine(h, return_kind.hash_code());
    for (i, p) in params.iter().enumerate() {
        h = combine(
            combine(combine(h, i as i32), bool_hash(p.is_var)),
            p.kind.hash_code(),
        );
    }
    adjust_for_system_codeunits(h, containing_object_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use NavTypeKind as K;

    fn p(kind: NavTypeKind, is_var: bool) -> ParamSig {
        ParamSig { kind, is_var }
    }

    /// Vectors captured from real `alc` 17.0.34 output (a user codeunit, id
    /// 50100). Each is `(name, return_kind, params, expected_id)`. If alc's
    /// algorithm changes in a future toolchain, these break loudly.
    #[test]
    fn reproduces_alc_method_ids() {
        let cases: &[(&str, NavTypeKind, Vec<ParamSig>, i32)] = &[
            ("M0", K::None, vec![], -2118248254),
            ("M1", K::Integer, vec![], 1756487754),
            ("M2", K::Text, vec![], 1319086004),
            ("M3", K::None, vec![p(K::Integer, false)], 409225226),
            (
                "M4",
                K::None,
                vec![p(K::Integer, false), p(K::Text, false)],
                11450251,
            ),
            ("M5", K::None, vec![p(K::Integer, true)], 363343141),
            ("M6", K::Boolean, vec![p(K::Boolean, false)], 56457289),
            ("M7", K::None, vec![p(K::Code, false)], 296523416),
        ];
        for (name, ret, params, expected) in cases {
            let got = method_id(name, *ret, params, 50100);
            assert_eq!(
                got, *expected,
                "method {name}: got {got}, expected {expected}"
            );
        }
    }

    /// Independent vectors from a separate codeunit confirm the id is a pure
    /// function of the signature (object-independent) - `Greet()`/void was
    /// `1462331901` in two different codeunits.
    #[test]
    fn id_is_object_independent_void_methods() {
        for (name, expected) in [
            ("Greet", 1462331901),
            ("A", -981080143),
            ("AB", 1554881421),
            ("Hello", 267291819),
        ] {
            assert_eq!(method_id(name, K::None, &[], 50100), expected, "{name}");
            // Same signature in a different object id gives the same id.
            assert_eq!(
                method_id(name, K::None, &[], 50101),
                expected,
                "{name} obj 50101"
            );
        }
    }

    #[test]
    fn name_is_uppercased_before_hashing() {
        // ToUpperInvariant: casing must not change the id.
        assert_eq!(
            method_id("greet", K::None, &[], 50100),
            method_id("GREET", K::None, &[], 50100)
        );
    }
}
