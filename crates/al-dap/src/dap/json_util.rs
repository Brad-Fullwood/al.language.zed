//! JSONC pre-processing utilities.
//!
//! The canonical implementations now live in the tier-0 `al-types` crate;
//! this module re-exports them so existing `crate::dap::json_util::…` paths
//! keep working.

pub use al_types::jsonc::{strip_json_comments, strip_trailing_commas};
