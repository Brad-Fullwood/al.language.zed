//! Transport-agnostic syntax types for crate::syntax.
//!
//! These types are the native vocabulary of the crate::syntax crate. They mirror
//! the LSP types (Position, Range, DocumentSymbol, SymbolKind, FoldingRange,
//! FoldingRangeKind) but carry no dependency on tower-lsp or any transport layer.

/// A position in a document (0-indexed line and character/UTF-16 column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyntaxPosition {
    pub line: u32,
    pub character: u32,
}

/// A range in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyntaxRange {
    pub start: SyntaxPosition,
    pub end: SyntaxPosition,
}

/// Symbol kind — mirrors LSP SymbolKind without the transport dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxSymbolKind {
    File,
    Module,
    Namespace,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    EnumMember,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Struct,
    Event,
    Operator,
    TypeParameter,
    /// Table index key.
    Key,
}

/// A document symbol (for outline/symbol views).
#[derive(Debug, Clone)]
pub struct SyntaxDocumentSymbol {
    pub name: std::string::String,
    pub detail: Option<std::string::String>,
    pub kind: SyntaxSymbolKind,
    pub range: SyntaxRange,
    pub selection_range: SyntaxRange,
    pub children: Option<Vec<SyntaxDocumentSymbol>>,
}

/// Folding range kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxFoldingRangeKind {
    Comment,
    Imports,
    Region,
}

/// A folding range in a document.
#[derive(Debug, Clone)]
pub struct SyntaxFoldingRange {
    pub start_line: u32,
    pub start_character: Option<u32>,
    pub end_line: u32,
    pub end_character: Option<u32>,
    pub kind: Option<SyntaxFoldingRangeKind>,
}
