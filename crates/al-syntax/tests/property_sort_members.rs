//! Property tests for `sort_members` over generated and mutated AL source.
//!
//! `sort_members` sits behind an editor command, so running it twice must not
//! move anything the second time. It is also a pure reordering: the output
//! holds exactly the input's lines.
//!
//! Case count follows `PROPTEST_CASES` (default 128).

mod algen;

use al_syntax::sort_members;
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

/// `sort_members` returns `None` when it declines to touch the file, which is
/// its fixed point too.
fn sorted_or_same(text: &str) -> String {
    sort_members(text).unwrap_or_else(|| text.to_string())
}

fn assert_idempotent(src: &str) -> Result<(), TestCaseError> {
    let Some(once) = sort_members(src) else {
        return Ok(());
    };
    let twice = sorted_or_same(&once);
    prop_assert_eq!(
        &once,
        &twice,
        "sort_members is not idempotent\n--- input ---\n{}\n--- once ---\n{}\n--- twice ---\n{}",
        src,
        once,
        twice
    );
    Ok(())
}

fn assert_pure_reordering(src: &str) -> Result<(), TestCaseError> {
    let Some(out) = sort_members(src) else {
        return Ok(());
    };
    let mut before: Vec<&str> = src.lines().collect();
    let mut after: Vec<&str> = out.lines().collect();
    before.sort_unstable();
    after.sort_unstable();
    prop_assert_eq!(
        before,
        after,
        "sort_members changed the line multiset\n--- input ---\n{}\n--- output ---\n{}",
        src,
        out
    );
    Ok(())
}

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
    fn generated_object_sort_is_idempotent(src in algen::al_object()) {
        assert_idempotent(&src)?;
    }

    #[test]
    fn generated_object_sort_is_a_pure_reordering(src in algen::al_object()) {
        assert_pure_reordering(&src)?;
    }

    #[test]
    fn mutated_fixture_sort_is_idempotent((text, ops) in fixture_strategy()) {
        assert_idempotent(&algen::mutate(&text, &ops))?;
    }

    #[test]
    fn mutated_fixture_sort_is_a_pure_reordering((text, ops) in fixture_strategy()) {
        assert_pure_reordering(&algen::mutate(&text, &ops))?;
    }
}
