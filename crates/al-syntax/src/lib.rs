//! AL syntax layer — tree-sitter parsing, AST navigation, formatting, lint.

pub mod parser;
pub mod navigation;
pub mod formatting;
pub mod lint;
pub mod symbols;
pub mod tokens;
pub mod folding;

pub use parser::{AlParser, ParseResult};
