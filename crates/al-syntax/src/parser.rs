//! Tree-sitter parser wrapper for AL.

use tree_sitter::{Language, Parser, Tree};

/// The AL grammar, from the canonical `tree-sitter-al` binding crate.
pub fn language() -> Language {
    tree_sitter_al::LANGUAGE.into()
}

/// A fingerprint of the grammar this build parses with: FNV-1a over the
/// grammar's node types and the sizes of its parse table.
///
/// Data derived from parse trees and written to disk records it, so a build
/// with a different grammar can tell the data was not made by its parser.
pub fn grammar_fingerprint() -> u64 {
    static FINGERPRINT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *FINGERPRINT.get_or_init(|| {
        let language = language();
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut feed = |bytes: &[u8]| {
            for &byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x00000100000001b3);
            }
        };
        feed(tree_sitter_al::NODE_TYPES.as_bytes());
        for count in [
            language.abi_version(),
            language.node_kind_count(),
            language.parse_state_count(),
            language.field_count(),
        ] {
            feed(&(count as u64).to_le_bytes());
        }
        hash
    })
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
    /// The alc (Microsoft AL compiler) code when the condition matches one
    /// alc reports for the same source, see [`ALC_MISSING_SEMICOLON_CODE`].
    /// `None` means no alc equivalent is mapped, callers use their own code.
    pub code: Option<&'static str>,
}

/// alc code and message for a statement that is not terminated by `;`.
/// Both strings and the range rule below come from alc 18.1.43 errorlogs of
/// real mutated projects (journey-15), not from the documentation.
pub const ALC_MISSING_SEMICOLON_CODE: &str = "AL0111";
pub const ALC_MISSING_SEMICOLON_MESSAGE: &str =
    "Semicolon expected. Add a semicolon (;) to terminate the statement.";

/// alc code for an expected token that is absent (AL0104, "Syntax error, 'x' expected").
/// Code, message form and ranges come from alc 18.1.43 errorlogs (journey-15).
pub const ALC_EXPECTED_TOKEN_CODE: &str = "AL0104";

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
        let errors = collect_errors(&tree, text.as_bytes());
        ParseResult { tree, errors }
    }

    pub fn parse_incremental(&mut self, text: &str, old_tree: &Tree) -> ParseResult {
        let tree = self
            .parser
            .parse(text, Some(old_tree))
            .expect("tree-sitter incremental parse must succeed without timeout or cancellation");
        let errors = collect_errors(&tree, text.as_bytes());
        ParseResult { tree, errors }
    }

    /// Parse with `symbols` defined, such as the `preprocessorSymbols` of the
    /// project's `app.json`, so `#if` blocks on them are active.
    pub fn parse_with_symbols<S: AsRef<str>>(&mut self, text: &str, symbols: &[S]) -> ParseResult {
        tree_sitter_al::with_preprocessor_symbols(symbols, || self.parse(text))
    }

    /// Incremental form of [`Self::parse_with_symbols`]. `old_tree` must have
    /// been parsed with the same symbols, because the scanner state stored in
    /// the tree does not record them.
    pub fn parse_incremental_with_symbols<S: AsRef<str>>(
        &mut self,
        text: &str,
        old_tree: &Tree,
        symbols: &[S],
    ) -> ParseResult {
        tree_sitter_al::with_preprocessor_symbols(symbols, || {
            self.parse_incremental(text, old_tree)
        })
    }

    /// Extract syntax errors from an already-parsed tree.
    ///
    /// Used to obtain errors from a cached tree without re-parsing the source.
    /// `source` is the text the tree was parsed from, the alc code mapping reads
    /// keyword text from it.
    pub fn errors_from_tree(tree: &Tree, source: &[u8]) -> Vec<SyntaxError> {
        collect_errors(tree, source)
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

    /// [`Self::parse_quick`] with `symbols` defined, see
    /// [`Self::parse_with_symbols`].
    pub fn parse_quick_with_symbols<S: AsRef<str>>(text: &str, symbols: &[S]) -> ParseResult {
        tree_sitter_al::with_preprocessor_symbols(symbols, || Self::parse_quick(text))
    }
}

