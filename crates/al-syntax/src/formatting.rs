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

#[derive(Debug, Clone, Default)]
pub enum KeywordCasing {
    #[default]
    Preserve,
    Lower,
    Upper,
}

#[derive(Debug, Clone, Default)]
pub enum BlankLinesBetweenProcedures {
    #[default]
    Preserve,
    /// Ensure exactly one blank line between procedures.
    One,
    /// Ensure exactly two blank lines between procedures.
    Two,
}

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
/// **Wiring status:** all fields are honoured by `format_al`. `tab_size`,
/// `insert_spaces`, and `keyword_casing` are applied by the main
/// indentation/casing pass; the four A13 fields
/// (`sort_properties`, `blank_lines_between_procedures`, `max_line_length`,
/// `brace_style`) are applied by dedicated post-processing passes that run
/// after the main pass. Each A13 pass is a strict no-op at its default value
/// and is idempotent.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Keyword casing to apply. `Preserve` is the no-op default; `Lower` and
    /// `Upper` walk each non-comment, non-string token and case-fold it when
    /// the token matches a known AL keyword.
    pub keyword_casing: KeywordCasing,
    /// Blank lines between procedures. `Preserve` (default) is a no-op; `One`
    /// / `Two` normalise the gap between a procedure's closing `end;` and the
    /// next member (counting from a leading attribute block).
    pub blank_lines_between_procedures: BlankLinesBetweenProcedures,
    /// Maximum line length (0 = no limit, the default no-op). When set,
    /// over-long single-line object properties are wrapped at top-level commas.
    pub max_line_length: usize,
    /// Brace placement style. `NextLine` (default) is a no-op; `SameLine`
    /// merges a stand-alone `{` onto the preceding opener line.
    pub brace_style: BraceStyle,
    /// Sort object-level properties alphabetically within each contiguous run.
    /// `false` (default) is a no-op.
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

    // FB-18: multi-line property assignments (`Permissions = tabledata A = rm,`)
    // continue on following lines until the terminating `;`. Continuation
    // lines are indented one level past the opener instead of being
    // collapsed to the property's own level.
    let mut in_property_continuation = false;

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

        // `begin` closes a var section — dedent back to the procedure level
        if in_var_section && (trimmed_lower == "begin" || trimmed_lower.ends_with(" begin")) {
            indent_level = (indent_level - 1).max(0);
            in_var_section = false;
        }

        // FB-18: an OBJECT-level `var` section has no closing `begin` — it
        // ends at the next member declaration: an attribute line
        // (`[EventSubscriber(...)]`) or a procedure/trigger header.
        // Previously the next member stayed at variable indentation
        // (verified on a real codeunit: the attribute + `local procedure`
        // header were pushed to var-entry depth while `begin` stayed put).
        if in_var_section {
            let is_member_start = trimmed.starts_with('[')
                || trimmed_lower.starts_with("procedure ")
                || trimmed_lower.starts_with("local ")
                || trimmed_lower.starts_with("internal ")
                || trimmed_lower.starts_with("protected ")
                || trimmed_lower.starts_with("trigger ");
            if is_member_start {
                indent_level = (indent_level - 1).max(0);
                in_var_section = false;
            }
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

        let is_close = trimmed_lower == "}"
            || trimmed_lower == "end;"
            || trimmed_lower == "end"
            || trimmed_lower.starts_with("end;")
            || trimmed_lower.starts_with("end ");

        if is_close {
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

        if trimmed_lower.starts_with("until ") || trimmed_lower == "until" {
            indent_level = (indent_level - 1).max(0);
        }

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

        // FB-18: property-assignment continuations. A non-comment line that
        // ends with `,` outside any parens starts (or stays in) a
        // continuation — the following line(s) indent one extra level until
        // the `;` terminator. Trailing commas outside parens are not valid
        // in executable AL, so this only fires on multi-line property
        // values (Permissions, TableRelation, CalcFormula, …).
        let is_comment_line =
            trimmed.starts_with("//") || trimmed.starts_with("/*") || in_block_comment;
        if in_property_continuation && (trimmed.ends_with(';') || is_close) {
            indent_level = (indent_level - 1).max(0);
            in_property_continuation = false;
        } else if !in_property_continuation && !is_comment_line && trimmed.ends_with(',') {
            indent_level += 1;
            in_property_continuation = true;
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

    if !result.ends_with('\n') {
        result.push('\n');
    }

    // ---- A13 post-processing passes ------------------------------------
    //
    // These run AFTER the indentation/casing state machine above, on the
    // already-formatted text. Each pass is a STRICT no-op at its option's
    // default value (returning the input `String` untouched), and each is
    // INDIVIDUALLY IDEMPOTENT. Because the main pass re-runs first on a
    // second `format_al` call, the composition is also idempotent: the main
    // pass collapses inserted blanks / re-indents wrapped continuations to
    // the exact shape these passes produce, so a second pass reproduces the
    // first pass's output verbatim.
    //
    // Order: sort (reorders property runs) → blank lines (procedure gaps) →
    // wrap (splits long single-line properties) → brace style (merges `{`).
    // Wrapping after sort means a wrapped multi-line property is recognised
    // as already-multi-line on re-entry and skipped; brace merging runs last
    // so the earlier passes always see `{` on its own line.
    let result = sort_object_properties(result, options);
    let result = normalize_blank_lines_between_procedures(result, options);
    let result = wrap_long_property_lines(result, options);
    let result = apply_brace_style(result, options);

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
    // Append a trailing newline so the replacement bridges into the line that
    // follows the selection. The one exception is the document's final line
    // when the original text has no trailing newline: appending `\n` there
    // would inject a newline the document never had. `.lines()` discards the
    // trailing-newline distinction, so consult `text` directly.
    let end_is_last_line = end == orig_lines.len().saturating_sub(1);
    if !end_is_last_line || text.ends_with('\n') {
        new_text.push('\n');
    }

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
        // String literal — copy verbatim until the matching quote. AL escapes
        // a single quote inside a single-quoted literal by doubling it (''), so
        // a `''` pair is content, not a terminator. Mirror the escape handling
        // in count_net_delimiters (mod.rs) so an escaped quote can't desync the
        // scanner and let a later keyword be mis-cased.
        if b == b'\'' || b == b'"' {
            let quote = b;
            out.push(b as char);
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    // AL `''` escape (single quotes only): both bytes are content.
                    if quote == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        out.push('\'');
                        out.push('\'');
                        i += 2;
                    } else {
                        break;
                    }
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            if i < bytes.len() {
                out.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
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

/// Single-statement openers are loaded from `tree-sitter-al/data/single_stmt_openers.json`
/// via [`super::language_data::single_stmt_openers`].
fn is_single_statement_opener(trimmed_lower: &str) -> bool {
    super::language_data::single_stmt_openers().iter().any(|o| {
        trimmed_lower.starts_with(o.prefix.as_str()) && trimmed_lower.ends_with(o.suffix.as_str())
    })
}

// ======================================================================
// A13 post-processing passes and their shared helpers.
//
// All four passes operate on the fully-formatted text emitted by the main
// `format_al` state machine (lines already trimmed-and-reindented, output
// terminated by a single `\n`). They take the buffer by value and return it
// by value so the caller can chain them without cloning.
// ======================================================================

/// The single indentation unit the main pass uses (spaces or one tab).
fn indent_unit(options: &FormatOptions) -> String {
    if options.insert_spaces {
        " ".repeat(options.tab_size)
    } else {
        "\t".to_string()
    }
}

/// Leading whitespace of `line` (everything before the first non-space char).
fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Re-join post-pass output lines into a buffer terminated by a single `\n`.
///
/// `text.lines()` followed by `lines.join("\n")` plus a trailing `\n`
/// round-trips any buffer the main pass produces (which always ends in `\n`),
/// so a pass that does not modify its `lines` reproduces its input verbatim.
fn join_lines(lines: Vec<String>) -> String {
    let mut result = lines.join("\n");
    result.push('\n');
    result
}

/// Byte offset of the first occurrence of `target` at the statement's top
/// level — i.e. outside single/double-quoted spans, outside `()`/`[]`, and
/// before any `//` line comment. `None` if no such occurrence exists.
fn first_top_level(s: &str, target: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                // AL escapes a single quote inside a literal by doubling it.
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return None,
            c if c == target && paren == 0 && bracket == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Byte offset of the property-assignment `=` (the first top-level `=` that is
/// not part of `:=`, `<=`, `>=`, `<>`, `!=`, or `==`). `None` if absent.
fn property_eq_pos(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return None,
            b'=' if paren == 0 && bracket == 0 => {
                let prev = if i > 0 { bytes[i - 1] } else { 0 };
                let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
                if !matches!(prev, b':' | b'<' | b'>' | b'!' | b'=') && next != b'=' {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// A property name is a single ASCII identifier (`Caption`, `TableRelation`,
/// `Permissions`, …): non-empty, only alphanumerics and underscores.
fn is_property_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// True if `trimmed` (an already-trimmed line) opens a property statement —
/// `Name = value …`. Rejects comments, attributes, sub-block openers (`fields`
/// + `{`), and executable assignments (`x := …`) via the name/`=` checks.
fn is_property_opener(trimmed: &str) -> bool {
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('[')
        || trimmed.ends_with('{')
    {
        return false;
    }
    match property_eq_pos(trimmed) {
        Some(pos) => is_property_name(trimmed[..pos].trim()),
        None => false,
    }
}

/// Case-insensitive sort key for a property statement: its name (the text
/// before the top-level `=`), lower-cased.
fn property_sort_key(first_line: &str) -> String {
    let t = first_line.trim();
    match property_eq_pos(t) {
        Some(pos) => t[..pos].trim().to_ascii_lowercase(),
        None => t.to_ascii_lowercase(),
    }
}

/// True if `s` contains a `//` line comment that is OUTSIDE any single- or
/// double-quoted span. Used by the brace-merge pass to avoid commenting out a
/// `{` that would be merged onto a line ending in a trailing comment.
fn has_line_comment_outside_strings(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// PASS 1 — `sort_properties`. Within each object body, sort every contiguous
/// run of object-level (brace depth 1) property statements case-insensitively
/// by property name. Multi-line property values move as a single unit; nothing
/// below depth 1 (fields/keys/layout/actions/triggers/procedures) is touched.
fn sort_object_properties(text: String, options: &FormatOptions) -> String {
    if !options.sort_properties {
        return text;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    // Each pending run element is the full set of physical lines of one
    // property statement.
    let mut run: Vec<Vec<String>> = Vec::new();
    let mut depth: i32 = 0; // brace depth BEFORE the current line
    let mut i = 0;

    fn flush(run: &mut Vec<Vec<String>>, out: &mut Vec<String>) {
        if run.is_empty() {
            return;
        }
        // Stable sort keeps equal-named properties in source order.
        run.sort_by(|a, b| property_sort_key(&a[0]).cmp(&property_sort_key(&b[0])));
        for prop in run.drain(..) {
            out.extend(prop);
        }
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if depth == 1 && is_property_opener(trimmed) {
            // Collect this property's lines: from the opener until the line
            // that carries the statement-terminating top-level `;`.
            let mut j = i;
            let mut prop = vec![line.to_string()];
            while first_top_level(lines[j], b';').is_none() && j + 1 < lines.len() {
                j += 1;
                prop.push(lines[j].to_string());
            }
            // Properties carry no braces, so depth is unchanged; account for
            // any stray braces defensively.
            for l in &prop {
                depth += super::count_net_delimiters(l, '{', '}');
            }
            depth = depth.max(0);
            run.push(prop);
            i = j + 1;
            continue;
        }

        // Any non-property line ends the current run.
        flush(&mut run, &mut out);
        depth += super::count_net_delimiters(line, '{', '}');
        depth = depth.max(0);
        out.push(line.to_string());
        i += 1;
    }
    flush(&mut run, &mut out);
    join_lines(out)
}

/// Member-start lines that the blank-line pass treats as "the next procedure":
/// an attribute block, or a procedure/trigger header.
fn is_procedure_member_start(trimmed: &str) -> bool {
    if trimmed.starts_with('[') {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("procedure ")
        || lower.starts_with("local procedure ")
        || lower.starts_with("internal procedure ")
        || lower.starts_with("protected procedure ")
        || lower.starts_with("trigger ")
}

/// PASS 2 — `blank_lines_between_procedures`. Normalise the blank-line gap
/// between a member-level `end;` and the following procedure/trigger member
/// (or its leading attribute block) to the configured count. Inserts blanks
/// when none exist; leaves everything else alone.
fn normalize_blank_lines_between_procedures(text: String, options: &FormatOptions) -> String {
    let desired = match options.blank_lines_between_procedures {
        BlankLinesBetweenProcedures::Preserve => return text,
        BlankLinesBetweenProcedures::One => 1,
        BlankLinesBetweenProcedures::Two => 2,
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line.to_string());

        // A member-closing `end;` is `<indent>end;` (procedure/trigger bodies
        // sit one indent inside their object; nested `end;` is deeper, so the
        // sibling-indent check below filters those out anyway).
        if line.trim().eq_ignore_ascii_case("end;") {
            let end_indent = leading_ws(line);
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() {
                let next = lines[j];
                if leading_ws(next) == end_indent && is_procedure_member_start(next.trim()) {
                    for _ in 0..desired {
                        out.push(String::new());
                    }
                    i = j; // skip the original blank gap; emit `next` next loop
                    continue;
                }
            }
        }
        i += 1;
    }
    join_lines(out)
}

/// PASS 3 — `max_line_length`. Wrap over-long single-line property statements
/// by splitting at top-level commas (never inside `()`, `[]`, or a string).
/// Continuation lines are indented one unit past the opener — matching how the
/// main pass indents an already-multi-line property — so re-formatting is a
/// no-op. A property with no top-level comma (one long token) is left intact.
fn wrap_long_property_lines(text: String, options: &FormatOptions) -> String {
    let max = options.max_line_length;
    if max == 0 {
        return text;
    }
    let unit = indent_unit(options);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_continuation = false;
    for line in lines {
        let trimmed = line.trim();
        let is_comment =
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*');

        let wrappable = !in_continuation
            && trimmed.ends_with(';')
            && is_property_opener(trimmed)
            && line.chars().count() > max;
        if wrappable {
            out.extend(wrap_property_line(line, &unit));
        } else {
            out.push(line.to_string());
        }

        // Mirror the main pass's property-continuation tracking so a wrapped
        // (now multi-line) property is not re-examined as a fresh opener.
        if in_continuation && trimmed.ends_with(';') {
            in_continuation = false;
        } else if !in_continuation && !is_comment && trimmed.ends_with(',') {
            in_continuation = true;
        }
    }
    join_lines(out)
}

/// Split one over-long property line at top-level commas. Returns the original
/// line unchanged (as a single element) when there is nothing to split.
fn wrap_property_line(line: &str, unit: &str) -> Vec<String> {
    let opener_indent = leading_ws(line);
    let content = line.trim();

    // Collect top-level comma positions (paren/bracket/string aware).
    let bytes = content.as_bytes();
    let mut cuts: Vec<usize> = Vec::new();
    let mut i = 0;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            b',' if paren == 0 && bracket == 0 => cuts.push(i),
            _ => {}
        }
        i += 1;
    }

    if cuts.is_empty() {
        return vec![line.to_string()];
    }

    let cont_indent = format!("{opener_indent}{unit}");
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut seg_start = 0;
    for (idx, &cut) in cuts.iter().enumerate() {
        // Include the comma at the end of this segment.
        let seg = content[seg_start..=cut].trim();
        let indent = if idx == 0 { opener_indent } else { &cont_indent };
        out.push(format!("{indent}{seg}"));
        seg_start = cut + 1;
    }
    // Final segment (carries the terminating `;`).
    let seg = content[seg_start..].trim();
    out.push(format!("{cont_indent}{seg}"));
    out
}

/// True if a stand-alone `{` may be merged onto `prev`. Rejects block keywords
/// (`begin`/`end`), closers (`}`), comment lines, lines already ending in `{`,
/// and — critically — any line carrying a trailing `//` comment (merging there
/// would comment the brace out and produce invalid AL).
fn is_mergeable_brace_target(prev: &str) -> bool {
    let t = prev.trim();
    if t.is_empty()
        || t == "}"
        || t.ends_with('{')
        || t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
    {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower == "begin"
        || lower.ends_with(" begin")
        || lower == "end"
        || lower == "end;"
        || lower.starts_with("end ")
        || lower.starts_with("end;")
    {
        return false;
    }
    // CRITICAL: never merge onto a line with a trailing line comment.
    !has_line_comment_outside_strings(t)
}

/// PASS 4 — `brace_style`. For `SameLine`, merge a stand-alone `{` line onto
/// the preceding mergeable opener line. `NextLine` (default) is a no-op; this
/// pass never moves `begin`/`end` or a closing `}`.
fn apply_brace_style(text: String, options: &FormatOptions) -> String {
    if !matches!(options.brace_style, BraceStyle::SameLine) {
        return text;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.trim() == "{" {
            let do_merge = out
                .last()
                .map(|p| is_mergeable_brace_target(p))
                .unwrap_or(false);
            if do_merge {
                let prev = out.last().unwrap().trim_end().to_string();
                *out.last_mut().unwrap() = format!("{prev} {{");
                continue;
            }
        }
        out.push(line.to_string());
    }
    join_lines(out)
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
    fn test_format_range_last_line_no_trailing_newline() {
        // F-OPEN-112: when the selection ends on the document's final line and
        // the document has no trailing newline, the emitted edit must NOT append
        // a spurious trailing newline.
        let input = "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n        }";
        assert!(!input.ends_with('\n'));
        let opts = FormatOptions::default();
        let edits = format_range(input, 5, 5, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        let edit = &edits[0];
        assert_eq!(edit.start_line, 5);
        assert_eq!(edit.end_line, 5);
        // The over-indented `}` is corrected to column 0, with no extra newline.
        assert_eq!(edit.new_text, "}");
        assert!(!edit.new_text.ends_with('\n'));

        // Applying the edit must reproduce a document that still has no trailing
        // newline.
        let mut applied =
            String::from("codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n");
        applied.push_str(&edit.new_text);
        assert!(!applied.ends_with('\n'));
    }

    #[test]
    fn test_format_range_last_line_with_trailing_newline() {
        // Counterpart to the above: when the document DOES end with a newline,
        // the trailing newline must be preserved in the edit.
        let input = "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n        }\n";
        assert!(input.ends_with('\n'));
        let opts = FormatOptions::default();
        let edits = format_range(input, 5, 5, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "}\n");
    }

    #[test]
    fn test_format_range_non_last_line_keeps_trailing_newline() {
        // A selection that does not reach the final line always keeps the
        // bridging trailing newline, regardless of the document's final newline.
        let input = "codeunit 50100 Test\n{\nprocedure X()\nbegin\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 4, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.ends_with('\n'));
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
        let opts = FormatOptions {
            keyword_casing: KeywordCasing::Preserve,
            ..Default::default()
        };
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

    #[test]
    fn keyword_casing_handles_escaped_single_quotes() {
        // AL escapes a single quote inside a literal by doubling it: 'can''t'
        // is one string. The escaped '' must not desync the scanner — the
        // string content stays verbatim and only the trailing IF/THEN keywords
        // are folded.
        let input = "Message('can''t'); IF x THEN";
        let lowered = apply_keyword_casing(input, &KeywordCasing::Lower);
        assert_eq!(lowered, "Message('can''t'); if x then", "got: {lowered}");
    }

    #[test]
    fn keyword_casing_escaped_quote_does_not_swallow_keyword() {
        // A literal that ends right after an escaped pair must close at the
        // real terminator; the following END keyword is still cased.
        let input = "Error('a''b') END";
        let lowered = apply_keyword_casing(input, &KeywordCasing::Lower);
        assert_eq!(lowered, "Error('a''b') end", "got: {lowered}");
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

    /// FB-18 regression (found on a real customer codeunit): an object-level
    /// `var` section is not closed by `begin` — the next member's attribute
    /// and procedure header must dedent back to member level, and a
    /// multi-line `Permissions = …,` property keeps its continuation line
    /// indented past the opener instead of collapsing to property level.
    #[test]
    fn fb18_attribute_after_object_var_and_property_continuation() {
        let input = r#"codeunit 50104 "AUK Data Management Event Subs"
{
    InherentPermissions = x;
    Permissions = tabledata "Warehouse Shipment Header" = rm,
                  tabledata "Warehouse Shipment Line" = r;

    var
        FieldValueCalcHelper: Codeunit "AUK Field Value Calc Helper";

    [EventSubscriber(ObjectType::Table, Database::"Warehouse Shipment Line", OnAfterInsertEvent, '', false, false)]
    local procedure OnAfterWarehouseShipmentLineInsert(var Rec: Record "Warehouse Shipment Line"; RunTrigger: Boolean)
    begin
        if not RunTrigger then
            exit;
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);

        // The attribute and procedure header sit at member level (4), not
        // var-entry level (8).
        assert!(
            pass1.contains("\n    [EventSubscriber(ObjectType::Table"),
            "attribute must be at member indentation, got:\n{pass1}"
        );
        assert!(
            pass1.contains("\n    local procedure OnAfterWarehouseShipmentLineInsert"),
            "procedure header must be at member indentation, got:\n{pass1}"
        );
        // The Permissions continuation stays indented past the opener.
        assert!(
            pass1.contains("\n        tabledata \"Warehouse Shipment Line\" = r;"),
            "property continuation must keep extra indentation, got:\n{pass1}"
        );
        // And the property after the continuation is unaffected.
        assert!(
            pass1.contains("\n    var\n"),
            "var keyword must stay at member level, got:\n{pass1}"
        );

        // Idempotent: formatting the formatted output changes nothing.
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "format must be idempotent");
    }

    // ===================================================================
    // A13 options: sort_properties
    // ===================================================================

    const SORT_INPUT: &str = "\
table 50100 Test
{
    DataPerCompany = true;
    Caption = 'Test';
    DataClassification = ToBeClassified;

    fields
    {
        field(1; \"No.\"; Code[20])
        {
            Zzz = 1;
            Aaa = 2;
        }
    }
}
";

    #[test]
    fn sort_properties_orders_object_level_run() {
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let out = format_al(SORT_INPUT, &opts);
        // Object-level run is sorted case-insensitively: Caption, Data..., Data...
        let cap = out.find("Caption").unwrap();
        let dcl = out.find("DataClassification").unwrap();
        let dpc = out.find("DataPerCompany").unwrap();
        assert!(cap < dcl && dcl < dpc, "object props not sorted:\n{out}");
        // Field-level (depth 3) properties must NOT be reordered.
        assert!(
            out.find("Zzz = 1;").unwrap() < out.find("Aaa = 2;").unwrap(),
            "depth-3 field properties must be left untouched:\n{out}"
        );
    }

    #[test]
    fn sort_properties_default_is_noop() {
        let opts = FormatOptions::default();
        let out = format_al(SORT_INPUT, &opts);
        // Default preserves source order.
        assert!(
            out.find("DataPerCompany").unwrap() < out.find("Caption").unwrap(),
            "default must preserve property source order:\n{out}"
        );
    }

    #[test]
    fn sort_properties_is_idempotent() {
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let pass1 = format_al(SORT_INPUT, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "sort_properties must be idempotent");
    }

    #[test]
    fn sort_properties_keeps_multiline_value_intact() {
        // A multi-line property must move as one unit, keeping its
        // continuation line attached.
        let input = "\
codeunit 50100 T
{
    Zulu = 1;
    Permissions = tabledata A = rm,
        tabledata B = r;
    Alpha = 2;
}
";
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // Order: Alpha, Permissions, Zulu — and the Permissions continuation
        // stays directly under its opener.
        assert!(out.find("Alpha").unwrap() < out.find("Permissions").unwrap());
        assert!(out.find("Permissions").unwrap() < out.find("Zulu").unwrap());
        assert!(
            out.contains("Permissions = tabledata A = rm,\n        tabledata B = r;"),
            "multi-line value must stay intact:\n{out}"
        );
        let pass2 = format_al(&out, &opts);
        assert_eq!(out, pass2, "must remain idempotent with multi-line values");
    }

    // ===================================================================
    // A13 options: blank_lines_between_procedures
    // ===================================================================

    const PROC_NO_GAP: &str = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
    procedure B()
    begin
    end;
}
";

    #[test]
    fn blank_lines_two_inserts_two() {
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let out = format_al(PROC_NO_GAP, &opts);
        assert!(
            out.contains("    end;\n\n\n    procedure B()"),
            "expected two blank lines between procedures:\n{out}"
        );
    }

    #[test]
    fn blank_lines_one_normalizes_existing_gap() {
        // Three existing blanks (collapsed to one by the main pass) become one.
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;



    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    end;\n\n    procedure B()"),
            "expected exactly one blank line:\n{out}"
        );
        assert!(
            !out.contains("    end;\n\n\n    procedure B()"),
            "must not leave two blanks:\n{out}"
        );
    }

    #[test]
    fn blank_lines_counts_from_attribute_block() {
        // The gap is measured to the leading attribute, not the procedure.
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
    [IntegrationEvent(false, false)]
    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    end;\n\n\n    [IntegrationEvent(false, false)]"),
            "blanks must be inserted before the attribute block:\n{out}"
        );
    }

    #[test]
    fn blank_lines_default_is_noop() {
        let opts = FormatOptions::default();
        let out = format_al(PROC_NO_GAP, &opts);
        // Preserve: no blank inserted between A's end; and procedure B.
        assert!(
            out.contains("    end;\n    procedure B()"),
            "default must preserve the (absent) gap:\n{out}"
        );
    }

    #[test]
    fn blank_lines_is_idempotent() {
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let pass1 = format_al(PROC_NO_GAP, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "blank_lines must be idempotent");
    }

    // ===================================================================
    // A13 options: max_line_length
    // ===================================================================

    const LONG_PROP: &str = "\
codeunit 50100 T
{
    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;
}
";

    #[test]
    fn max_line_length_wraps_at_top_level_comma() {
        let opts = FormatOptions {
            max_line_length: 60,
            ..Default::default()
        };
        let out = format_al(LONG_PROP, &opts);
        assert!(
            out.contains("= rm,\n        tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;"),
            "long property must wrap at the top-level comma:\n{out}"
        );
    }

    #[test]
    fn max_line_length_does_not_split_inside_string() {
        // The only comma is inside a string literal — leave the line intact
        // even though it exceeds the limit.
        let input = "\
codeunit 50100 T
{
    Caption = 'Hello, World this is a very long caption indeed yes';
}
";
        let opts = FormatOptions {
            max_line_length: 20,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    Caption = 'Hello, World this is a very long caption indeed yes';"),
            "must not split inside a string literal:\n{out}"
        );
    }

    #[test]
    fn max_line_length_default_is_noop() {
        let opts = FormatOptions::default(); // max_line_length = 0
        let out = format_al(LONG_PROP, &opts);
        assert!(
            out.contains(
                "    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;"
            ),
            "max_line_length=0 must leave long lines intact:\n{out}"
        );
    }

    #[test]
    fn max_line_length_is_idempotent() {
        let opts = FormatOptions {
            max_line_length: 60,
            ..Default::default()
        };
        let pass1 = format_al(LONG_PROP, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "max_line_length must be idempotent");
    }

    // ===================================================================
    // A13 options: brace_style
    // ===================================================================

    const BRACE_INPUT: &str = "\
table 50100 Test
{
    fields
    {
        field(1; \"No.\"; Code[20])
        {
        }
    }
}
";

    #[test]
    fn brace_style_same_line_merges_openers() {
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(BRACE_INPUT, &opts);
        assert!(out.contains("table 50100 Test {"), "object brace:\n{out}");
        assert!(out.contains("    fields {"), "sub-block brace:\n{out}");
        assert!(
            out.contains("        field(1; \"No.\"; Code[20]) {"),
            "field brace:\n{out}"
        );
    }

    #[test]
    fn brace_style_default_is_noop() {
        let opts = FormatOptions::default(); // NextLine
        let out = format_al(BRACE_INPUT, &opts);
        assert!(
            out.contains("table 50100 Test\n{"),
            "NextLine default must leave braces on their own line:\n{out}"
        );
    }

    #[test]
    fn brace_style_is_idempotent() {
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(BRACE_INPUT, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "brace_style must be idempotent");
    }

    #[test]
    fn brace_style_leaves_begin_end_untouched() {
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
}
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // `begin` keeps its own line; only the object `{` merged onto header.
        assert!(out.contains("codeunit 50100 T {"), "header merge:\n{out}");
        assert!(out.contains("    begin\n"), "begin untouched:\n{out}");
        assert!(out.contains("    end;\n"), "end; untouched:\n{out}");
    }

    #[test]
    fn brace_style_same_line_does_not_comment_out_brace() {
        // CRITICAL: a `{` after a line that ends in a `//` comment must stay on
        // its own line, otherwise the brace gets commented out -> invalid AL.
        let input = "\
table 50100 Test
{
    fields
    {
        field(1; \"No.\"; Code[20]) // pk
        {
        }
    }
}
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // The brace must NOT be appended after the comment.
        assert!(
            !out.contains("// pk {"),
            "must not comment out the brace:\n{out}"
        );
        // The comment line is intact and the brace remains on its own line.
        assert!(
            out.contains("field(1; \"No.\"; Code[20]) // pk\n"),
            "comment line must be preserved:\n{out}"
        );
        assert!(
            out.contains("// pk\n        {"),
            "brace must remain on its own line:\n{out}"
        );
        // Other (comment-free) braces still merge.
        assert!(out.contains("table 50100 Test {"));
        assert!(out.contains("    fields {"));
        // And the whole thing is still idempotent.
        let pass2 = format_al(&out, &opts);
        assert_eq!(out, pass2, "comment-before-brace case must be idempotent");
    }

    #[test]
    fn a13_all_options_together_are_idempotent() {
        // Combined run exercises pass ordering and cross-pass idempotency.
        let input = "\
table 50100 Test
{
    Zulu = 1;
    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbb = r;
    Alpha = 2;

    fields
    {
        field(1; \"No.\"; Code[20])
        {
        }
    }

    procedure A()
    begin
    end;
    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            sort_properties: true,
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            max_line_length: 60,
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "combined A13 passes must be idempotent");
    }
}
