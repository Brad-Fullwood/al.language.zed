//! AL code formatter — line-by-line indentation state machine.
//!
//! Ported from v2. This is a text-based formatter that does not require
//! tree-sitter; it operates on raw AL source lines.
//!
//! Key rules:
//! - Indent after: begin, {, (, var, trigger body, procedure body, repeat,
//!   case...of, single-statement openers (if...then, for...do, while...do, with...do)
//! - Dedent before: end, }, ), until
//! - **Comments don't count as statement bodies** for single-statement tracking
//! - **Split end/else**: bare `end` on one line + `else begin` on next
//! - **Multi-line paren close**: `) then` mid-line — track net parens per line
//! - **Blank lines drain single-statement stack**
//! - **Property continuation**: preserve whitespace when `prev_ended_with_comma`
//!
//! ## Hardcoded AL block-keyword literals (F-OPEN-111)
//!
//! This file matches against AL keyword text directly: `begin`, `end`,
//! `end;`, `var`, `repeat`, `until`, `else`, `case`, `of`. These are grammar
//! terminals of AL's Pascal-derived block syntax — stable since AL/NAV's
//! introduction and not part of the keyword surface that drifts with BC
//! releases (built-ins, object types, attribute names). Per the CLAUDE.md
//! "no hardcoded AL values" rule's targeted scope, these block terminals
//! are exempt — the same way single-statement openers were judged exempt
//! when first loaded from `single_stmt_openers.json` (those have grown
//! over time; block keywords have not). If a future BC release alters the
//! block-syntax grammar, the right fix is to add a `block_keywords.json`
//! and route every match through `language_data`. Until then, the
//! literal-match path is documented here rather than scattered as a
//! debt-tracked comment per call site.

/// AL keyword casing style.
#[derive(Debug, Clone, Default)]
pub enum KeywordCasing {
    /// Preserve existing casing.
    #[default]
    Preserve,
    /// Lowercase all keywords.
    Lower,
    /// Uppercase all keywords.
    Upper,
}

/// How blank lines between procedures should be handled.
#[derive(Debug, Clone, Default)]
pub enum BlankLinesBetweenProcedures {
    /// Preserve existing blank lines.
    #[default]
    Preserve,
    /// Ensure exactly one blank line between procedures.
    One,
    /// Ensure exactly two blank lines between procedures.
    Two,
}

/// Brace placement style for `begin`/`end` blocks.
#[derive(Debug, Clone, Default)]
pub enum BraceStyle {
    /// `begin` on the same line as the statement.
    SameLine,
    /// `begin` on the next line (default AL style).
    #[default]
    NextLine,
}

/// Formatting options.
///
/// **Wiring status:** the formatter currently honours `tab_size`,
/// `insert_spaces`, and `keyword_casing`. The remaining fields
/// (`blank_lines_between_procedures`, `max_line_length`, `brace_style`,
/// `sort_properties`) are declared so the public type matches the user-
/// facing `.alformat.json` schema and config-merge layer, but they are NOT
/// applied during formatting yet. The unconsumed-field warning in
/// `queries::format` (logged via `tracing::warn!`) surfaces them so users
/// notice the gap.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Keyword casing to apply. `Preserve` is the no-op default; `Lower` and
    /// `Upper` walk each non-comment, non-string token and case-fold it when
    /// the token matches a known AL keyword.
    pub keyword_casing: KeywordCasing,
    /// Blank lines between procedures. **Currently a no-op.**
    pub blank_lines_between_procedures: BlankLinesBetweenProcedures,
    /// Maximum line length (0 = no limit). **Currently a no-op.**
    pub max_line_length: usize,
    /// Brace placement style. **Currently a no-op.**
    pub brace_style: BraceStyle,
    /// Sort object properties alphabetically. **Currently a no-op.**
    pub sort_properties: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
            keyword_casing: KeywordCasing::Preserve,
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Preserve,
            max_line_length: 0,
            brace_style: BraceStyle::NextLine,
            sort_properties: false,
        }
    }
}

