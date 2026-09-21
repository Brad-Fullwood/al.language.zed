//! Keyword casing: fold AL keywords to the configured case while leaving
//! identifiers, literals and comments byte-for-byte.

use super::options::KeywordCasing;

/// Apply `KeywordCasing` to a single source line. Walks the line token-by-
/// token, transforming runs of ASCII alphabetic characters (the only legal
/// AL identifier/keyword shape) when they match a known AL keyword in
/// `language_data::keywords()`. Identifiers, string literals, comments, and
/// numeric literals pass through unchanged.
///
/// Returns the input string when casing is `Preserve` (no allocation).
pub(super) fn apply_keyword_casing<'a>(
    line: &'a str,
    casing: &KeywordCasing,
) -> std::borrow::Cow<'a, str> {
    apply_keyword_casing_with_state(line, casing, false)
}

pub(super) fn apply_keyword_casing_with_state<'a>(
    line: &'a str,
    casing: &KeywordCasing,
    in_block_comment: bool,
) -> std::borrow::Cow<'a, str> {
    if matches!(casing, KeywordCasing::Preserve) {
        return std::borrow::Cow::Borrowed(line);
    }

    let mut out = String::with_capacity(line.len());
    for span in crate::lexical::LineScanner::new(line, in_block_comment) {
        if span.kind != crate::lexical::SpanKind::Code {
            out.push_str(span.text);
            continue;
        }

        let bytes = span.text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_alphabetic() || b == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &span.text[start..i];
                if crate::language_data::is_keyword(word) {
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
            let ch = span.text[i..].chars().next().unwrap_or(b as char);
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::apply_keyword_casing;
    use crate::formatting::{format_al, FormatOptions, KeywordCasing};

    fn upper_cased(input: &str) -> String {
        format_al(
            input,
            &FormatOptions {
                keyword_casing: KeywordCasing::Upper,
                ..Default::default()
            },
        )
    }

    #[test]
    fn keyword_casing_leaves_block_comment_prose_alone() {
        let input = "\
codeunit 50100 T
{
    /* begin end
       var if then */
}
";
        let out = upper_cased(input);
        assert!(out.contains("/* begin end"), "got:\n{out}");
        assert!(out.contains("var if then */"), "got:\n{out}");
    }

    #[test]
    fn keyword_casing_leaves_mid_line_block_comments_alone() {
        let input = "\
codeunit 50100 T
{
    procedure P()
    begin
        Foo(); /* begin end
        var */
    end;
}
";
        let out = upper_cased(input);
        assert!(out.contains("/* begin end"), "got:\n{out}");
        assert!(out.contains("var */"), "got:\n{out}");
    }

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
    fn keyword_casing_preserves_non_ascii_content() {
        let out = apply_keyword_casing("if X then Message('Grüße: €');", &KeywordCasing::Upper);
        assert!(
            out.contains("'Grüße: €'"),
            "non-ASCII string content corrupted: {out}"
        );
        assert!(
            out.starts_with("IF ") && out.contains(" THEN "),
            "keywords were not upper-cased: {out}"
        );
        let id = apply_keyword_casing("field(1; \"Preis in €\"; Decimal)", &KeywordCasing::Lower);
        assert!(
            id.contains("\"Preis in €\""),
            "quoted identifier corrupted: {id}"
        );
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

    // multi-line paren continuation idempotency
}
