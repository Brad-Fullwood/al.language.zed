//! Randomised differential test for incremental LSP edits.
//!
//! `DocumentStore` applies `textDocument/didChange` ranges against a ropey
//! `Rope`, converting LSP UTF-16 positions to rope offsets. That conversion is
//! easy to get subtly wrong on non-ASCII text, CRLF line breaks, and
//! out-of-bounds ranges — and wrong means a silently corrupted buffer, which
//! then poisons every parse, diagnostic and completion for that file.
//!
//! Rather than enumerate cases by hand, this replays randomised edit sequences
//! against a straightforward `String`-splicing reference and asserts the two
//! agree exactly. The generated alphabet deliberately includes multi-byte
//! characters (`é`), a 3-byte CJK character (`日`), an astral character whose
//! UTF-16 form is a surrogate pair (`🎉`), and `\r\n`.
use al_source::documents::{DocumentStore, TextChange, TextRange};
use url::Url;

/// Reference implementation: convert an LSP (line, utf16-char) position to a
/// byte offset in `s`, using the spec's clamping rules.
fn ref_offset(s: &str, line: u32, character: u32) -> Option<usize> {
    // Split keeping terminators, matching ropey's line model.
    let mut starts = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    let line = line as usize;
    if line >= starts.len() {
        return None;
    }
    let start = starts[line];
    let end = starts.get(line + 1).copied().unwrap_or(s.len());
    let line_str = &s[start..end];
    // LSP line content excludes the trailing break.
    let content = line_str
        .strip_suffix('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .unwrap_or(line_str);
    // Round DOWN to a char boundary when `character` lands inside a surrogate
    // pair — never split a code point. (This is what ropey's utf16_cu_to_char
    // does, and what VS Code sends/expects.)
    let mut u16s = 0u32;
    for (byte, ch) in content.char_indices() {
        let next = u16s + ch.len_utf16() as u32;
        if next > character {
            return Some(start + byte);
        }
        u16s = next;
    }
    Some(start + content.len())
}

fn ref_apply(s: &str, c: &TextChange) -> String {
    match c.range {
        None => c.text.clone(),
        Some(r) => {
            let (a, b) = (
                ref_offset(s, r.start_line, r.start_character),
                ref_offset(s, r.end_line, r.end_character),
            );
            match (a, b) {
                (Some(a), Some(b)) if a <= b => {
                    let mut out = String::with_capacity(s.len() + c.text.len());
                    out.push_str(&s[..a]);
                    out.push_str(&c.text);
                    out.push_str(&s[b..]);
                    out
                }
                _ => s.to_string(),
            }
        }
    }
}

/// Deterministic xorshift so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

const ALPHABET: &[&str] = &["a", "b", "\n", " ", "é", "日", "🎉", "\r\n", "{", "}"];

#[test]
fn incremental_edits_match_reference_implementation() {
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let mut mismatches = 0usize;

    for case in 0..400u32 {
        let store = DocumentStore::new();
        let uri = Url::parse(&format!("file:///t/{case}.al")).unwrap();

        // Random starting document.
        let mut text = String::new();
        for _ in 0..rng.below(40) {
            text.push_str(ALPHABET[rng.below(ALPHABET.len() as u64) as usize]);
        }
        store.open(uri.clone(), text.clone());
        let mut expected = text.clone();

        for _step in 0..12 {
            // Random range, biased toward in-bounds.
            let lines = expected.lines().count().max(1) as u64;
            let sl = rng.below(lines + 1) as u32;
            let sc = rng.below(8) as u32;
            let el = (sl as u64 + rng.below(3)) as u32;
            let ec = rng.below(8) as u32;
            let mut ins = String::new();
            for _ in 0..rng.below(5) {
                ins.push_str(ALPHABET[rng.below(ALPHABET.len() as u64) as usize]);
            }
            let change = TextChange {
                range: Some(TextRange {
                    start_line: sl,
                    start_character: sc,
                    end_line: el,
                    end_character: ec,
                }),
                text: ins,
            };

            let before = expected.clone();
            expected = ref_apply(&expected, &change);
            store.apply_changes(&uri, std::slice::from_ref(&change));
            let got = store.get_text(&uri).unwrap();

            if got != expected {
                mismatches += 1;
                if mismatches <= 3 {
                    eprintln!(
                        "case {case} MISMATCH\n  before:   {before:?}\n  change:   {:?} {:?}\n  expected: {expected:?}\n  got:      {got:?}",
                        change.range, change.text
                    );
                }
                // Resync so one divergence doesn't cascade.
                expected = got;
            }
        }
    }
    assert_eq!(mismatches, 0, "incremental edits diverged from reference");
}

/// The store must never panic, and must never produce invalid UTF-8 /
/// lose the document, on adversarial ranges.
#[test]
fn adversarial_ranges_do_not_panic() {
    let store = DocumentStore::new();
    let uri = Url::parse("file:///t/adv.al").unwrap();
    store.open(uri.clone(), "héllo\nwörld🎉\n".to_string());
    let ranges = [
        (0, 0, 0, 0),
        (0, 0, u32::MAX, u32::MAX),
        (u32::MAX, u32::MAX, 0, 0),
        (5, 0, 1, 0),
        (0, 3, 0, 1),
        (1, u32::MAX, 1, u32::MAX),
        (0, 1, 0, 2), // mid-multibyte in UTF-16 terms
        (1, 6, 1, 7), // inside the surrogate pair of 🎉
    ];
    for (sl, sc, el, ec) in ranges {
        store.apply_changes(
            &uri,
            &[TextChange {
                range: Some(TextRange {
                    start_line: sl,
                    start_character: sc,
                    end_line: el,
                    end_character: ec,
                }),
                text: "X".into(),
            }],
        );
        assert!(store.get_text(&uri).is_some(), "document vanished");
    }
}
