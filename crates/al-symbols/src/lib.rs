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
pub mod virtual_file;

#[cfg(feature = "nuget")]
pub mod bc_server;
#[cfg(feature = "nuget")]
pub mod nuget;

// Re-export primary types for convenience.
pub use index::SymbolIndex;
pub use model::{
    AttributeSymbol, ComposedObject, ControlSymbol, EnumValueSymbol, FieldSymbol, MethodSymbol,
    ObjectKind, ParameterSymbol, SymbolEntry, SymbolPackage,
};

// Re-export key functions and types from submodules.
pub use app_reader::{read_app_bytes, read_app_file};
pub use composition::get_composed;
pub use events::{get_events, EventPublisher, EventResults, EventSubscriber, EventType};
pub use manifest::{parse_manifest, NavxManifest};

#[cfg(feature = "nuget")]
pub use nuget::{AppDependency, NuGetClient, NuGetError, NuGetFeed, PackageRef};
