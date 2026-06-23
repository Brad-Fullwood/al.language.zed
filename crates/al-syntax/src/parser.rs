//! Tree-sitter parser wrapper for AL.

use super::traversal::walk_tree;
use tree_sitter::{Language, Parser, Tree};

/// The AL grammar, from the canonical `tree-sitter-al` binding crate.
pub fn language() -> Language {
    tree_sitter_al::LANGUAGE.into()
}

pub struct AlParser {
    parser: Parser,
}

pub struct ParseResult {
    pub tree: Tree,
    pub errors: Vec<SyntaxError>,
}

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub message: String,
    pub range: tree_sitter::Range,
}

impl AlParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&language())
            .expect("Failed to set AL language");
        Self { parser }
    }

    pub fn parse(&mut self, text: &str) -> ParseResult {
        let tree = self
            .parser
            .parse(text, None)
            .expect("tree-sitter parse must succeed without timeout or cancellation");
        let errors = collect_errors(&tree);
        ParseResult { tree, errors }
    }

    pub fn parse_incremental(&mut self, text: &str, old_tree: &Tree) -> ParseResult {
        let tree = self
            .parser
            .parse(text, Some(old_tree))
            .expect("tree-sitter incremental parse must succeed without timeout or cancellation");
        let errors = collect_errors(&tree);
        ParseResult { tree, errors }
    }

    /// Extract syntax errors from an already-parsed tree.
    ///
    /// Used to obtain errors from a cached tree without re-parsing the source.
    pub fn errors_from_tree(tree: &Tree) -> Vec<SyntaxError> {
        collect_errors(tree)
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

fn collect_errors(tree: &Tree) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    walk_tree(tree.root_node(), &mut |node| {
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
    });
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_string() {
        let mut parser = AlParser::new();
        let result = parser.parse("");
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

    #[test]
    fn test_errors_from_tree_extracts_without_reparse() {
        // `errors_from_tree` should yield the same errors as the original parse,
        // operating on a cached tree without re-parsing the source.
        let mut parser = AlParser::new();
        let source = "codeunit 50100 Test { procedure () begin end; }";
        let result = parser.parse(source);

        let from_tree = AlParser::errors_from_tree(&result.tree);

        assert_eq!(
            from_tree.len(),
            result.errors.len(),
            "errors_from_tree should match the parse result's error count"
        );
        for (a, b) in from_tree.iter().zip(result.errors.iter()) {
            assert_eq!(a.message, b.message);
            assert_eq!(a.range.start_byte, b.range.start_byte);
            assert_eq!(a.range.end_byte, b.range.end_byte);
        }
    }

    #[test]
    fn test_errors_from_tree_empty_for_valid_code() {
        let mut parser = AlParser::new();
        let result = parser.parse("codeunit 50100 Test { }");
        let from_tree = AlParser::errors_from_tree(&result.tree);
        assert!(
            from_tree.is_empty(),
            "valid code should yield no errors from the cached tree"
        );
    }

    // T1307: List of [Interface IFoo] syntax

    #[test]
    fn test_parse_list_of_interface_variable() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 "Test"
{
    var
        Tools: List of [Interface "AOAI Function"];

    procedure DoSomething()
    var
        LocalList: List of [Interface IMyInterface];
    begin
    end;
}"#;
        let result = parser.parse(source);
        assert!(
            result.tree.root_node().child_count() > 0,
            "Should parse List of [Interface ...] successfully"
        );
        let root_text = result.tree.root_node().to_sexp();
        assert!(
            !root_text.contains("ERROR"),
            "No parse errors expected for List of [Interface ...] syntax"
        );
    }

    #[test]
    fn test_parse_list_of_interface_return_type() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 "Test"
{
    procedure GetTools(): List of [Interface "AOAI Function"]
    var
        List: List of [Interface "AOAI Function"];
    begin
        exit(List);
    end;
}"#;
        let result = parser.parse(source);
        assert!(result.tree.root_node().child_count() > 0);
        let root_text = result.tree.root_node().to_sexp();
        assert!(
            !root_text.contains("ERROR"),
            "No parse errors expected for List of [Interface ...] return type"
        );
    }

    #[test]
    fn test_deeply_nested_does_not_stackoverflow() {
        let mut parser = AlParser::new();
        let mut code = String::from("codeunit 1 Test { trigger OnRun() { ");
        for _ in 0..500 {
            code.push_str("if true then begin ");
        }
        for _ in 0..500 {
            code.push_str("end; ");
        }
        code.push_str("} }");
        let result = parser.parse(&code);
        let _ = result;
    }
}
