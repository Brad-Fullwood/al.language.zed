//! AL workspace source ingestion.
//!
//! Owns the three building blocks that turn raw `.al` text into cached,
//! query-ready state:
//!
//! - [`documents`]: rope-based open-document store with bounded parse-tree
//!   caching (transport-agnostic `TextChange`/`TextRange`).
//! - [`file_index`]: on-disk `.al` file index (object-name / object-id /
//!   procedure reverse maps, cached trees + symbols).
//! - [`parsing`]: version-validated parse-tree cache (`get_or_parse`).

pub mod documents;
pub mod file_index;
pub mod parsing;