impl Default for AlParser {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_errors(tree: &Tree, source: &[u8]) -> Vec<SyntaxError> {
    let mut errors = Vec::new();
    let tokens = Tokens::new(tree.root_node());
    // A cursor walk in pre-order. `walk_tree` hides the tree lifetime from its
    // visitor, which the shared `tokens` needs.
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            if let Some(mapped) = alc_errors(node, source, &tokens) {
                errors.extend(mapped);
            } else {
                errors.push(SyntaxError {
                    message: if node.is_missing() {
                        format!("Missing {}", node.kind())
                    } else {
                        "Syntax error".to_string()
                    },
                    range: node.range(),
                    code: None,
                });
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    errors
}

fn alc_error(code: &'static str, message: String, range: tree_sitter::Range) -> SyntaxError {
    SyntaxError {
        message,
        range,
        code: Some(code),
    }
}

/// The alc errors for one tree-sitter error node, when its shape is one alc
/// was recorded on (tests/alc_syntax_codes.rs). `None` keeps the generic error.
fn alc_errors<'tree>(
    node: tree_sitter::Node<'tree>,
    source: &[u8],
    tokens: &Tokens<'tree>,
) -> Option<Vec<SyntaxError>> {
    if let Some((_, range)) = missing_semicolon_shape(node, tokens) {
        return Some(vec![alc_error(
            ALC_MISSING_SEMICOLON_CODE,
            ALC_MISSING_SEMICOLON_MESSAGE.to_string(),
            range,
        )]);
    }
    // Missing `)`: tree-sitter inserts a zero width MISSING node, alc reports
    // the token that stands where `)` belongs.
    if node.is_missing() && node.kind() == ")" {
        let next = tokens.first_from(node.start_byte())?;
        return Some(vec![alc_error(
            ALC_EXPECTED_TOKEN_CODE,
            "Syntax error, ')' expected".to_string(),
            next.range(),
        )]);
    }
    // Missing `end`: a `procedure` keyword read as an identifier where a
    // statement starts, after a `;`. alc reports 'end' and ';' expected, both on
    // that keyword. The errors tree-sitter raises further on while recovering
    // stay generic.
    if lone_token_error_after(node, &["semicolon"], tokens).is_some()
        && node.child(0)?.kind() == "identifier"
        && node
            .child(0)?
            .utf8_text(source)
            .ok()?
            .eq_ignore_ascii_case("procedure")
    {
        let range = node.range();
        return Some(vec![
            alc_error(
                ALC_EXPECTED_TOKEN_CODE,
                "Syntax error, 'end' expected".to_string(),
                range,
            ),
            alc_error(
                ALC_EXPECTED_TOKEN_CODE,
                "Syntax error, ';' expected".to_string(),
                range,
            ),
        ]);
    }
    None
}

fn root_of(node: tree_sitter::Node) -> tree_sitter::Node {
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    root
}

/// The non-empty leaf tokens of a tree outside comments, in source order.
/// Built on the first lookup, so error collection walks the tree once however
/// many errors ask for a neighbouring token.
struct Tokens<'tree> {
    root: tree_sitter::Node<'tree>,
    leaves: std::cell::OnceCell<Vec<tree_sitter::Node<'tree>>>,
}

impl<'tree> Tokens<'tree> {
    fn new(root: tree_sitter::Node<'tree>) -> Self {
        Self {
            root,
            leaves: std::cell::OnceCell::new(),
        }
    }

