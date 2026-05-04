//! Folding range extraction from tree-sitter trees.

use super::types::{
    SyntaxFoldingRange as FoldingRange, SyntaxFoldingRangeKind as FoldingRangeKind,
};
use tree_sitter::{Node, Tree};

use super::{byte_col_to_utf16_col, get_source_line, traversal::walk_tree};

/// Extract folding ranges from a parsed tree.
///
/// Produces fold regions for:
/// - Object bodies (`{ ... }`)
/// - Procedure/trigger bodies (`begin ... end`)
/// - Section bodies (fields, keys, layout, actions, etc.)
/// - Comment blocks (consecutive `//` lines, `/* ... */`)
/// - `var` sections
/// - `repeat ... until`
/// - `case ... end`
/// - `if ... else` compound blocks
pub fn extract_folding_ranges(tree: &Tree, text: &str) -> Vec<FoldingRange> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut ranges = Vec::new();

    // Extract structural folding ranges from AST
    extract_structural_ranges(root, source, &mut ranges);

    // Extract comment block folding ranges (consecutive // lines)
    extract_comment_block_ranges(text, &mut ranges);

    ranges
}

/// Extract structural folding ranges by walking the AST.
fn extract_structural_ranges(root: Node, source: &[u8], ranges: &mut Vec<FoldingRange>) {
    walk_tree(root, &mut |node| {
        match node.kind() {
            // Object declarations fold their entire body
            "object_declaration" => {
                if let Some(body) = node.child_by_field_name("body") {
                    add_range(body, FoldingRangeKind::Region, source, ranges);
                }
            }

            // Multi-line structural nodes: procedures, blocks, sections, control flow
            "procedure_declaration"
            | "trigger_declaration"
            | "event_procedure_declaration"
            | "begin_end_block"
            | "object_section"
            | "object_body"
            | "braced_block"
            | "var_section"
            | "object_var_section"
            | "if_statement"
            | "case_statement"
            | "for_statement"
            | "foreach_statement"
            | "while_statement"
            | "repeat_statement"
            | "with_statement"
            | "enum_value_declaration"
                if node.start_position().row < node.end_position().row =>
            {
                add_range(node, FoldingRangeKind::Region, source, ranges);
            }

            // Block comments
            "comment" => {
                let start = node.start_position();
                let end = node.end_position();
                // Only fold multi-line block comments (/* ... */)
                if start.row < end.row {
                    let start_line_str = get_source_line(source, start.row);
                    let end_line_str = get_source_line(source, end.row);
                    ranges.push(FoldingRange {
                        start_line: start.row as u32,
                        start_character: Some(byte_col_to_utf16_col(start_line_str, start.column)),
                        end_line: end.row as u32,
                        end_character: Some(byte_col_to_utf16_col(end_line_str, end.column)),
                        kind: Some(FoldingRangeKind::Comment),
                    });
                }
            }

            _ => {}
        }
    });
}

/// Add a folding range from a node.
fn add_range(node: Node, kind: FoldingRangeKind, source: &[u8], ranges: &mut Vec<FoldingRange>) {
    let start = node.start_position();
    let end = node.end_position();
    let start_line_str = get_source_line(source, start.row);
    let end_line_str = if end.row == start.row {
        start_line_str
    } else {
        get_source_line(source, end.row)
    };
    ranges.push(FoldingRange {
        start_line: start.row as u32,
        start_character: Some(byte_col_to_utf16_col(start_line_str, start.column)),
        end_line: end.row as u32,
        end_character: Some(byte_col_to_utf16_col(end_line_str, end.column)),
        kind: Some(kind),
    });
}

/// Extract folding ranges for blocks of consecutive `//` comment lines.
fn extract_comment_block_ranges(text: &str, ranges: &mut Vec<FoldingRange>) {
    let mut block_start: Option<u32> = None;
    let mut block_end: u32 = 0;

    for (line_num, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            if block_start.is_none() {
                block_start = Some(line_num as u32);
            }
            block_end = line_num as u32;
        } else {
            if let Some(start) = block_start {
                // Only fold if the block spans at least 2 lines
                if block_end > start {
                    ranges.push(FoldingRange {
                        start_line: start,
                        start_character: None,
                        end_line: block_end,
                        end_character: None,
                        kind: Some(FoldingRangeKind::Comment),
                    });
                }
            }
            block_start = None;
        }
    }

    // Handle trailing comment block
    if let Some(start) = block_start {
        if block_end > start {
            ranges.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line: block_end,
                end_character: None,
                kind: Some(FoldingRangeKind::Comment),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;

    #[test]
    fn test_folding_codeunit() {
        let src = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;

    procedure DoAnother()
    begin
        Message('World');
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let ranges = extract_folding_ranges(&result.tree, src);
        // Should have ranges for: object body, proc1, begin..end1, proc2, begin..end2
        assert!(
            ranges.len() >= 3,
            "Expected at least 3 folding ranges, got {}",
            ranges.len()
        );
    }

    #[test]
    fn test_folding_comment_blocks() {
        let src = r#"// This is a comment block
// that spans multiple lines
// and should be foldable
codeunit 50100 Test
{
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let ranges = extract_folding_ranges(&result.tree, src);
        let comment_ranges: Vec<_> = ranges
            .iter()
            .filter(|r| r.kind == Some(FoldingRangeKind::Comment))
            .collect();
        assert!(
            !comment_ranges.is_empty(),
            "Should have at least one comment folding range"
        );
        assert_eq!(comment_ranges[0].start_line, 0);
    }

    #[test]
    fn test_folding_empty_source() {
        let mut parser = AlParser::new();
        let ranges = extract_folding_ranges(&parser.parse("").tree, "");
        assert!(ranges.is_empty());
    }

    #[test]
    fn test_folding_consecutive_comments_at_top() {
        let mut parser = AlParser::new();
        let source = "// Line 1\n// Line 2\n// Line 3\ncodeunit 50100 Test { }";
        let result = parser.parse(source);
        let ranges = extract_folding_ranges(&result.tree, source);
        // Should have a comment block fold for the 3 consecutive comment lines
        let comment_folds: Vec<_> = ranges
            .iter()
            .filter(|r| {
                r.kind
                    .as_ref()
                    .is_some_and(|k| matches!(k, FoldingRangeKind::Comment))
            })
            .collect();
        assert!(!comment_folds.is_empty(), "Should have comment block fold");
    }

    #[test]
    fn test_folding_procedure_body() {
        let mut parser = AlParser::new();
        let source = r#"codeunit 50100 Test {
    procedure LongProc()
    begin
        Message('a');
        Message('b');
        Message('c');
    end;
}"#;
        let result = parser.parse(source);
        let ranges = extract_folding_ranges(&result.tree, source);
        assert!(
            !ranges.is_empty(),
            "Should have folding ranges for procedure"
        );
    }

    #[test]
    fn test_folding_single_comment_line_no_fold() {
        let mut parser = AlParser::new();
        let source = "// Just one comment line\ncodeunit 50100 Test { }";
        let result = parser.parse(source);
        let ranges = extract_folding_ranges(&result.tree, source);
        // A single comment line should NOT produce a comment block fold
        let single_line_comment_folds: Vec<_> = ranges
            .iter()
            .filter(|r| r.kind == Some(FoldingRangeKind::Comment) && r.start_line == r.end_line)
            .collect();
        // Single-line comment blocks should not exist (block must span 2+ lines)
        assert!(
            single_line_comment_folds.is_empty(),
            "Single comment line should not produce fold"
        );
    }
}
