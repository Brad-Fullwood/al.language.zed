//! Property tests for `format_al` over generated and mutated AL source.
//!
//! Three properties, all at the default `FormatOptions`:
//! - formatting is idempotent
//! - formatting a file that parses without ERROR nodes leaves it parsing without them
//! - formatting preserves the token stream, modulo whitespace
//!
//! Case count follows `PROPTEST_CASES` (default 128).

mod algen;

use al_syntax::formatting::{
    format_al, BlankLinesBetweenProcedures, BraceStyle, FormatOptions, KeywordCasing,
};
use al_syntax::AlParser;
use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        ..ProptestConfig::default()
    }
}

fn parses_clean(text: &str) -> bool {
    AlParser::parse_quick(text).errors.is_empty()
}

/// Leaf tokens of the parse, as `(kind, text with whitespace runs collapsed)`.
///
/// Whitespace is collapsed inside each token because a block comment's own text carries
/// the indentation the formatter is allowed to change.
fn token_stream(text: &str) -> Vec<(String, String)> {
    let result = AlParser::parse_quick(text);
    let source = text.as_bytes();
    let mut out = Vec::new();
    let mut cursor = result.tree.walk();
    let mut recurse = true;
    loop {
        if recurse && cursor.goto_first_child() {
            continue;
        }
        let node = cursor.node();
        if node.child_count() == 0 && !node.is_extra() || node.is_extra() && node.child_count() == 0
        {
            if let Ok(t) = node.utf8_text(source) {
                let norm = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !norm.is_empty() {
                    out.push((node.kind().to_string(), norm));
                }
            }
        }
        if cursor.goto_next_sibling() {
            recurse = true;
        } else if cursor.goto_parent() {
            recurse = false;
        } else {
            break;
        }
    }
    out
}

fn assert_idempotent(src: &str) -> Result<(), TestCaseError> {
    let opts = FormatOptions::default();
    let once = format_al(src, &opts);
    let twice = format_al(&once, &opts);
    prop_assert_eq!(
        &once,
        &twice,
        "format is not idempotent\n--- input ---\n{}\n--- once ---\n{}\n--- twice ---\n{}",
        src,
        once,
        twice
    );
    Ok(())
}

fn assert_keeps_clean(src: &str) -> Result<(), TestCaseError> {
    if !parses_clean(src) {
        return Ok(());
    }
    let out = format_al(src, &FormatOptions::default());
    prop_assert!(
        parses_clean(&out),
        "formatting introduced parse errors\n--- input ---\n{}\n--- output ---\n{}\n--- errors ---\n{:?}",
        src,
        out,
        AlParser::parse_quick(&out).errors
    );
    Ok(())
}