    fn leaves(&self) -> &[tree_sitter::Node<'tree>] {
        self.leaves.get_or_init(|| {
            let mut leaves = Vec::new();
            let mut cursor = self.root.walk();
            'walk: loop {
                let node = cursor.node();
                let is_comment = node.kind().contains("comment");
                if !is_comment && cursor.goto_first_child() {
                    continue;
                }
                if !is_comment && node.child_count() == 0 && node.start_byte() < node.end_byte() {
                    leaves.push(node);
                }
                while !cursor.goto_next_sibling() {
                    if !cursor.goto_parent() {
                        break 'walk;
                    }
                }
            }
            leaves
        })
    }

    /// The last token that ends at or before `byte`.
    fn last_before(&self, byte: usize) -> Option<tree_sitter::Node<'tree>> {
        let leaves = self.leaves();
        let count = leaves.partition_point(|leaf| leaf.end_byte() <= byte);
        count.checked_sub(1).map(|idx| leaves[idx])
    }

    /// The first token that starts at or after `byte`.
    fn first_from(&self, byte: usize) -> Option<tree_sitter::Node<'tree>> {
        let leaves = self.leaves();
        let idx = leaves.partition_point(|leaf| leaf.start_byte() < byte);
        leaves.get(idx).copied()
    }
}

/// An ERROR node holding exactly one leaf token, inside a `statement_list`
/// and not inside another error. Returns the token before it when its kind is
/// one of `previous_kinds`.
fn lone_token_error_after<'tree>(
    error: tree_sitter::Node<'tree>,
    previous_kinds: &[&str],
    tokens: &Tokens<'tree>,
) -> Option<tree_sitter::Node<'tree>> {
    if !error.is_error() || error.child_count() != 1 || error.child(0)?.child_count() != 0 {
        return None;
    }
    in_statement_list(error)?;
    let previous = tokens.last_before(error.start_byte())?;
    previous_kinds
        .contains(&previous.kind())
        .then_some(previous)
}

/// `Some(())` when `node` sits in a `statement_list` with no error node between.
fn in_statement_list(node: tree_sitter::Node) -> Option<()> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "statement_list" {
            return Some(());
        }
        // An error inside another error is part of a recovery cascade (a missing
        // `end`), where alc reports other codes.
        if parent.is_error() {
            return None;
        }
        ancestor = parent.parent();
    }
    None
}

/// Recognise the shape tree-sitter produces for a statement that lacks its
/// `;` and is followed by another statement: an ERROR node holding exactly the
/// first token of the next statement, inside a `statement_list`, directly after
/// a token that can end an expression. alc reports AL0111 on that same token
/// (checked on a statement start, an `exit`, and a second statement on the same
/// line). Returns the byte offset where the `;` belongs, the end of the token
/// before the error. A following `if` is handled by `missing_semicolon_shape`.
pub fn missing_semicolon_insert_byte(error: tree_sitter::Node) -> Option<usize> {
    missing_semicolon_shape(error, &Tokens::new(root_of(error))).map(|(insert, _)| insert)
}

/// Where the `;` belongs and the range alc reports. Besides the single token
/// shape above, a statement that spans several tokens: an ERROR node holding
/// the whole previous statement (a call expression) followed by an `if`
/// statement. alc reports AL0111 on the `if` keyword only, the `;` belongs at
/// the end of the error node.
fn missing_semicolon_shape<'tree>(
    error: tree_sitter::Node<'tree>,
    tokens: &Tokens<'tree>,
) -> Option<(usize, tree_sitter::Range)> {
    if let Some(previous) = lone_token_error_after(
        error,
        &[
            ")",
            "]",
            "identifier",
            "quoted_identifier",
            "integer",
            "decimal",
            "string",
        ],
        tokens,
    ) {
        return Some((previous.end_byte(), error.range()));
    }
    if !error.is_error()
        || error.child_count() != 1
        || error.child(0)?.kind() != "postfix_expression"
    {
        return None;
    }
    in_statement_list(error)?;
    let next = error.next_sibling()?;
    if next.kind() != "statement" {
        return None;
    }
    let keyword = tokens.first_from(next.start_byte())?;
    if keyword.kind() != "kw_if" {
        return None;
    }
    let before = tokens.last_before(error.start_byte())?;
    matches!(before.kind(), "semicolon" | "kw_begin").then(|| (error.end_byte(), keyword.range()))
}

