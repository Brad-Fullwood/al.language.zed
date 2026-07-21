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
//! Reverse-engineered against alc 17.0.34.

pub mod assemble;
pub mod manifest;
pub mod method_id;
pub mod package;
pub mod project;
pub mod symbol_extract;
pub mod symbol_reference;
#[cfg(test)]
mod symbol_reference_test;

pub use assemble::{assemble_app, SourceFile};
pub use manifest::{AppManifest, Dependency, ResourceExposurePolicy};
pub use method_id::{method_id, ParamSig};
pub use package::{random_package_guid, write_app_package, EmitError};
pub use project::{build_app_from_project, load_external_symbols, now_timestamp, BuiltApp};
pub use symbol_extract::{extract_objects, EmitObject};
pub use symbol_reference::{
    build_profile_symbol_references, build_symbol_reference, ExternalSymbols, ObjectRef, Resolver,
    SymbolRefMeta,
};
