//! AL symbol index — .app package parsing, symbol search, NuGet download.

pub mod app_reader;
pub mod model;
pub mod index;
pub mod composition;
pub mod events;
pub mod manifest;
pub mod nuget;

pub use index::SymbolIndex;
pub use model::{ObjectKind, SymbolEntry, SymbolPackage};
