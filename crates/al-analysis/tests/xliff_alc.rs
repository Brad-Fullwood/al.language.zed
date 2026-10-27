//! The units `xlf generate` extracts against the `.g.xlf` that alc 18.1 wrote
//! for the same sources.
//!
//! `tests/fixtures/xliff_alc/src` is a synthetic project compiled with
//! `alc` 18.1 and `features: ["TranslationFile"]` (expected-translationfile.g.xlf) and with
//! `["TranslationFile", "GenerateCaptions"]` (expected-generatecaptions.g.xlf).
//! The ids and the `al-object-target` of every unit must match exactly.

use std::collections::BTreeMap;
use std::path::PathBuf;

use al_analysis::xliff::{extract_translation_units_with, parse_xliff, XliffOptions};
use al_workspace::Workspace;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xliff_alc")
}

fn project_workspace() -> Workspace {
    let workspace = Workspace::new();
    let mut paths: Vec<_> = std::fs::read_dir(fixture_dir().join("src"))
        .expect("fixture src")
        .map(|entry| entry.expect("entry").path())
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("read al file");
        workspace.file_index.add_file(path, text);
    }
    workspace
}

fn ids_with_targets(
    units: impl IntoIterator<Item = (String, Option<String>)>,
) -> BTreeMap<String, Option<String>> {
    units.into_iter().collect()
}

fn check(expected_file: &str, options: XliffOptions) {
    let expected_text =
        std::fs::read_to_string(fixture_dir().join(expected_file)).expect("expected");
    let expected = ids_with_targets(
        parse_xliff(expected_text.trim_start_matches('\u{feff}'))
            .into_values()
            .map(|unit| (unit.id, unit.object_target)),
    );
    let actual = ids_with_targets(
        extract_translation_units_with(&project_workspace(), options)
            .into_iter()
            .map(|unit| (unit.id, unit.object_target)),
    );
    let only_alc: Vec<_> = expected
        .keys()
        .filter(|id| !actual.contains_key(*id))
        .collect();
    let only_ours: Vec<_> = actual
        .keys()
        .filter(|id| !expected.contains_key(*id))
        .collect();
    let target_diffs: Vec<_> = expected
        .iter()
        .filter(|(id, target)| actual.get(*id).is_some_and(|ours| ours != *target))
        .map(|(id, target)| format!("{id}: alc {target:?}, ours {:?}", actual[id]))
        .collect();
    assert!(
        only_alc.is_empty() && only_ours.is_empty() && target_diffs.is_empty(),
        "only in alc: {only_alc:#?}\nonly in ours: {only_ours:#?}\nal-object-target: {target_diffs:#?}"
    );
}

#[test]
fn translation_file_matches_alc() {
    check("expected-translationfile.g.xlf", XliffOptions::default());
}

#[test]
fn generate_captions_matches_alc() {
    check(
        "expected-generatecaptions.g.xlf",
        XliffOptions::from_features(&["TranslationFile", "GenerateCaptions"]),
    );
}
