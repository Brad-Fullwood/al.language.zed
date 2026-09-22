//! Fuzz properties for the `.app` package reader.
//!
//! A `.app` is untrusted input: it arrives from a NuGet feed, a BC server or a file the
//! user pointed at. The reader must answer with a package or an error for any byte
//! string, and must never panic, hang or allocate without bound.
//!
//! Three sources of input: arbitrary bytes, bytes shaped like the real header, and
//! single-byte mutations of the `representative.app` benchmark fixture.
//!
//! Case count follows `PROPTEST_CASES` (default 128).

use al_symbols::app_reader::read_app_bytes;
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

/// The benchmark fixture, if it is checked out. Returns `None` rather than failing so a
/// sparse checkout still runs the rest of the file.
fn fixture() -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .join("al-lsp/benches/fixtures/representative.app");
    std::fs::read(path).ok()
}

proptest! {
    #![proptest_config(config())]

    /// Arbitrary bytes produce a package or an error, never a panic.
    #[test]
    fn arbitrary_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = read_app_bytes(&data);
    }

    /// The same, for input that already carries the NAVX magic, so the reader gets past
    /// the first check and into the header and ZIP scan.
    #[test]
    fn navx_shaped_bytes_never_panic(
        header in prop::collection::vec(any::<u8>(), 0..64),
        body in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut data = b"NAVX".to_vec();
        data.extend_from_slice(&header);
        data.extend_from_slice(&body);
        let _ = read_app_bytes(&data);
    }

    /// Input that carries both the NAVX magic and a ZIP local-file-header signature, so
    /// the reader reaches the archive parser.
    #[test]
    fn navx_plus_zip_signature_never_panics(
        gap in prop::collection::vec(any::<u8>(), 0..40),
        body in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut data = b"NAVX".to_vec();
        data.extend_from_slice(&gap);
        data.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        data.extend_from_slice(&body);
        let _ = read_app_bytes(&data);
    }

    /// A truncated real package is an error, never a panic.
    #[test]
    fn truncated_fixture_never_panics(cut in 0usize..100_000) {
        let Some(data) = fixture() else { return Ok(()); };
        let end = cut.min(data.len());
        let _ = read_app_bytes(&data[..end]);
    }

    /// A real package with bytes flipped is an error or a package, never a panic. Byte
    /// positions are taken modulo the file length so the strategy does not depend on it.
    #[test]
    fn mutated_fixture_never_panics(
        edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..8),
    ) {
        let Some(mut data) = fixture() else { return Ok(()); };
        if data.is_empty() {
            return Ok(());
        }
        for (pos, byte) in edits {
            let i = pos % data.len();
            data[i] = byte;
        }
        let _ = read_app_bytes(&data);
    }

    /// Reading is deterministic and free of interior mutation: the same bytes give the
    /// same answer, and the input is not modified.
    #[test]
    fn reading_is_deterministic(data in prop::collection::vec(any::<u8>(), 0..2048)) {
        let first = read_app_bytes(&data).map(|p| (p.app_id, p.name, p.object_count));
        let second = read_app_bytes(&data).map(|p| (p.app_id, p.name, p.object_count));
        prop_assert_eq!(first.is_ok(), second.is_ok());
        if let (Ok(a), Ok(b)) = (first, second) {
            prop_assert_eq!(a, b);
        }
    }
}

/// The unmutated fixture must still read cleanly, so a mutation test that always errors
/// cannot pass silently.
#[test]
fn the_fixture_itself_reads() {
    let Some(data) = fixture() else {
        eprintln!("representative.app is not checked out; skipping");
        return;
    };
    let pkg = read_app_bytes(&data).expect("the benchmark fixture must read");
    assert!(
        pkg.object_count > 0,
        "the fixture should carry objects, got {}",
        pkg.object_count
    );
}
