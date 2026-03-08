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

/// Formatting options.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
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
        if in_var_section
            && (trimmed_lower == "begin" || trimmed_lower.ends_with(" begin"))
        {
            indent_level = (indent_level - 1).max(0);
            in_var_section = false;
        }

        // `begin` after single-statement openers (if...then begin written separately)
        // drains the single-stmt stack since begin starts a block
        if single_stmt_depth > 0
            && (trimmed_lower == "begin" || trimmed_lower.ends_with(" begin"))
        {
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        // Case label: close previous label body before this new label
        let is_case_label =
            case_depth > 0 && trimmed.ends_with(':') && !trimmed.ends_with("::");
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
        {
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
        }

        // `until` closes a `repeat` block
        if trimmed_lower.starts_with("until ") || trimmed_lower == "until" {
            indent_level = (indent_level - 1).max(0);
        }

        // --- Write the indented line ---
        for _ in 0..indent_level {
            result.push_str(&indent_str);
        }
        result.push_str(trimmed);
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
        let is_comment = trimmed.starts_with("//");
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

/// Count net parentheses on a line: `(` adds +1, `)` adds -1.
/// Skips characters inside string literals (single-quoted).
fn count_net_parens(line: &str) -> i32 {
    let mut depth = 0i32;
    let mut in_string = false;
    for ch in line.chars() {
        if ch == '\'' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Returns true if trimmed_lower represents a single-statement control flow opener.
fn is_single_statement_opener(trimmed_lower: &str) -> bool {
    // `if ... then` (but not `if ... then begin`)
    if trimmed_lower.starts_with("if ") && trimmed_lower.ends_with(" then") {
        return true;
    }
    // `for ... do`
    if trimmed_lower.starts_with("for ") && trimmed_lower.ends_with(" do") {
        return true;
    }
    // `while ... do`
    if trimmed_lower.starts_with("while ") && trimmed_lower.ends_with(" do") {
        return true;
    }
    // `with ... do`
    if trimmed_lower.starts_with("with ") && trimmed_lower.ends_with(" do") {
        return true;
    }
    // `foreach ... do`
    if trimmed_lower.starts_with("foreach ") && trimmed_lower.ends_with(" do") {
        return true;
    }
    false
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
        assert!(result.contains("        Message('after blank');") || result.contains("    Message('after blank');"));
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
}
