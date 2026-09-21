//! Property tests for LSP position handling in the document store.
//!
//! `DocumentStore::apply_changes` converts an LSP `(line, character)` pair to a rope
//! offset. LSP fixes both halves of that conversion: a line ends at `\n`, `\r\n` or a
//! lone `\r`, and `character` counts UTF-16 code units clamped to the line's length. The
//! reference implementation here follows the specification literally; the properties
//! require the store to agree with it over text containing multi-byte and astral
//! characters, CRLF, lone CR and the Unicode separators that are *not* LSP line breaks.
//!
//! Case count follows `PROPTEST_CASES` (default 128).

use al_source::documents::{DocumentStore, TextChange, TextRange};
use proptest::prelude::*;
use url::Url;

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

/// Byte offsets of the start of each LSP line: the whole text is one line plus one more
/// after every `\n`, `\r\n` or lone `\r`.
fn lsp_line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0usize];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                starts.push(i + 1);
                i += 1;
            }
            b'\r' => {
                let skip = if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                starts.push(i + skip);
                i += skip;
            }
            _ => i += 1,
        }
    }
    starts
}

/// LSP `(line, character)` to a byte offset, per the specification: out-of-range line is
/// rejected, out-of-range character clamps to the end of the line's content.
fn reference_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let starts = lsp_line_starts(text);
    let start = *starts.get(line as usize)?;
    let end = starts
        .get(line as usize + 1)
        .map(|next| {
            // Exclude the terminator that opened the next line.
            let mut e = *next;
            if text.as_bytes().get(e.wrapping_sub(1)) == Some(&b'\n') {
                e -= 1;
            }
            if text.as_bytes().get(e.wrapping_sub(1)) == Some(&b'\r') {
                e -= 1;
            }
            e
        })
        .unwrap_or(text.len());
    let content = &text[start..end];
    let mut remaining = character as usize;
    for (idx, ch) in content.char_indices() {
        if remaining == 0 {
            return Some(start + idx);
        }
        if remaining < ch.len_utf16() {
            // The position falls inside a surrogate pair. Clamp to the character start,
            // which is what every LSP server that cannot split a code point does.
            return Some(start + idx);
        }
        remaining -= ch.len_utf16();
    }
    Some(end)
}

/// Text that stresses the conversion: ASCII, 2- and 3-byte characters, astral characters,
/// every line-terminator form, and the Unicode separators LSP does not treat as line
/// breaks.
fn tricky_text() -> impl Strategy<Value = String> {
    let piece = prop::sample::select(vec![
        "a", "bc", "  ", "é", "日", "😀", "𝄞", "\n", "\r\n", "\r", "\u{0b}", "\u{0c}", "\u{85}",
        "\u{2028}", "\u{2029}", "'x'", "//c", "\t",
    ]);
    prop::collection::vec(piece, 0..24).prop_map(|v| v.concat())
}

fn uri(n: usize) -> Url {
    Url::parse(&format!("file:///prop/{n}.al")).unwrap()
}

