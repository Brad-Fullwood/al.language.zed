//! AL symbol index crate.
//!
//! Parses `.app` packages (NAVX header + ZIP containing SymbolReference.json),
//! builds a concurrent symbol index, and provides NuGet symbol download.

pub mod app_inspect;
pub mod app_reader;
pub mod cache;
pub mod composition;
pub mod events;
pub mod index;
pub mod language_data;
pub mod manifest;
pub mod model;
pub mod source_availability;
pub mod source_index;
pub mod virtual_file;

#[cfg(feature = "nuget")]
pub mod bc_server;
#[cfg(feature = "nuget")]
pub mod nuget;
#[cfg(feature = "nuget")]
pub mod oauth;

pub use index::{PackageLoadError, PackageLoadFailure, SymbolIndex, SymbolIndexMemoryStats};
pub use model::{
    AttributeSymbol, ComposedObject, ControlSymbol, DeclarationIdError, EnumValueSymbol,
    FieldSymbol, KeySymbol, MethodSymbol, ObjectKind, ParameterSymbol, PermissionSymbol,
    PropertyValue, SymbolEntry, SymbolPackage, VariableSymbol,
};
pub use source_availability::{SourceAvailability, SourceAvailabilitySummary};

pub use app_reader::{read_app_bytes, read_app_file, read_symbol_reference_bytes};
pub use composition::get_composed;
pub use events::{get_events, EventPublisher, EventResults, EventSubscriber, EventType};
pub use manifest::{parse_manifest, ManifestDependency, NavxManifest};

#[cfg(feature = "nuget")]
pub use nuget::{AppDependency, NuGetClient, NuGetError, NuGetFeed, PackageRef};