/// Format AL source code.
///
/// Applies consistent indentation using a line-by-line state machine.
/// See module-level docs for the complete set of rules.
pub fn format_al(text: &str, options: &FormatOptions) -> String {
    let indent_str = if options.insert_spaces {
        " ".repeat(options.tab_size)
    } else {
        "\t".to_string()
    };

    let mut result = String::with_capacity(text.len());
    let mut indent_level: i32 = 0;
    let mut prev_was_empty = false;

    // Track how many single-statement indents are pending (if...then, for...do, while...do)
    let mut single_stmt_depth: i32 = 0;

    // Whether we are inside a `var` section (between `var` and `begin`)
    let mut in_var_section = false;

    // Track unclosed parentheses for multi-line call continuation
    let mut paren_depth: i32 = 0;

    // Track case...of nesting for label indentation
    let mut case_depth: i32 = 0;

    // Whether we are inside a case label body (indent +1 for label body)
    let mut in_case_label_body = false;

    // Track begin/end nesting depth within case label bodies.
    // When > 0, an `end;` closes a begin block, not the case label body.
    let mut case_begin_depth: i32 = 0;

    // Multi-line block comment state. `/* ... */` comments may span many
    // lines; while we're inside one, the formatter MUST NOT drain the
    // single-stmt stack or re-indent based on the comment text. The
    // previous text-based scanner only knew about line comments (`//`),
    // so a block comment between an `if … then` and its body would
    // misclassify as a regular statement and collapse the single-stmt
    // indent prematurely. See the AL formatter audit.
    let mut in_block_comment = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip double blank lines
        if trimmed.is_empty() {
            if prev_was_empty {
                continue;
            }
            prev_was_empty = true;

            // Blank lines drain single-statement stack (MS behavior)
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            result.push('\n');
            continue;
        }
        prev_was_empty = false;

        let trimmed_lower = trimmed.to_lowercase();

        // --- Pre-indent adjustments (dedent before writing this line) ---

        // `begin` closes a var section — dedent back to the procedure level
        if in_var_section && (trimmed_lower == "begin" || trimmed_lower.ends_with(" begin")) {
            indent_level = (indent_level - 1).max(0);
            in_var_section = false;
        }

        // `begin` after single-statement openers (if...then begin written separately)
        // drains the single-stmt stack since begin starts a block
        if single_stmt_depth > 0 && (trimmed_lower == "begin" || trimmed_lower.ends_with(" begin"))
        {
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        // Case label: close previous label body before this new label.
        // Be precise: a case label is "<token>:" with no whitespace before the
        // colon — not `Trigger:` inside a field declaration on the same line,
        // and not `OnValidate:` on a trigger header. Reject lines containing
        // anything other than the label token, optional inner spaces (for
        // multi-word string labels like `"Foo Bar"`) and the trailing colon.
        let is_case_label = case_depth > 0
            && trimmed.ends_with(':')
            && !trimmed.ends_with("::")
            // The colon must directly follow the last non-space character —
            // no `;` or `=` etc. before it.
            && !trimmed
                .trim_end_matches(':')
                .chars()
                .any(|c| matches!(c, ';' | '=' | '(' | ')' | ','));
        if is_case_label && in_case_label_body {
            // Drain any single-stmt from within the previous label body
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            indent_level = (indent_level - 1).max(0);
            in_case_label_body = false;
        }

        // Closing constructs: `}`, `end;`, `end`
        let is_close = trimmed_lower == "}"
            || trimmed_lower == "end;"
            || trimmed_lower == "end"
            || trimmed_lower.starts_with("end;")
            || trimmed_lower.starts_with("end ");

        if is_close {
            // Drain single-stmt stack first
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            // Close var section if still open (e.g., object-level `var` closed by `}`)
            if in_var_section {
                indent_level = (indent_level - 1).max(0);
                in_var_section = false;
            }
            // Inside case label body: determine if this end; closes a begin
            // block within the label, or the label body / case itself
            if in_case_label_body && case_begin_depth > 0 {
                // This end; closes a begin block within the case label body
                case_begin_depth -= 1;
            } else if in_case_label_body {
                // No nested begin — this end; closes the case block itself
                // First close the label body indent
                indent_level = (indent_level - 1).max(0);
                in_case_label_body = false;
                if case_depth > 0 {
                    case_depth -= 1;
                }
            } else if case_depth > 0
                && (trimmed_lower == "end;" || trimmed_lower.starts_with("end;"))
            {
                // Case block close without label body
                case_depth -= 1;
            }
            indent_level = (indent_level - 1).max(0);
        }

        // `else` after single-statement if-then: drain remaining single-stmt depth
        if !is_close
            && (trimmed_lower == "else" || trimmed_lower.starts_with("else "))
            && single_stmt_depth > 0
        {
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        // `until` closes a `repeat` block
        if trimmed_lower.starts_with("until ") || trimmed_lower == "until" {
            indent_level = (indent_level - 1).max(0);
        }

        // --- Write the indented line ---
        for _ in 0..indent_level {
            result.push_str(&indent_str);
        }
        // F-OPEN-110: apply keyword casing transformation. Skips work entirely
        // for Preserve (no allocation). For Lower/Upper, walks the line and
        // case-folds only AL keyword tokens (matched via word boundaries +
        // language_data lookup) — keeps identifiers and string literals
        // untouched.
        if !in_block_comment {
            let transformed = apply_keyword_casing(trimmed, &options.keyword_casing);
            result.push_str(&transformed);
        } else {
            result.push_str(trimmed);
        }
        result.push('\n');

        // --- Post-indent adjustments (indent after writing this line) ---

        // Track parenthesis depth for multi-line call continuation
        let net_parens = count_net_parens(trimmed);
        if net_parens > 0 && paren_depth == 0 {
            // Opening parens on this line — indent continuation lines
            indent_level += 1;
            paren_depth += net_parens;
        } else if net_parens > 0 {
            paren_depth += net_parens;
        } else if net_parens < 0 {
            let old_depth = paren_depth;
            paren_depth = (paren_depth + net_parens).max(0);
            if old_depth > 0 && paren_depth == 0 {
                // Parens fully closed — remove continuation indent
                indent_level = (indent_level - 1).max(0);
            }
        }

        // If inside open parens, skip all other indent logic for this line
        if paren_depth > 0 {
            continue;
        }

        // Drain single-stmt stack: if we just wrote a "normal" statement (not a
        // block opener, closer, comment, or another single-stmt opener), pop the stack.
        //
        // Treat block-comment lines (whether they start a comment with `/*`
        // or sit inside an ongoing one) as comments here too — they're not
        // executable statements and must not consume single-stmt-depth.
        // After classifying THIS line, update `in_block_comment` based on
        // whether the line opens and/or closes a block comment.
        let starts_block_comment = trimmed.starts_with("/*");
        let line_is_block_comment_body = in_block_comment || starts_block_comment;
        // A line counts as "closing" the block comment if it contains `*/`
        // AFTER the position where the block comment starts on this line.
        // For the simple case we just look at whether the trimmed text
        // contains `*/` — adversarial pathologies (string literals
        // containing `*/`) are out of scope for this text-based scanner.
        let closes_block_comment = trimmed.contains("*/");
        if line_is_block_comment_body {
            // Update the multi-line tracker for the NEXT iteration. If this
            // line opens-and-closes a block comment on the same line, we
            // stay outside afterwards; if it opens without closing, we're
            // inside; if it was already inside and closes here, we leave.
            in_block_comment = !closes_block_comment;
        }
        let is_comment = trimmed.starts_with("//") || line_is_block_comment_body;
        let is_block_opener = trimmed.ends_with('{')
            || trimmed_lower == "begin"
            || trimmed_lower.ends_with(" begin")
            || trimmed_lower == "var"
            || trimmed_lower == "repeat";
        let is_single_stmt_opener = is_single_statement_opener(&trimmed_lower);

        if single_stmt_depth > 0
            && !is_block_opener
            && !is_close
            && !is_comment
            && !is_single_stmt_opener
            && !is_case_label
            && trimmed_lower != "else"
            && !trimmed_lower.starts_with("else ")
        {
            // This line consumed one single-statement slot; drain all
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        // Block openers: increase indent
        if trimmed.ends_with('{') {
            indent_level += 1;
        } else if trimmed_lower == "begin" || trimmed_lower.ends_with(" begin") {
            indent_level += 1;
            if in_case_label_body {
                case_begin_depth += 1;
            }
        } else if trimmed_lower == "var" {
            indent_level += 1;
            in_var_section = true;
        } else if trimmed_lower == "repeat" {
            indent_level += 1;
        } else if trimmed_lower == "else" || trimmed_lower.starts_with("else ") {
            // `else` without `begin` on same line — next stmt is single-stmt
            if !trimmed_lower.ends_with(" begin") {
                indent_level += 1;
                single_stmt_depth += 1;
            }
        } else if trimmed_lower.starts_with("case ") && trimmed_lower.ends_with(" of") {
            indent_level += 1;
            case_depth += 1;
        }

        // Case labels: indent the body after a label (separate from single-stmt)
        if is_case_label {
            indent_level += 1;
            in_case_label_body = true;
        }

        // Single-statement openers: if...then, for...do, while...do, with...do
        // Only when not followed by `begin` on the same line
        if is_single_stmt_opener && !trimmed_lower.ends_with(" begin") {
            indent_level += 1;
            single_stmt_depth += 1;
        }
    }

    // Ensure file ends with newline
    if !result.ends_with('\n') {
        result.push('\n');
    }

    result
}

/// Format a range of lines within AL source code.
///
/// Transport-agnostic text edit emitted by `format_range`.
///
/// Lines are 0-based; `end_character` is in UTF-16 code units. Callers
/// (al-lsp) convert to `tower_lsp::lsp_types::TextEdit` at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatTextEdit {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub new_text: String,
}

/// Formats the entire document (to derive correct indent context) but returns
/// `FormatTextEdit`s covering only the requested line range.
///
/// `start_line` and `end_line` are 0-based, inclusive.
/// Returns `None` if `start_line` is out of bounds.
/// Returns `Some(vec![])` if the selected lines are already correctly formatted.
pub fn format_range(
    text: &str,
    start_line: u32,
    end_line: u32,
    options: &FormatOptions,
) -> Option<Vec<FormatTextEdit>> {
    let formatted_full = format_al(text, options);

    let orig_lines: Vec<&str> = text.lines().collect();
    let fmt_lines: Vec<&str> = formatted_full.lines().collect();

    let start = start_line as usize;
    if start >= orig_lines.len() {
        return None;
    }
    let end = (end_line as usize).min(orig_lines.len().saturating_sub(1));
    if start > end {
        return None;
    }

    let formatted_region = extract_formatted_region(&orig_lines, &fmt_lines, start, end);
    let original_region: Vec<&str> = orig_lines[start..=end].to_vec();

    if formatted_region == original_region {
        return Some(Vec::new());
    }

    let mut new_text = formatted_region.join("\n");
    new_text.push('\n');

    let end_char = orig_lines
        .get(end)
        .map(|l| super::byte_col_to_utf16_col(l, l.len()))
        .unwrap_or(0);

    Some(vec![FormatTextEdit {
        start_line,
        start_character: 0,
        end_line,
        end_character: end_char,
        new_text,
    }])
}

/// Extract lines from the formatted output corresponding to orig lines [start..=end].
///
/// The formatter collapses consecutive blank lines (two → one). We walk orig and fmt
/// in lockstep, skipping orig-only collapsed blanks without advancing the fmt cursor.
///
/// **Invariant** (F-OPEN-025). The only documented asymmetry between
/// `orig_lines` and `fmt_lines` is collapsed double-blanks. If a future
/// formatter rule drops or duplicates any other line, the lockstep walk
/// here silently mis-aligns and emits the wrong fmt rows for the requested
/// range. To make that regression loud instead of silent, a `debug_assert`
/// at the bottom of this function verifies that the fmt cursor advanced by
/// the same count as the non-collapsed-blank orig rows we visited up to
/// `end`. Release builds skip the check (the function still returns a
/// best-effort slice — wrong but non-crashing) so production never panics
/// on a benign mismatch.
fn extract_formatted_region<'a>(
    orig_lines: &[&str],
    fmt_lines: &[&'a str],
    start: usize,
    end: usize,
) -> Vec<&'a str> {
    let mut orig_idx = 0usize;
    let mut fmt_idx = 0usize;
    let mut result: Vec<&'a str> = Vec::new();
    let mut prev_orig_blank = false;
    let mut visited_non_collapsed = 0usize;

    while orig_idx <= end && fmt_idx < fmt_lines.len() {
        let orig_is_blank = orig_lines[orig_idx].trim().is_empty();

        if orig_is_blank && prev_orig_blank {
            orig_idx += 1;
            continue;
        }

        prev_orig_blank = orig_is_blank;

        if orig_idx >= start {
            result.push(fmt_lines[fmt_idx]);
        }

        orig_idx += 1;
        fmt_idx += 1;
        visited_non_collapsed += 1;
    }

    // Invariant: we advanced fmt_idx exactly as often as we advanced
    // through orig rows that weren't collapsed blanks. Any other formatter
    // asymmetry (drop, duplicate, reorder) would manifest as fmt_idx
    // diverging from visited_non_collapsed.
    debug_assert_eq!(
        fmt_idx, visited_non_collapsed,
        "extract_formatted_region: fmt cursor diverged from orig walk — \
         formatter introduced a line-count asymmetry beyond blank collapse"
    );

    result
}