/// Apply `replace [start, end) with marker` through the store and compute the same
/// edit through the reference conversion. They must produce the same document.
fn check_edit(
    text: &str,
    start: (u32, u32),
    end: (u32, u32),
    marker: &str,
) -> Result<(), TestCaseError> {
    let store = DocumentStore::new();
    let u = uri(0);
    store.open(u.clone(), text.to_string()).unwrap();
    let change = TextChange {
        range: Some(TextRange {
            start_line: start.0,
            start_character: start.1,
            end_line: end.0,
            end_character: end.1,
        }),
        text: marker.to_string(),
    };
    let applied = store.apply_changes(&u, std::slice::from_ref(&change));

    let (rs, re) = match (
        reference_offset(text, start.0, start.1),
        reference_offset(text, end.0, end.1),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            prop_assert!(
                applied.is_err(),
                "store accepted a position the specification rejects: {:?} in {:?}",
                (start, end),
                text
            );
            return Ok(());
        }
    };
    if rs > re {
        prop_assert!(
            applied.is_err(),
            "store accepted a backward range {:?} in {:?}",
            (start, end),
            text
        );
        return Ok(());
    }
    prop_assert!(
        applied.is_ok(),
        "store rejected a valid range {:?} in {:?}: {:?}",
        (start, end),
        text,
        applied.err()
    );
    let expected = format!("{}{marker}{}", &text[..rs], &text[re..]);
    prop_assert_eq!(
        store.get_text(&u).unwrap(),
        expected,
        "edit at {:?}..{:?} of {:?} disagreed with the LSP position reference",
        start,
        end,
        text
    );
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// A zero-width edit inserting nothing is the identity, whatever position it names.
    #[test]
    fn empty_edit_at_any_position_is_the_identity(
        text in tricky_text(),
        line in 0u32..8,
        character in 0u32..12,
    ) {
        let store = DocumentStore::new();
        let u = uri(1);
        store.open(u.clone(), text.clone()).unwrap();
        let change = TextChange {
            range: Some(TextRange {
                start_line: line,
                start_character: character,
                end_line: line,
                end_character: character,
            }),
            text: String::new(),
        };
        if store.apply_changes(&u, &[change]).is_ok() {
            prop_assert_eq!(store.get_text(&u).unwrap(), text);
        }
    }

    /// The store's position conversion matches the LSP specification.
    #[test]
    fn edits_agree_with_the_lsp_position_reference(
        text in tricky_text(),
        sl in 0u32..8,
        sc in 0u32..12,
        el in 0u32..8,
        ec in 0u32..12,
    ) {
        check_edit(&text, (sl, sc), (el, ec), "@")?;
    }

    /// Replacing a range with the text it covers leaves the document unchanged, so
    /// offset -> text -> offset round trips.
    #[test]
    fn replacing_a_range_with_its_own_text_round_trips(
        text in tricky_text(),
        sl in 0u32..8,
        sc in 0u32..12,
        el in 0u32..8,
        ec in 0u32..12,
    ) {
        let (Some(rs), Some(re)) = (
            reference_offset(&text, sl, sc),
            reference_offset(&text, el, ec),
        ) else {
            return Ok(());
        };
        if rs > re {
            return Ok(());
        }
        let covered = text[rs..re].to_string();
        let store = DocumentStore::new();
        let u = uri(2);
        store.open(u.clone(), text.clone()).unwrap();
        let change = TextChange {
            range: Some(TextRange {
                start_line: sl,
                start_character: sc,
                end_line: el,
                end_character: ec,
            }),
            text: covered,
        };
        if store.apply_changes(&u, &[change]).is_ok() {
            prop_assert_eq!(store.get_text(&u).unwrap(), text);
        }
    }
}

/// Byte offset -> UTF-16 column -> byte offset, over the same alphabet. These two are
/// the conversion al-lsp uses at the tree-sitter boundary.
mod utf16_columns {
    use super::*;
    use al_syntax::{byte_col_to_utf16_col, utf16_col_to_byte_offset};

    fn line() -> impl Strategy<Value = String> {
        let piece = prop::sample::select(vec![
            "a", "bc", "  ", "é", "日", "😀", "𝄞", "\u{85}", "\u{2028}", "\t",
        ]);
        prop::collection::vec(piece, 0..20).prop_map(|v| v.concat())
    }

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn byte_offset_round_trips_through_utf16(line in line()) {
            for (byte_idx, _) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
                let utf16 = byte_col_to_utf16_col(&line, byte_idx);
                let back = utf16_col_to_byte_offset(&line, utf16 as usize);
                prop_assert_eq!(
                    back, byte_idx,
                    "byte {} -> utf16 {} -> byte {} in {:?}",
                    byte_idx, utf16, back, line
                );
            }
        }

        #[test]
        fn utf16_column_round_trips_through_bytes(line in line()) {
            let total: usize = line.chars().map(char::len_utf16).sum();
            let mut utf16 = 0usize;
            for ch in line.chars() {
                let byte = utf16_col_to_byte_offset(&line, utf16);
                prop_assert_eq!(
                    byte_col_to_utf16_col(&line, byte) as usize,
                    utf16,
                    "utf16 {} -> byte {} -> utf16 in {:?}",
                    utf16, byte, line
                );
                utf16 += ch.len_utf16();
            }
            prop_assert_eq!(utf16, total);
            prop_assert_eq!(utf16_col_to_byte_offset(&line, total), line.len());
        }

        /// A column past the end of the line clamps to the end, it never panics or
        /// returns an offset outside the string.
        #[test]
        fn out_of_range_columns_clamp(line in line(), over in 0usize..4096) {
            let total: usize = line.chars().map(char::len_utf16).sum();
            prop_assert_eq!(utf16_col_to_byte_offset(&line, total + over), line.len());
            prop_assert!(byte_col_to_utf16_col(&line, line.len() + over) as usize <= total);
        }
    }
}