fn assert_preserves_tokens(src: &str) -> Result<(), TestCaseError> {
    if !parses_clean(src) {
        return Ok(());
    }
    let out = format_al(src, &FormatOptions::default());
    prop_assert_eq!(
        token_stream(src),
        token_stream(&out),
        "formatting changed the token stream\n--- input ---\n{}\n--- output ---\n{}",
        src,
        out
    );
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn generated_object_format_is_idempotent(src in algen::al_object()) {
        assert_idempotent(&src)?;
    }

    #[test]
    fn generated_object_format_keeps_parse_clean(src in algen::al_object()) {
        assert_keeps_clean(&src)?;
    }

    #[test]
    fn generated_object_format_preserves_tokens(src in algen::al_object()) {
        assert_preserves_tokens(&src)?;
    }

    #[test]
    fn generated_statement_format_is_idempotent(body in prop::collection::vec(algen::stmt(), 1..5)) {
        let src = format!(
            "codeunit 50100 Gen\n{{\n    procedure P()\n    begin\n{}\n    end;\n}}",
            body.iter()
                .map(|s| s.lines().map(|l| format!("        {l}")).collect::<Vec<_>>().join("\n"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_idempotent(&src)?;
    }

    #[test]
    fn keyword_casing_is_idempotent(src in algen::al_object(), upper in any::<bool>()) {
        let opts = FormatOptions {
            keyword_casing: if upper { KeywordCasing::Upper } else { KeywordCasing::Lower },
            ..FormatOptions::default()
        };
        let once = format_al(&src, &opts);
        let twice = format_al(&once, &opts);
        prop_assert_eq!(&once, &twice, "keyword casing is not idempotent on\n{}", src);
    }

    /// Each advanced pass documents itself as idempotent. Exercise them in combination.
    #[test]
    fn every_option_combination_is_idempotent(src in algen::al_object(), opts in options()) {
        let once = format_al(&src, &opts);
        let twice = format_al(&once, &opts);
        prop_assert_eq!(
            &once,
            &twice,
            "format is not idempotent at {:?}\n--- input ---\n{}\n--- once ---\n{}\n--- twice ---\n{}",
            opts, src, once, twice
        );
    }

    /// Line endings survive: a CRLF file stays CRLF, an LF file stays LF.
    #[test]
    fn line_endings_are_preserved(src in algen::al_object(), crlf in any::<bool>()) {
        let src = if crlf { src.replace('\n', "\r\n") } else { src };
        let out = format_al(&src, &FormatOptions::default());
        prop_assert_eq!(
            out.contains("\r\n"),
            crlf,
            "line ending convention changed\n--- input ---\n{:?}\n--- output ---\n{:?}",
            src, out
        );
        prop_assert!(
            !out.contains('\r') || crlf,
            "stray CR in LF output: {:?}",
            out
        );
    }
}

fn options() -> impl Strategy<Value = FormatOptions> {
    (
        1usize..9,
        any::<bool>(),
        0usize..3,
        0usize..3,
        prop::sample::select(vec![0usize, 40, 80, 120]),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(tab_size, insert_spaces, casing, blanks, max_line_length, same_line, sort_props)| {
                FormatOptions {
                    tab_size,
                    insert_spaces,
                    keyword_casing: match casing {
                        0 => KeywordCasing::Preserve,
                        1 => KeywordCasing::Lower,
                        _ => KeywordCasing::Upper,
                    },
                    blank_lines_between_procedures: match blanks {
                        0 => BlankLinesBetweenProcedures::Preserve,
                        1 => BlankLinesBetweenProcedures::One,
                        _ => BlankLinesBetweenProcedures::Two,
                    },
                    max_line_length,
                    brace_style: if same_line {
                        BraceStyle::SameLine
                    } else {
                        BraceStyle::NextLine
                    },
                    sort_properties: sort_props,
                }
            },
        )
}

/// The mutation half: take each shipped `.al` fixture, perturb its whitespace and line
/// order, and require the same three properties.
mod fixtures {
    use super::*;

    fn fixture_strategy() -> impl Strategy<Value = (String, Vec<algen::Mutation>)> {
        let files = algen::fixture_files();
        assert!(!files.is_empty(), "no .al fixtures found");
        let texts: Vec<String> = files.into_iter().map(|(_, t)| t).collect();
        (
            prop::sample::select(texts),
            prop::collection::vec(algen::mutation(), 0..6),
        )
    }

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn mutated_fixture_format_is_idempotent((text, ops) in fixture_strategy()) {
            assert_idempotent(&algen::mutate(&text, &ops))?;
        }

        #[test]
        fn mutated_fixture_format_keeps_parse_clean((text, ops) in fixture_strategy()) {
            assert_keeps_clean(&algen::mutate(&text, &ops))?;
        }

        #[test]
        fn mutated_fixture_format_preserves_tokens((text, ops) in fixture_strategy()) {
            assert_preserves_tokens(&algen::mutate(&text, &ops))?;
        }

        #[test]
        fn mutated_fixture_every_option_is_idempotent(
            (text, ops) in fixture_strategy(),
            opts in options(),
        ) {
            let src = algen::mutate(&text, &ops);
            let once = format_al(&src, &opts);
            let twice = format_al(&once, &opts);
            prop_assert_eq!(
                &once,
                &twice,
                "format is not idempotent at {:?}\n--- input ---\n{}\n--- once ---\n{}\n--- twice ---\n{}",
                opts, src, once, twice
            );
        }
    }

    #[test]
    fn every_fixture_formats_idempotently() {
        for (path, text) in algen::fixture_files() {
            let opts = FormatOptions::default();
            let once = format_al(&text, &opts);
            let twice = format_al(&once, &opts);
            assert_eq!(once, twice, "format is not idempotent for {path}");
        }
    }
}
