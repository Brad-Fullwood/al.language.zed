//! This crate provides AL (Microsoft Dynamics 365 Business Central) language
//! support for the [tree-sitter] parsing library.
//!
//! Typical usage builds a [`tree_sitter::Parser`] with the [`LANGUAGE`] constant:
//!
//! ```
//! let mut parser = tree_sitter::Parser::new();
//! parser
//!     .set_language(&tree_sitter_al::LANGUAGE.into())
//!     .expect("Error loading AL parser");
//! ```
//!
//! [tree-sitter]: https://tree-sitter.github.io/

use tree_sitter_language::LanguageFn;

extern "C" {
    fn tree_sitter_al() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for this grammar.
///
/// Convert to a [`tree_sitter::Language`] with `.into()`.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_al) };

/// The content of this grammar's `node-types.json` file.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");

/// The syntax-highlighting query for this grammar.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// Generated AL language-data tables embedded for downstream consumers.
pub mod data {
    pub const KEYWORDS: &str = include_str!("../../data/keywords.json");
    pub const BUILTIN_FUNCTIONS: &str = include_str!("../../data/builtin_functions.json");
    pub const OBJECT_TYPES: &str = include_str!("../../data/object_types.json");
    pub const IMPLICIT_VARIABLES: &str = include_str!("../../data/implicit_variables.json");
    pub const PAGE_CONTROLS: &str = include_str!("../../data/page_controls.json");
    pub const SINGLE_STMT_OPENERS: &str = include_str!("../../data/single_stmt_openers.json");
    pub const RUNTIME_ENUMS: &str = include_str!("../../data/runtime_enums.json");
    pub const NAV_TYPE_KINDS: &str = include_str!("../../data/nav_type_kinds.json");
    pub const SYSTEM_OBJECTS: &str = include_str!("../../data/system_objects.json");
    pub const TOKEN_CLASSIFICATION: &str = include_str!("../../data/token_classification.json");
}

#[cfg(test)]
mod tests {
    #[test]
    fn can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("Error loading AL parser");
    }
}
