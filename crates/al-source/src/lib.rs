//! al-source: AL workspace source ingestion (tier 1).
//!
//! Owns the three building blocks that turn raw `.al` text into cached,
//! query-ready state without any dependency on the tier-4 `Workspace` hub:
//!
//! - [`documents`]: rope-based open-document store with bounded parse-tree
//!   caching (transport-agnostic `TextChange`/`TextRange`).
//! - [`file_index`]: on-disk `.al` file index (object-name / object-id /
//!   procedure reverse maps, cached trees + symbols).
//! - [`parsing`]: version-validated parse-tree cache (`get_or_parse`).
//!
//! Depends only on `al-syntax` (tree-sitter parsing + native syntax types) and
//! `al-types`. Document symbols are cached as
//! `al_syntax::types::SyntaxDocumentSymbol` so this crate never reaches up into
//! the analysis-layer query DTOs (`crate::queries::AlDocumentSymbol`).

pub mod documents;
pub mod file_index;
pub mod parsing;
