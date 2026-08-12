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

    extract_structural_ranges(root, source, &mut ranges);
    extract_region_ranges(root, source, &mut ranges);
    extract_comment_block_ranges(text, &mut ranges);

    ranges
}

#[derive(Clone, Copy)]
enum RegionMarker {
    Start,
    End,
}

/// Classify a `directive` node's text as a `#region` / `#endregion` marker.
///
/// Mirrors the `@fold.region.start` / `@fold.region.end` captures in
/// `folds.scm`: the keyword is matched case-insensitively and requires a name
/// boundary, so `#regional` does not open a fold and `#endregionExtra` does not
/// close one. Any amount of whitespace is allowed between `#` and the keyword.
fn directive_region_kind(text: &str) -> Option<RegionMarker> {
    let rest = text.trim_start().strip_prefix('#')?.trim_start();
    let lower = rest.to_ascii_lowercase();
    if let Some(after) = lower.strip_prefix("endregion") {
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            return Some(RegionMarker::End);
        }
    } else if let Some(after) = lower.strip_prefix("region") {
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            return Some(RegionMarker::Start);
        }
    }
    None
}

/// Fold `#region … #endregion` preprocessor blocks. `directive` nodes span the
/// whole `#…` line; they are paired with a stack so nested regions fold
/// independently and unmatched markers are ignored.
fn extract_region_ranges(root: Node, source: &[u8], ranges: &mut Vec<FoldingRange>) {
    // Collect region markers as owned data — `walk_tree` hands the closure a
    // node that does not outlive the call, so `Node`s cannot be stored.
    struct Marker {
        kind: RegionMarker,
        start_byte: usize,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    }
    let mut markers: Vec<Marker> = Vec::new();
    walk_tree(root, &mut |node| {
        if node.kind() != "directive" {
            return;
        }
        if let Ok(text) = node.utf8_text(source) {
            if let Some(kind) = directive_region_kind(text) {
                let start = node.start_position();
                let end = node.end_position();
                markers.push(Marker {
                    kind,
                    start_byte: node.start_byte(),
                    start_row: start.row,
                    start_col: start.column,
                    end_row: end.row,
                    end_col: end.column,
                });
            }
        }
    });
    markers.sort_by_key(|m| m.start_byte);

    let mut open: Vec<(usize, usize)> = Vec::new();
    for marker in &markers {
        match marker.kind {
            RegionMarker::Start => open.push((marker.start_row, marker.start_col)),
            RegionMarker::End => {
                if let Some((start_row, start_col)) = open.pop() {
                    if start_row < marker.end_row {
                        let start_line_str = get_source_line(source, start_row);
                        let end_line_str = get_source_line(source, marker.end_row);
                        ranges.push(FoldingRange {
                            start_line: start_row as u32,
                            start_character: Some(byte_col_to_utf16_col(start_line_str, start_col)),
                            end_line: marker.end_row as u32,
                            end_character: Some(byte_col_to_utf16_col(end_line_str, marker.end_col)),
                            kind: Some(FoldingRangeKind::Region),
                        });
                    }
                }
            }
        }
    }
}

fn extract_structural_ranges(root: Node, source: &[u8], ranges: &mut Vec<FoldingRange>) {
    walk_tree(root, &mut |node| {
        match node.kind() {
            // Object bodies (`{ ... }`) fold via the `object_body` arm below,
            // which the walk reaches as a child of `object_declaration`. We
            // deliberately do NOT also add a fold here for the declaration's
            // body field — doing so produced a duplicate range for the same
            // region.
            "procedure_declaration"
            | "trigger_declaration"
            | "event_procedure_declaration"
            | "begin_end_block"
            | "object_section"
            | "key_section"
            | "key_declaration"
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
    use crate::AlParser;

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
        assert_no_duplicate_ranges(&ranges);
    }

    /// Assert that no two folding ranges cover the exact same region.
    /// Guards against the duplicate-object-body regression.
    fn assert_no_duplicate_ranges(ranges: &[FoldingRange]) {
        let mut seen = std::collections::HashSet::new();
        for r in ranges {
            let key = (r.start_line, r.start_character, r.end_line, r.end_character);
            assert!(
                seen.insert(key),
                "duplicate folding range for region {key:?}; all ranges: {ranges:?}"
            );
        }
    }

    #[test]
    fn test_folding_no_duplicate_object_body() {
        // The object body must produce exactly one fold, not one from the
        // `object_declaration` arm and another from the `object_body` arm.
        let src = "codeunit 50100 Test\n{\n    procedure P()\n    begin\n        Message('x');\n    end;\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let ranges = extract_folding_ranges(&result.tree, src);
        assert_no_duplicate_ranges(&ranges);

        // There should be exactly one Region fold that starts on the body's
        // opening-brace line (line 1).
        let body_folds: Vec<_> = ranges
            .iter()
            .filter(|r| r.kind == Some(FoldingRangeKind::Region) && r.start_line == 1)
            .collect();
        assert_eq!(
            body_folds.len(),
            1,
            "expected exactly one fold for the object body, got {}: {:?}",
            body_folds.len(),
            body_folds
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
    fn test_folding_table_keys() {
        // Post grammar bump `keys { }` is a `key_section` and each `key(...)` a
        // `key_declaration`; both must still produce folds.
        let src = "table 50100 \"My Table\"\n{\n    keys\n    {\n        key(PK; \"No.\")\n        {\n            Clustered = true;\n        }\n    }\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let ranges = extract_folding_ranges(&result.tree, src);
        // The `keys` block opens on line 3 (its `{`).
        assert!(
            ranges
                .iter()
                .any(|r| r.kind == Some(FoldingRangeKind::Region) && r.start_line == 3),
            "expected a fold for the keys block, got {ranges:?}"
        );
        assert_no_duplicate_ranges(&ranges);
    }

    #[test]
    fn test_folding_region_directives() {
        let src = "codeunit 50100 Test\n{\n    #region Helpers\n    procedure P()\n    begin\n    end;\n    #endregion\n}";
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let ranges = extract_folding_ranges(&result.tree, src);
        // #region on line 2, #endregion on line 6.
        assert!(
            ranges
                .iter()
                .any(|r| r.kind == Some(FoldingRangeKind::Region)
                    && r.start_line == 2
                    && r.end_line == 6),
            "expected a #region fold spanning lines 2..=6, got {ranges:?}"
        );
    }

    #[test]
    fn test_region_marker_name_boundary() {
        assert!(matches!(
            directive_region_kind("#region"),
            Some(RegionMarker::Start)
        ));
        assert!(matches!(
            directive_region_kind("#region MyRegion"),
            Some(RegionMarker::Start)
        ));
        assert!(matches!(
            directive_region_kind("# region Spaced"),
            Some(RegionMarker::Start)
        ));
        assert!(matches!(
            directive_region_kind("#endregion"),
            Some(RegionMarker::End)
        ));
        // Name boundary: these are not region markers.
        assert!(directive_region_kind("#regional").is_none());
        assert!(directive_region_kind("#endregionExtra").is_none());
        assert!(directive_region_kind("#pragma warning disable AA0001").is_none());
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
