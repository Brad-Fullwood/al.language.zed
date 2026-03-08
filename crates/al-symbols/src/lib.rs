//! AL symbol index crate.
//!
//! Parses `.app` packages (NAVX header + ZIP containing SymbolReference.json),
//! builds a concurrent symbol index, and provides NuGet symbol download.
//!
//! # Modules
//!
//! - [`model`] — Core types: `SymbolEntry`, `ObjectKind`, `SymbolPackage`, etc.
//! - [`app_reader`] — Parse `.app` files (NAVX header + ZIP)
//! - [`manifest`] — Parse `NavxManifest.xml` for package metadata
//! - [`index`] — DashMap-backed concurrent symbol index
//! - [`composition`] — Merge base objects with extensions
//! - [`events`] — Discover event publishers and subscribers
//! - [`nuget`] — NuGet v3 client for downloading symbol packages

pub mod app_reader;
pub mod composition;
pub mod events;
pub mod index;
pub mod manifest;
pub mod model;

#[cfg(feature = "nuget")]
pub mod nuget;

// Re-export primary types for convenience.
pub use index::SymbolIndex;
pub use model::{
    ComposedObject, ObjectKind, SymbolEntry, SymbolPackage,
};
