//! Bringing a language `.xlf` up to date with the generated `.g.xlf`.

use super::*;

/// Result of refreshing a language XLIFF against the generated XLIFF.
#[derive(Debug, Default, Serialize)]
pub struct RefreshResult {
    /// IDs of newly added translation units (in g.xlf but not in lang xlf)
    pub added: Vec<String>,
    /// IDs of units where source text changed (needs review)
    pub changed: Vec<String>,
    /// IDs of units removed from g.xlf (obsolete in lang xlf)
    pub removed: Vec<String>,
    /// Number of existing translations preserved
    pub preserved: usize,
}

/// Refresh a language-specific .xlf against the generated .g.xlf.
///
/// - Units in `generated` but not in `language` are added with state `new`.
/// - Units in both where source changed are marked `needs-review-translation`.
/// - Units in `language` but not in `generated` are marked `final` (obsolete).
/// - Existing translations are preserved.
///
/// Returns the updated `Vec<TranslationUnit>` and a `RefreshResult` summary.
pub fn refresh_xliff(
    generated: &[TranslationUnit],
    language: &HashMap<String, TranslationUnit>,
) -> (Vec<TranslationUnit>, RefreshResult) {
    let mut result_units = Vec::new();
    let mut refresh = RefreshResult::default();

    let generated_ids: std::collections::HashSet<&str> =
        generated.iter().map(|u| u.id.as_str()).collect();

    for gen_unit in generated {
        if let Some(lang_unit) = language.get(&gen_unit.id) {
            // Everything but the translation comes from the generated file:
            // keeping the language file's notes meant a developer `Comment`
            // added or edited in the code never reached a unit that already
            // existed.
            if lang_unit.source != gen_unit.source {
                result_units.push(TranslationUnit {
                    target: lang_unit.target.clone(),
                    state: TranslationState::NeedsReviewTranslation,
                    ..gen_unit.clone()
                });
                refresh.changed.push(gen_unit.id.clone());
            } else {
                result_units.push(TranslationUnit {
                    target: lang_unit.target.clone(),
                    state: lang_unit.state.clone(),
                    ..gen_unit.clone()
                });
                refresh.preserved += 1;
            }
        } else {
            result_units.push(TranslationUnit {
                state: TranslationState::New,
                ..gen_unit.clone()
            });
            refresh.added.push(gen_unit.id.clone());
        }
    }

    // `language` is a HashMap, so iterating it directly appended obsolete units
    // in a different order on every run — huge spurious VCS diffs on the
    // rewritten language file. Sort by id for stable output.
    let mut obsolete: Vec<&String> = language
        .keys()
        .filter(|id| !generated_ids.contains(id.as_str()))
        .collect();
    obsolete.sort_unstable();
    for id in obsolete {
        let lang_unit = &language[id];
        result_units.push(TranslationUnit {
            state: TranslationState::Final,
            ..lang_unit.clone()
        });
        refresh.removed.push(id.clone());
    }

    (result_units, refresh)
}

/// Find all translation units that have no target translation.
///
/// Returns units where `target` is `None` or empty, sorted by id. The caller
/// builds its input from a `HashMap`'s values, so without the sort the
/// `xlf.untranslated` output came out in a different order on every run.
pub fn find_untranslated(units: &[TranslationUnit]) -> Vec<&TranslationUnit> {
    let mut untranslated: Vec<&TranslationUnit> = units
        .iter()
        .filter(|u| {
            u.target
                .as_deref()
                .map(|t| t.trim().is_empty())
                .unwrap_or(true)
        })
        .filter(|u| u.state != TranslationState::Final)
        .collect();
    untranslated.sort_by(|a, b| a.id.cmp(&b.id));
    untranslated
}
