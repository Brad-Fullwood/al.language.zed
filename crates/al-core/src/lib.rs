//! al-core: Central engine for AL language analysis.
//!
//! All state, queries, and orchestration live here. Only `al-lsp` imports this crate.
//! Analysis libraries (al-syntax, al-symbols, al-semantic) are standalone dependencies.

pub mod errors;
pub mod workspace;