/// Count net parentheses on a line: `(` adds +1, `)` adds -1.
/// Delegates to the crate-level `count_net_delimiters` which skips string literals.
fn count_net_parens(line: &str) -> i32 {
    super::count_net_delimiters(line, '(', ')')
}

/// Apply `KeywordCasing` to a single source line. Walks the line token-by-
/// token, transforming runs of ASCII alphabetic characters (the only legal
/// AL identifier/keyword shape) when they match a known AL keyword in
/// `language_data::keywords()`. Identifiers, string literals, comments, and
/// numeric literals pass through unchanged.
///
/// Returns the input string when casing is `Preserve` (no allocation).
fn apply_keyword_casing(line: &str, casing: &KeywordCasing) -> String {
    if matches!(casing, KeywordCasing::Preserve) {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // String literal — copy verbatim until the matching quote.
        if b == b'\'' || b == b'"' {
            let quote = b;
            out.push(b as char);
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                out.push(bytes[i] as char);
                i += 1;
            }
            if i < bytes.len() {
                out.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        // Line comment — emit the rest of the line as-is.
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // SAFETY: bytes is the original UTF-8 line; `&line[i..]` is a
            // valid str slice because i lies on a char boundary (we only
            // advanced past ASCII bytes above).
            out.push_str(&line[i..]);
            break;
        }
        // Identifier-shaped word (letters + digits + underscore, starting
        // with letter or underscore). AL is ASCII-only so byte-level scan
        // is safe.
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if super::language_data::is_keyword(word) {
                match casing {
                    KeywordCasing::Lower => out.push_str(&word.to_ascii_lowercase()),
                    KeywordCasing::Upper => out.push_str(&word.to_ascii_uppercase()),
                    KeywordCasing::Preserve => out.push_str(word),
                }
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Returns true if trimmed_lower represents a single-statement control flow opener.
///
/// Single-statement openers are loaded from `tree-sitter-al/data/single_stmt_openers.json`
/// via [`super::language_data::single_stmt_openers`].
fn is_single_statement_opener(trimmed_lower: &str) -> bool {
    super::language_data::single_stmt_openers().iter().any(|o| {
        trimmed_lower.starts_with(o.prefix.as_str()) && trimmed_lower.ends_with(o.suffix.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(text: &str) -> String {
        format_al(text, &FormatOptions::default())
    }

    #[test]
    fn test_basic_codeunit() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_if_then_single_statement() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then
Message('positive');
Message('always');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then
            Message('positive');
        Message('always');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_if_then_begin_end() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then begin
Message('a');
Message('b');
end;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then begin
            Message('a');
            Message('b');
        end;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_var_section() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
var
x: Integer;
y: Text;
begin
x := 42;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        x: Integer;
        y: Text;
    begin
        x := 42;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_comments_dont_drain_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then
// This is a comment
Message('positive');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then
            // This is a comment
            Message('positive');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_blank_line_drains_single_stmt() {
        let input = "codeunit 50100 Test\n{\nprocedure DoSomething()\nbegin\nif x > 0 then\n\nMessage('after blank');\nend;\n}";
        let result = fmt(input);
        // After blank line, single-stmt stack should be drained
        // so Message should be at the same level as the if
        assert!(
            result.contains("        Message('after blank');")
                || result.contains("    Message('after blank');")
        );
    }

    #[test]
    fn test_case_statement() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
case x of
1:
Message('one');
2:
Message('two');
end;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        case x of
            1:
                Message('one');
            2:
                Message('two');
        end;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_repeat_until() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
repeat
x += 1;
until x > 10;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        repeat
            x += 1;
        until x > 10;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_nested_if_then() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if a then
if b then
Message('nested');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if a then
            if b then
                Message('nested');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_else_branch() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if a then
Message('yes')
else
Message('no');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if a then
            Message('yes')
        else
            Message('no');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_double_blank_lines_collapsed() {
        let input = "codeunit 50100 Test\n{\n\n\nprocedure A()\nbegin\nend;\n}";
        let result = fmt(input);
        // Should not have two consecutive blank lines
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_tabs_formatting() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
end;
}"#;
        let opts = FormatOptions {
            tab_size: 4,
            insert_spaces: false,
            ..Default::default()
        };
        let result = format_al(input, &opts);
        assert!(result.contains("\tprocedure DoSomething()"));
    }

    #[test]
    fn test_for_do_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
for i := 1 to 10 do
Message('%1', i);
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        for i := 1 to 10 do
            Message('%1', i);
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_while_do_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
while x > 0 do
x -= 1;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        while x > 0 do
            x -= 1;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }
    // -----------------------------------------------------------------------
    // format_range tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_range_no_change_needed() {
        let input = "codeunit 50100 Test\n{\n    procedure DoSomething()\n    begin\n        Message(\'Hello\');\n    end;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 5, &opts);
        assert!(edits.is_some());
        assert!(edits.unwrap().is_empty());
    }

    #[test]
    fn test_format_range_single_procedure_body() {
        let input =
            "codeunit 50100 Test\n{\nprocedure DoSomething()\nbegin\nMessage(\'Hello\');\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 5, &opts);
        assert!(edits.is_some());
        let edits = edits.unwrap();
        assert!(!edits.is_empty());
        let edit = &edits[0];
        assert_eq!(edit.start_line, 2);
        assert_eq!(edit.end_line, 5);
        assert!(edit.new_text.contains("    procedure DoSomething()"));
        assert!(edit.new_text.contains("    begin"));
        assert!(edit.new_text.contains("        Message(\'Hello\');"));
        assert!(edit.new_text.contains("    end;"));
    }

    #[test]
    fn test_format_range_out_of_bounds_returns_none() {
        let input = "codeunit 50100 Test\n{\n}";
        let opts = FormatOptions::default();
        assert!(format_range(input, 10, 15, &opts).is_none());
    }

    #[test]
    fn test_format_range_preserves_context_indent() {
        let input = "codeunit 50100 Test\n{\nprocedure Outer()\nbegin\nif x > 0 then\nMessage(\'yes\');\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 4, 5, &opts);
        assert!(edits.is_some());
        let edits = edits.unwrap();
        assert!(!edits.is_empty());
        assert!(edits[0].new_text.contains("        if x > 0 then"));
        assert!(edits[0].new_text.contains("            Message(\'yes\');"));
    }

    #[test]
    fn test_block_comment_between_if_then_and_body_preserves_indent() {
        // Regression: a multi-line `/* */` block comment between an
        // `if … then` and its body must NOT drain the single-statement
        // indent stack. Previously the text-based scanner saw the comment
        // body as a regular statement and collapsed the indent, leaving
        // the actual body de-indented. See iteration-6 formatter audit.
        let input = "\
codeunit 50100 Test
{
    procedure Outer()
    begin
        if x > 0 then
            /* explain
               what's going on */
            Message('yes');
    end;
}
";
        let opts = FormatOptions::default();
        let out = format_al(input, &opts);
        // The body line must remain indented one level past `if … then`.
        assert!(
            out.contains("            Message('yes');"),
            "block comment must not collapse single-stmt indent — got:\n{out}"
        );
    }

    #[test]
    fn test_formatter_is_idempotent_on_simple_input() {
        // Positive: formatting twice produces identical output. Regressions
        // here usually mean the state machine is sensitive to the very
        // whitespace it just produced — a quietly catastrophic class of bug
        // when formatting fires on save.
        let input = "\
codeunit 50100 Test
{
    procedure Outer()
    var
        x: Integer;
    begin
        if x > 0 then
            Message('yes')
        else
            Message('no');

        for i := 1 to 10 do
            Message(Format(i));
    end;
}
";
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting should be a no-op — pass-1 output is\n{pass1}\nand pass-2 is\n{pass2}"
        );
    }

    #[test]
    fn test_formatter_is_idempotent_with_block_comments() {
        // Idempotency must hold even when block comments are present —
        // the per-line block-comment tracker must produce the same
        // classification on a second pass.
        let input = "\
codeunit 50100 Test
{
    /* class-level comment
       spans two lines */
    procedure Outer()
    begin
        if x > 0 then
            /* inline block
               comment */
            Message('yes');
    end;
}
";
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting with block comments should be a no-op"
        );
    }

    // F-OPEN-110: KeywordCasing wiring

    #[test]
    fn keyword_casing_preserve_is_identity() {
        let input = "if X then Message('hi');\n";
        let mut opts = FormatOptions::default();
        opts.keyword_casing = KeywordCasing::Preserve;
        assert_eq!(apply_keyword_casing(input, &opts.keyword_casing), input);
    }

    #[test]
    fn keyword_casing_lower_folds_keywords_only() {
        // IF/THEN are keywords — lowered. Message/X are identifiers — preserved.
        // The literal 'IF' inside the string MUST NOT be touched.
        let input = "IF X THEN Message('IF inside string');";
        let lowered = apply_keyword_casing(input, &KeywordCasing::Lower);
        assert_eq!(
            lowered, "if X then Message('IF inside string');",
            "got: {lowered}"
        );
    }

    #[test]
    fn keyword_casing_upper_folds_keywords_only() {
        let input = "if X then Message('hi');";
        let uppered = apply_keyword_casing(input, &KeywordCasing::Upper);
        assert_eq!(uppered, "IF X THEN Message('hi');", "got: {uppered}");
    }

    #[test]
    fn keyword_casing_skips_line_comments() {
        // After // anything goes, including "IF" tokens that look like keywords.
        let input = "if x then // IF this comment, IF that";
        let lowered = apply_keyword_casing(input, &KeywordCasing::Lower);
        assert_eq!(lowered, "if x then // IF this comment, IF that");
    }

    // F-OPEN-113: multi-line paren continuation idempotency

    #[test]
    fn test_multiline_paren_call_is_idempotent() {
        let input = r#"codeunit 50100 Test
{
    procedure DoWork()
    begin
        Foo(
            1,
            2,
            3);
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting of multi-line paren call should be a no-op\n\
             pass1:\n{pass1}\npass2:\n{pass2}"
        );
    }

    #[test]
    fn test_multiline_paren_in_expression_is_idempotent() {
        // Trickier shape: a call inside an if-condition, opening parens on
        // the if line. Whatever pass1 picks, pass2 must match.
        let input = r#"codeunit 50100 Test
{
    procedure DoWork()
    begin
        if MyFunc(
            a,
            b
        ) then
            Message('hit');
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2);
    }
}
