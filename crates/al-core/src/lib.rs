//! al-core: Central engine for AL language analysis.
//!
//! All state, queries, and orchestration live here. Only `al-lsp` imports this crate.
//! Analysis libraries (al-syntax, al-symbols, al-semantic) are standalone dependencies.

pub mod documents;
pub mod errors;
pub mod jsonrpc;
pub mod launch;
pub mod parsing;
pub mod project;
pub mod queries;
pub(crate) mod resolution;
pub mod toolchain;
pub mod workspace;
