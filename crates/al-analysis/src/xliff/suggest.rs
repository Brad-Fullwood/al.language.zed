//! Translation suggestions from the project's own translation memory.

use super::*;

/// Where a translation suggestion came from, so callers/users can see *why* a
/// suggestion was made and how much to trust it. Serialized in kebab-case
/// (`tm-exact`, `tm-fuzzy`, `name`) so the JSON tag is self-describing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestionOrigin {
    /// Exact match on normalized source text against an already-translated unit
    /// in the project's translation memory. Highest confidence.
    TmExact,
    /// Fuzzy match (normalized whitespace/case + token overlap) against the
    /// translation memory. Always ranks below an exact match.
    TmFuzzy,
    /// Fallback: the source matched a workspace symbol (or field) name. This
    /// only echoes the English name, so it is a weak hint, not a translation.
    Name,
}

/// A translation suggestion from translation memory or base app symbol data.
#[derive(Debug, Clone, Serialize)]
pub struct TranslationSuggestion {
    pub unit_id: String,
    pub source: String,
    pub suggested_translation: String,
    pub confidence: f32,
    pub source_object: String,
    /// Provenance of the suggestion + implied trust level.
    pub origin: SuggestionOrigin,
}

/// Build a `TranslationSuggestion` for `unit`, cloning its id/source.
pub(super) fn make_suggestion(
    unit: &TranslationUnit,
    suggested_translation: String,
    confidence: f32,
    source_object: String,
    origin: SuggestionOrigin,
) -> TranslationSuggestion {
    TranslationSuggestion {
        unit_id: unit.id.clone(),
        source: unit.source.clone(),
        suggested_translation,
        confidence,
        source_object,
        origin,
    }
}

/// Minimum token-overlap (Sørensen–Dice coefficient) for a fuzzy TM hit.
pub(super) const FUZZY_THRESHOLD: f32 = 0.5;
/// A fuzzy hit's confidence is `overlap * FUZZY_CONFIDENCE_SCALE`, clamped by
/// `FUZZY_CONFIDENCE_MAX` so it always ranks strictly below an exact match.
pub(super) const FUZZY_CONFIDENCE_SCALE: f32 = 0.9;
pub(super) const FUZZY_CONFIDENCE_MAX: f32 = 0.89;

