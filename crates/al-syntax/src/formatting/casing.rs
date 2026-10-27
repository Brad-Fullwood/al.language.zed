//! Keyword casing: fold AL keyword tokens to the configured case while leaving
//! identifiers, type names, literals and comments byte-for-byte.

use super::options::KeywordCasing;

/// Apply `KeywordCasing` to a formatted file.
///
/// The text is parsed with tree-sitter and only leaf nodes whose kind starts
/// with `kw_` are folded, so the grammar decides what a keyword is in each
/// position. Built-in type keywords (`Code`, `Date`, `Integer`, `Record`) are
/// left as written, as are names that happen to spell a keyword (`Rec.Code`,
/// `Text: Text`). Strings, comments and inactive code are single other nodes
/// and are never touched. Standard route checked: the shared
/// `language_data::keywords()` list matches by text, which is what folded
/// identifiers before, so the node kind is used instead.
///
/// Returns the input unchanged when casing is `Preserve`.
pub(super) fn apply_keyword_casing(text: String, casing: &KeywordCasing) -> String {
    if matches!(casing, KeywordCasing::Preserve) {
        return text;
    }
    let parsed = crate::parser::AlParser::parse_quick(&text);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut cursor = parsed.tree.walk();
    'walk: loop {
        let node = cursor.node();
        if node.child_count() == 0 && is_casing_keyword(&node) {
            ranges.push((node.start_byte(), node.end_byte()));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in ranges {
        // Only plain ASCII words are folded, which keeps byte offsets valid.
        let Some(word) = text.get(start..end).filter(|w| w.is_ascii()) else {
            continue;
        };
        out.push_str(&text[last..start]);
        match casing {
            KeywordCasing::Lower => out.push_str(&word.to_ascii_lowercase()),
            KeywordCasing::Upper => out.push_str(&word.to_ascii_uppercase()),
            KeywordCasing::Preserve => out.push_str(word),
        }
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// Keyword tokens are `kw_*` leaves, plus the `metadata_keyword` leaves of
/// object sections (`fields`, `field`, `layout`, `area`, `actions`). A built-in
/// type keyword inside a type reference (`Code[20]`, `Record Customer`) is a
/// type name and stays as written. The same word as an object keyword
/// (`table`, `page`) is folded.
fn is_casing_keyword(node: &tree_sitter::Node) -> bool {
    if node.is_missing() {
        return false;
    }
    let parent = node.parent().map_or("", |p| p.kind());
    match node.kind() {
        "metadata_keyword" => parent == "object_section",
        kind if kind.starts_with("kw_") => {
            !(crate::language_data::is_type_keyword_node(kind)
                && matches!(parent, "type_reference" | "name_or_keyword"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::formatting::{format_al, FormatOptions, KeywordCasing};

    fn cased(input: &str, casing: KeywordCasing) -> String {
        format_al(
            input,
            &FormatOptions {
                keyword_casing: casing,
                ..Default::default()
            },
        )
    }

    fn upper_cased(input: &str) -> String {
        cased(input, KeywordCasing::Upper)
    }

    const FINDING: &str = "\
table 50100 T
{
    fields
    {
        field(1; Code; Code[20])
        {
        }
    }
    keys
    {
        key(PK; Code)
        {
        }
    }
    procedure P(Text: Text; Date: Date)
    var
        Integer: Integer;
        Cust: Record Customer temporary;
        Lines: array[10] of Code[20];
    begin
        Rec.Code := Rec.Date;
        if Rec.Code = 'if' then // if
            exit;
        Integer := 1;
    end;
}
";

    #[test]
    fn upper_folds_keywords_and_leaves_names_and_types() {
        let out = upper_cased(FINDING);
        for kept in [
            "FIELD(1; Code; Code[20])",
            "Rec.Code := Rec.Date;",
            "(Text: Text; Date: Date)",
            "Integer: Integer;",
            "Cust: Record Customer",
            "Lines: array[10] OF Code[20];",
            "Integer := 1;",
            "'if' THEN // if",
        ] {
            assert!(out.contains(kept), "missing {kept:?} in:\n{out}");
        }
        for folded in [
            "TABLE 50100 T",
            "FIELDS",
            "KEYS",
            "KEY(PK; Code)",
            "PROCEDURE P",
            "VAR",
            "BEGIN",
            "END;",
            "EXIT;",
        ] {
            assert!(out.contains(folded), "missing {folded:?} in:\n{out}");
        }
        assert!(out.contains("TEMPORARY"), "got:\n{out}");
    }

    #[test]
    fn lower_folds_upper_case_keywords_only() {
        let input = FINDING
            .replace("table", "TABLE")
            .replace("fields", "FIELDS")
            .replace("begin", "BEGIN")
            .replace("field(", "FIELD(");
        let out = cased(&input, KeywordCasing::Lower);
        assert!(out.contains("table 50100 T"), "got:\n{out}");
        assert!(out.contains("    fields\n"), "got:\n{out}");
        assert!(out.contains("field(1; Code; Code[20])"), "got:\n{out}");
        assert!(out.contains("    begin\n"), "got:\n{out}");
    }

    #[test]
    fn casing_is_idempotent_and_preserve_is_identity() {
        let once = upper_cased(FINDING);
        assert_eq!(upper_cased(&once), once);
        assert_eq!(cased(FINDING, KeywordCasing::Preserve), FINDING);
    }

    #[test]
    fn casing_keeps_non_ascii_content() {
        let input = "codeunit 50100 T\n{\n    procedure P()\n    begin\n        Message('Grüße: €');\n        if \"Preis in €\" = 1 then;\n    end;\n}\n";
        let out = upper_cased(input);
        assert!(out.contains("'Grüße: €'"), "got:\n{out}");
        assert!(out.contains("\"Preis in €\""), "got:\n{out}");
        assert!(out.contains("CODEUNIT 50100 T"), "got:\n{out}");
    }

    #[test]
    fn casing_leaves_comments_alone() {
        let input = "codeunit 50100 T\n{\n    /* begin end\n       var if then */\n    procedure P()\n    begin\n        Foo(); /* begin end\n        var */\n    end;\n}\n";
        let out = upper_cased(input);
        assert!(
            out.contains("/* begin end\n       var if then */"),
            "got:\n{out}"
        );
        assert!(out.contains("/* begin end\n        var */"), "got:\n{out}");
    }
}
