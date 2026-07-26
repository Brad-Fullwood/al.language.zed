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
use al_source::documents::{DocumentMutationError, DocumentStore, TextChange, TextRange};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefRangeOutcome {
    Valid,
    OutOfBounds,
    Backward,
}

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

fn ref_range_outcome(s: &str, c: &TextChange) -> RefRangeOutcome {
    let Some(range) = c.range else {
        return RefRangeOutcome::Valid;
    };
    match (
        ref_offset(s, range.start_line, range.start_character),
        ref_offset(s, range.end_line, range.end_character),
    ) {
        (Some(start), Some(end)) if start <= end => RefRangeOutcome::Valid,
        (Some(_), Some(_)) => RefRangeOutcome::Backward,
        _ => RefRangeOutcome::OutOfBounds,
    }
}

fn assert_apply_outcome(
    outcome: RefRangeOutcome,
    result: Result<(), DocumentMutationError>,
    context: &str,
) {
    match (outcome, result) {
        (RefRangeOutcome::Valid, Ok(()))
        | (RefRangeOutcome::OutOfBounds, Err(DocumentMutationError::RangeOutOfBounds { .. }))
        | (RefRangeOutcome::Backward, Err(DocumentMutationError::BackwardRange { .. })) => {}
        (expected, actual) => {
            panic!("{context}: expected {expected:?}, got {actual:?}");
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
        store
            .open(uri.clone(), text.clone())
            .expect("random starting document should fit in the store");
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
            let expected_outcome = ref_range_outcome(&before, &change);
            expected = ref_apply(&expected, &change);
            let before_version = store.get_version(&uri).expect("document version present");
            let result = store.apply_changes(&uri, std::slice::from_ref(&change));
            assert_apply_outcome(
                expected_outcome,
                result,
                &format!("case {case} range {:?}", change.range),
            );
            let after_version = store.get_version(&uri).expect("document version present");
            match expected_outcome {
                RefRangeOutcome::Valid => assert_eq!(
                    after_version,
                    before_version + 1,
                    "case {case}: a successful edit must advance the document version"
                ),
                RefRangeOutcome::OutOfBounds | RefRangeOutcome::Backward => assert_eq!(
                    after_version, before_version,
                    "case {case}: a rejected edit must preserve the document version"
                ),
            }
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
    store
        .open(uri.clone(), "héllo\nwörld🎉\n".to_string())
        .expect("adversarial fixture should fit in the store");
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
        let before = store.get_text(&uri).expect("document present").to_string();
        let change = TextChange {
            range: Some(TextRange {
                start_line: sl,
                start_character: sc,
                end_line: el,
                end_character: ec,
            }),
            text: "X".into(),
        };
        let expected_outcome = ref_range_outcome(&before, &change);
        let before_version = store.get_version(&uri).expect("document version present");
        let result = store.apply_changes(&uri, std::slice::from_ref(&change));
        assert_apply_outcome(
            expected_outcome,
            result,
            &format!("range ({sl},{sc})..({el},{ec})"),
        );
        let after_version = store.get_version(&uri).expect("document version present");
        match expected_outcome {
            RefRangeOutcome::Valid => assert_eq!(
                after_version,
                before_version + 1,
                "successful adversarial edit must advance the document version"
            ),
            RefRangeOutcome::OutOfBounds | RefRangeOutcome::Backward => assert_eq!(
                after_version, before_version,
                "rejected adversarial edit must preserve the document version"
            ),
        }
        let after = store.get_text(&uri).expect("document vanished").to_string();

        // The contract is not merely "survives": an out-of-bounds or reversed
        // range must be skipped with the buffer untouched, and an in-bounds one
        // must splice exactly like the reference. `is_some()` alone would pass
        // on an emptied or corrupted buffer.
        let expected = ref_apply(&before, &change);
        assert_eq!(
            after, expected,
            "range ({sl},{sc})..({el},{ec}) diverged\n  before:   {before:?}\n  expected: {expected:?}\n  got:      {after:?}"
        );
        assert!(
            std::str::from_utf8(after.as_bytes()).is_ok(),
            "document is no longer valid UTF-8"
        );
    }
}
