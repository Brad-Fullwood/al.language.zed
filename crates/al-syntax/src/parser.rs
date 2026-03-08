//! Tree-sitter parser wrapper for AL.

use tree_sitter::{Language, Parser, Tree};

extern "C" {
    fn tree_sitter_al() -> Language;
}

/// Get the tree-sitter AL language.
pub fn language() -> Language {
    unsafe { tree_sitter_al() }
}

/// AL parser with tree-sitter.
pub struct AlParser {
    parser: Parser,
}

/// Result of parsing AL source code.
pub struct ParseResult {
    pub tree: Tree,
    pub errors: Vec<SyntaxError>,
}

/// A syntax error found during parsing.
#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub message: String,
    pub range: tree_sitter::Range,
}

impl AlParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser.set_language(&language()).expect("Failed to set AL language");
        Self { parser }
    }

    pub fn parse(&mut self, text: &str) -> ParseResult {
        let tree = self.parser.parse(text, None).expect("Parse failed");
        let errors = collect_errors(&tree, text);
        ParseResult { tree, errors }
    }

    pub fn parse_incremental(&mut self, text: &str, old_tree: &Tree) -> ParseResult {
        let tree = self.parser.parse(text, Some(old_tree)).expect("Parse failed");
        let errors = collect_errors(&tree, text);
        ParseResult { tree, errors }
    }
}

impl Default for AlParser {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_errors(tree: &Tree, _text: &str) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    let mut cursor = tree.walk();
    collect_errors_recursive(&mut cursor, &mut errors);
    errors
}

fn collect_errors_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    errors: &mut Vec<SyntaxError>,
) {
    let node = cursor.node();
    if node.is_error() || node.is_missing() {
        errors.push(SyntaxError {
            message: if node.is_missing() {
                format!("Missing {}", node.kind())
            } else {
                "Syntax error".to_string()
            },
            range: node.range(),
        });
    }
    if cursor.goto_first_child() {
        loop {
            collect_errors_recursive(cursor, errors);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
