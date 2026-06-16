//! Native AL `.app` emission - reverse-engineered from Microsoft's `alc`, no
//! Microsoft runtime dependency.
//!
//! A `.app` is a `NAVX` header + an OPC/ZIP of *source + symbol table +
//! metadata XML* - there is no IL (the BC server compiles to runtime artifacts
//! at publish). So a `.app` can be assembled natively; the one non-trivial file
//! is `SymbolReference.json`, whose method `Id`s are a generated hash of the
//! method signature. This module reproduces that hash exactly (see
//! [`method_id`]); the package writer and full symbol emitter build on it.
//!
//! Reverse-engineered against alc 17.0.34. See
//! `docs/spikes/2026-06-15-al-compiler-emit-feasibility.md`.

pub mod method_id;
pub mod nav_type_kind;

pub use method_id::{method_id, ParamSig};
pub use nav_type_kind::NavTypeKind;
