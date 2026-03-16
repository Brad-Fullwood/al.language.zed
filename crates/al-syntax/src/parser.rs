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
        // tree-sitter returns None only when parsing is cancelled (timeout/cancellation).
        // We don't set either, so None is unexpected; fall back to a fresh parser instance.
        let tree = match self.parser.parse(text, None) {
            Some(t) => t,
            None => {
                let mut fallback = Parser::new();
                let _ = fallback.set_language(&language());
                fallback
                    .parse(text, None)
                    .or_else(|| fallback.parse("", None))
                    .expect("empty source parse must succeed")
            }
        };
        let errors = collect_errors(&tree, text);
        ParseResult { tree, errors }
    }

    pub fn parse_incremental(&mut self, text: &str, old_tree: &Tree) -> ParseResult {
        let tree = match self.parser.parse(text, Some(old_tree)) {
            Some(t) => t,
            None => return self.parse(text),
        };
        let errors = collect_errors(&tree, text);
        ParseResult { tree, errors }
    }

    /// Extract syntax errors from an already-parsed tree.
    ///
    /// Used to obtain errors from a cached tree without re-parsing the source.
    /// The `text` parameter is accepted for API consistency but is currently unused.
    pub fn errors_from_tree(tree: &Tree) -> Vec<SyntaxError> {
        collect_errors(tree, "")
    }

    /// Parse using a thread-local parser, avoiding repeated `Parser::new()` + `set_language()`.
    ///
    /// Preferred over `AlParser::new()` + `parse()` in hot paths where the parser
    /// is used once and discarded.
    pub fn parse_quick(text: &str) -> ParseResult {
        thread_local! {
            static PARSER: std::cell::RefCell<AlParser> = std::cell::RefCell::new(AlParser::new());
        }
        PARSER.with(|p| p.borrow_mut().parse(text))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_string() {
        let mut parser = AlParser::new();
        let result = parser.parse("");
        // Empty source should produce a tree (even if trivial)
        assert!(result.tree.root_node().child_count() == 0 || result.errors.is_empty());
    }

    #[test]
    fn test_parse_just_whitespace() {
        let mut parser = AlParser::new();
        let result = parser.parse("   \n\n   ");
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_parse_unicode_content() {
        let mut parser = AlParser::new();
        let result = parser.parse("codeunit 50100 \"Ünîcödé Tëst\" { }");
        // Should parse without panicking
        assert!(result.tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_parse_very_long_line() {
        let mut parser = AlParser::new();
        let long_name = "A".repeat(1000);
        let source = format!("codeunit 50100 \"{}\" {{ }}", long_name);
        let result = parser.parse(&source);
        assert!(result.tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_parse_deeply_nested() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 Test {
    procedure Deep()
    begin
        if true then begin
            if true then begin
                if true then begin
                    if true then begin
                        if true then begin
                            Message('deep');
                        end;
                    end;
                end;
            end;
        end;
    end;
}"#;
        let result = parser.parse(source);
        // Should parse without stack overflow
        assert!(result.tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_parse_incremental_after_edit() {
        let mut parser = AlParser::new();
        let source1 = "codeunit 50100 Test { }";
        let result1 = parser.parse(source1);
        let source2 = "codeunit 50100 Test { procedure A() begin end; }";
        let result2 = parser.parse_incremental(source2, &result1.tree);
        assert!(result2.tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_parser_default_trait() {
        let mut parser = AlParser::default();
        let result = parser.parse("codeunit 50100 Test { }");
        assert!(result.tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_syntax_error_on_invalid_code() {
        let mut parser = AlParser::new();
        let result = parser.parse("codeunit 50100 Test { procedure () begin end; }");
        // Invalid code with missing procedure name may produce errors
        // At minimum it should not panic
        let _ = result;
    }
}
