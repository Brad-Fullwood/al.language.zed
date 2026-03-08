//! AL Language Server — LSP implementation.

pub mod server;
pub mod handlers;
pub mod hover;
pub mod definition;
pub mod completions;
pub mod diagnostics;
pub mod formatting;
pub mod document;
pub mod workspace;
pub mod type_resolver;
mod parsing;