/// Same check for a diagnostic range: find the ERROR node that starts at
/// `error_start_byte` and return where its `;` belongs.
pub fn missing_semicolon_insert_byte_at(tree: &Tree, error_start_byte: usize) -> Option<usize> {
    let tokens = Tokens::new(tree.root_node());
    let mut node = Some(
        tree.root_node()
            .descendant_for_byte_range(error_start_byte, error_start_byte)?,
    );
    while let Some(current) = node {
        // The reported range starts at the error node, or at the `if` of the
        // statement that follows an error node.
        for candidate in [Some(current), current.prev_sibling()]
            .into_iter()
            .flatten()
            .filter(|candidate| candidate.is_error())
        {
            if let Some((insert, range)) = missing_semicolon_shape(candidate, &tokens) {
                if range.start_byte == error_start_byte {
                    return Some(insert);
                }
            }
        }
        node = current.parent();
    }
    None
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

        let from_tree = AlParser::errors_from_tree(&result.tree, source.as_bytes());

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
        let source = "codeunit 50100 Test { }";
        let result = parser.parse(source);
        let from_tree = AlParser::errors_from_tree(&result.tree, source.as_bytes());
        assert!(
            from_tree.is_empty(),
            "valid code should yield no errors from the cached tree"
        );
    }

    // List of [Interface IFoo] syntax

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

    fn many_errors_source(line: &str, lines: usize) -> String {
        let mut source = String::from("codeunit 50100 C\n{\n    procedure P()\n    begin\n");
        for _ in 0..lines {
            source.push_str("        ");
            source.push_str(line);
            source.push('\n');
        }
        source.push_str("    end;\n}\n");
        source
    }

    /// Error collection on N errors stays close to linear. Each statement here
    /// lacks its `;`, which takes the path that looks up the previous token.
    #[test]
    fn many_errors_parse_in_bounded_time() {
        for line in ["x := ;", "x := 1"] {
            let source = many_errors_source(line, 5_000);
            let tree = AlParser::parse_quick(&source).tree;
            let started = std::time::Instant::now();
            let errors = AlParser::errors_from_tree(&tree, source.as_bytes());
            let elapsed = started.elapsed();
            eprintln!(
                "many_errors {line:?}: {} errors in {elapsed:?}",
                errors.len()
            );
            assert!(errors.len() >= 4_000, "{line:?}: {} errors", errors.len());
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "{line:?} took {elapsed:?}"
            );
        }
    }

    #[test]
    fn preprocessor_symbols_activate_if_block() {
        let source =
            "codeunit 50100 C\n{\n#if CLEAN24\n    procedure NewApi() begin end;\n#endif\n}\n";
        let names = |result: ParseResult| -> Vec<String> {
            fn walk(symbols: &[crate::SyntaxDocumentSymbol], out: &mut Vec<String>) {
                for symbol in symbols {
                    out.push(symbol.name.clone());
                    walk(symbol.children.as_deref().unwrap_or_default(), out);
                }
            }
            let mut out = Vec::new();
            walk(
                &crate::symbols::extract_document_symbols(&result.tree, source),
                &mut out,
            );
            out
        };
        let has_new_api = |names: Vec<String>| names.iter().any(|name| name == "NewApi");

        assert!(has_new_api(names(AlParser::parse_quick_with_symbols(
            source,
            &["CLEAN24"]
        ))));
        assert!(
            has_new_api(names(AlParser::parse_quick_with_symbols(
                source,
                &["clean24"]
            ))),
            "a second parse with the symbols still sees the block"
        );
        assert!(
            !has_new_api(names(AlParser::parse_quick(source))),
            "a plain parse on the same thread does not inherit the symbols"
        );
        assert!(!has_new_api(names(
            AlParser::new().parse_with_symbols(source, &["CLEAN25"])
        )));
    }
}