/// Lowercase + collapse internal whitespace so trivially-different source
/// strings ("Customer " vs "customer") compare equal for an exact TM lookup.
pub(super) fn normalize_source(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Distinct lowercased alphanumeric tokens, used for cheap token-overlap fuzzy
/// matching (no external fuzzy-distance crate — `strsim` is only a transitive
/// dependency, not a workspace dependency).
pub(super) fn tokenize(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Sørensen–Dice token overlap of two token sets: `2|A∩B| / (|A|+|B|)`.
pub(super) fn token_overlap(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    (2 * inter) as f32 / (a.len() + b.len()) as f32
}

/// One already-translated source/target pair, pre-normalized for matching.
/// (The normalized source itself lives as the key in [`TranslationMemory::by_norm`].)
pub(super) struct TmEntry {
    /// Distinct lowercased alphanumeric tokens for cheap fuzzy overlap.
    tokens: std::collections::HashSet<String>,
    /// Original (un-normalized) source text, for provenance reporting.
    source: String,
    /// The existing human translation.
    target: String,
}

/// The single best translation-memory match for a source string.
pub(super) struct TmMatch {
    target: String,
    confidence: f32,
    origin: SuggestionOrigin,
    source_object: String,
}

/// An index of already-translated `<trans-unit>` pairs gathered from the
/// project's XLIFF. Supports an O(1) exact normalized-source lookup and a cheap
/// token-overlap fuzzy match.
#[derive(Default)]
pub struct TranslationMemory {
    entries: Vec<TmEntry>,
    /// Normalized source -> index into `entries` for exact lookup. First
    /// occurrence wins, so results are deterministic for a given unit order.
    by_norm: HashMap<String, usize>,
}

impl TranslationMemory {
    /// Build a translation memory from translation units. Only units with a
    /// non-empty target whose state is `Translated` or `Final` are indexed —
    /// these are the trustworthy, completed translations.
    pub fn from_units<'a>(units: impl IntoIterator<Item = &'a TranslationUnit>) -> Self {
        let mut tm = TranslationMemory::default();
        for unit in units {
            let target = match unit.target.as_deref() {
                Some(t) if !t.trim().is_empty() => t,
                _ => continue,
            };
            if !matches!(
                unit.state,
                TranslationState::Translated | TranslationState::Final
            ) {
                continue;
            }
            let norm = normalize_source(&unit.source);
            if norm.is_empty() {
                continue;
            }
            let idx = tm.entries.len();
            tm.entries.push(TmEntry {
                tokens: tokenize(&unit.source),
                source: unit.source.clone(),
                target: target.to_string(),
            });
            tm.by_norm.entry(norm).or_insert(idx);
        }
        tm
    }

    /// Whether the memory holds no usable translations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find the best suggestion for `source`: an exact normalized match first
    /// (confidence 1.0), otherwise the highest token-overlap fuzzy match at or
    /// above [`FUZZY_THRESHOLD`] (lower confidence). `None` if nothing matches.
    fn best_match(&self, source: &str) -> Option<TmMatch> {
        let norm = normalize_source(source);
        if norm.is_empty() {
            return None;
        }
        if let Some(&idx) = self.by_norm.get(&norm) {
            let e = &self.entries[idx];
            return Some(TmMatch {
                target: e.target.clone(),
                confidence: 1.0,
                origin: SuggestionOrigin::TmExact,
                source_object: format!("translation-memory (exact): {:?}", e.source),
            });
        }

        let query_tokens = tokenize(source);
        if query_tokens.is_empty() {
            return None;
        }
        let mut best: Option<(f32, &TmEntry)> = None;
        for e in &self.entries {
            let overlap = token_overlap(&query_tokens, &e.tokens);
            if overlap >= FUZZY_THRESHOLD && best.map(|(b, _)| overlap > b).unwrap_or(true) {
                best = Some((overlap, e));
            }
        }
        best.map(|(overlap, e)| TmMatch {
            target: e.target.clone(),
            confidence: (overlap * FUZZY_CONFIDENCE_SCALE).min(FUZZY_CONFIDENCE_MAX),
            origin: SuggestionOrigin::TmFuzzy,
            source_object: format!("translation-memory (fuzzy): {:?}", e.source),
        })
    }
}

/// Suggest translations for untranslated units, preferring the project's
/// translation memory and falling back to symbol-name matching.
///
/// An empty `memory` means no translation memory is available, so every
/// suggestion has origin `name`.
///
/// `memory` is the pool of units to mine for existing translations — typically
/// every unit parsed from the project's XLIFF; the trustworthy (target present,
/// state translated/final) ones are selected internally. For each untranslated
/// unit, in order:
/// 1. an exact normalized-source TM match (origin `tm-exact`, confidence 1.0);
/// 2. else the best token-overlap fuzzy TM match (origin `tm-fuzzy`, lower);
/// 3. else symbol-name matching (origin `name`).
///
/// Suggestions are sorted by confidence (highest first), then unit id so the
/// output is deterministic.
pub fn suggest_translations(
    workspace: &Workspace,
    untranslated: &[&TranslationUnit],
    memory: &[&TranslationUnit],
) -> Vec<TranslationSuggestion> {
    let tm = TranslationMemory::from_units(memory.iter().copied());
    let mut suggestions = Vec::new();

    for unit in untranslated {
        if !tm.is_empty() {
            if let Some(m) = tm.best_match(&unit.source) {
                suggestions.push(make_suggestion(
                    unit,
                    m.target,
                    m.confidence,
                    m.source_object,
                    m.origin,
                ));
                continue;
            }
        }
        name_match_suggestions(unit, workspace, &mut suggestions);
    }

    suggestions.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.unit_id.cmp(&b.unit_id))
    });
    suggestions
}

/// Name-matching fallback: suggest the workspace symbol (or field) name whose
/// name equals the untranslated source. Tagged origin `name`.
pub(super) fn name_match_suggestions(
    unit: &TranslationUnit,
    workspace: &Workspace,
    out: &mut Vec<TranslationSuggestion>,
) {
    let source_lower = unit.source.to_lowercase();

    let matches = workspace.symbols.search(&unit.source, 5);
    for entry in &matches {
        let entry_name_lower = entry.name.to_lowercase();
        if entry_name_lower == source_lower {
            out.push(make_suggestion(
                unit,
                entry.name.clone(),
                1.0,
                format!("{:?} {}", entry.kind, entry.name),
                SuggestionOrigin::Name,
            ));
            continue;
        }

        for field in &entry.fields {
            if field.name.to_lowercase() == source_lower {
                out.push(make_suggestion(
                    unit,
                    field.name.clone(),
                    0.9,
                    format!("{:?} {} - Field {}", entry.kind, entry.name, field.name),
                    SuggestionOrigin::Name,
                ));
            }
        }
    }
}
