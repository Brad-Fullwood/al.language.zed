//! XLIFF translation support for AL projects.
//!
//! Provides XLIFF 1.2 generation from AL source (captions, tooltips, labels),
//! refresh workflow for language-specific .xlf files, and translation queries.
//!
//! # XLIFF file roles
//! - `*.g.xlf` — generated translation file (source of truth, produced by build)
//! - `*.xlf` (e.g., `de-DE.xlf`) — language-specific translation file (manually maintained)
//!
//! # Translation unit ID format
//! `{ObjectType} {ObjectId} - {PropertyName} {FieldId} - {PropertyType}`
//! Matches the MS AL extension format so translation memories are compatible.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use al_workspace::Workspace;

/// Upper bound on `.xlf` files we'll read into memory. Real AL translation
/// files are at most a few MB even on huge BC apps; a 64 MB cap is a
/// defence-in-depth bound that lets `parse_xliff` keep its simple
/// in-memory line-based parser without risking OOM from a malformed or
/// hostile input.
pub const MAX_XLF_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Return whether `path`'s on-disk size exceeds `MAX_XLF_FILE_BYTES`.
/// Callers should refuse to parse the file if this returns `Some(true)`.
/// `Some(false)` means the file is below the cap; `None` means metadata
/// could not be read (file missing or perms error) — caller decides
/// whether to surface that error itself.
pub fn xlf_exceeds_cap(path: &Path) -> Option<bool> {
    let meta = std::fs::metadata(path).ok()?;
    Some(meta.len() > MAX_XLF_FILE_BYTES)
}

/// A single translatable text unit extracted from AL source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationUnit {
    /// Unique ID following MS AL format: `ObjectType ObjectId - PropertyName FieldId - PropertyType`
    pub id: String,
    /// Object type (e.g. "Table", "Page", "Codeunit")
    pub object_type: String,
    pub object_id: u32,
    pub object_name: String,
    /// Source text (English caption/tooltip/label value)
    pub source: String,
    /// Translated text (if available — None means untranslated)
    pub target: Option<String>,
    pub state: TranslationState,
    /// Note (context from AL property name and field)
    pub note: Option<String>,
}

/// Translation state following XLIFF 1.2 conventions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationState {
    New,
    Translated,
    /// Source changed — needs review
    NeedsReviewTranslation,
    /// Source was removed — no longer used
    Final,
}

impl TranslationState {
    fn as_xliff_state(&self) -> &'static str {
        match self {
            TranslationState::New => "new",
            TranslationState::Translated => "translated",
            TranslationState::NeedsReviewTranslation => "needs-review-translation",
            TranslationState::Final => "final",
        }
    }

    fn from_xliff_state(s: &str) -> Self {
        match s {
            "translated" => TranslationState::Translated,
            "needs-review-translation" => TranslationState::NeedsReviewTranslation,
            "final" => TranslationState::Final,
            _ => TranslationState::New,
        }
    }
}

/// Extract all translatable text units from the workspace AL files.
///
/// Scans for `Caption`, `ToolTip`, and `Label` properties in all workspace .al files.
/// Returns units in the order they appear (deterministic output).
pub fn extract_translation_units(workspace: &Workspace) -> Vec<TranslationUnit> {
    let mut units = Vec::new();

    let mut paths: Vec<_> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .collect();
    paths.sort();

    for path in &paths {
        let text = match workspace.file_index.files.get(path) {
            Some(t) => t.value().clone(),
            None => continue,
        };
        extract_from_file(path, &text, &mut units);
    }

    units
}

/// Extract translation units from a single AL file.
fn extract_from_file(path: &Path, text: &str, units: &mut Vec<TranslationUnit>) {
    let (obj_type, obj_id, obj_name) = match detect_object_header(text) {
        Some(v) => v,
        None => return,
    };

    let mut field_id: u32 = 0;
    let mut current_field: Option<String> = None;
    // Per-file label counter so Label IDs depend only on position within this
    // object, not on how many units earlier files contributed. Using the
    // cumulative `units.len()` would make the same label's ID shift whenever
    // file ordering changes, breaking translation-memory matching.
    let mut label_index: usize = 0;

    for line in text.lines() {
        let trimmed = line.trim();

        // Track field declarations to associate captions with fields
        if let Some(fid) = parse_field_declaration(trimmed) {
            field_id = fid;
            current_field = parse_field_name(trimmed);
        }
        let context = current_field.as_deref().unwrap_or(&obj_name);

        if let Some(caption) = parse_property_value(trimmed, "Caption") {
            let id = make_translation_id(
                &obj_type, obj_id, &obj_name, "Caption", field_id, context, path,
            );
            units.push(make_translation_unit(
                id,
                &obj_type,
                obj_id,
                &obj_name,
                caption,
                current_field.as_ref().map(|f| format!("Caption for {}", f)),
            ));
        }

        if let Some(tooltip) = parse_property_value(trimmed, "ToolTip") {
            let id = make_translation_id(
                &obj_type, obj_id, &obj_name, "ToolTip", field_id, context, path,
            );
            units.push(make_translation_unit(
                id,
                &obj_type,
                obj_id,
                &obj_name,
                tooltip,
                current_field.as_ref().map(|f| format!("ToolTip for {}", f)),
            ));
        }

        // Label 'varname': 'text'  or   MyLabel: Label 'text';
        if let Some(label_text) = parse_label_declaration(trimmed) {
            let id = make_label_id(&obj_type, obj_id, &obj_name, field_id, path, label_index);
            label_index += 1;
            units.push(make_translation_unit(
                id,
                &obj_type,
                obj_id,
                &obj_name,
                label_text,
                Some("Label".to_string()),
            ));
        }
    }
}

fn make_translation_unit(
    id: String,
    obj_type: &str,
    obj_id: u32,
    obj_name: &str,
    source: String,
    note: Option<String>,
) -> TranslationUnit {
    TranslationUnit {
        id,
        object_type: obj_type.to_string(),
        object_id: obj_id,
        object_name: obj_name.to_string(),
        source,
        target: None,
        state: TranslationState::New,
        note,
    }
}

/// Detect the first AL object declaration line: (type, id, name).
fn detect_object_header(text: &str) -> Option<(String, u32, String)> {
    // Sort object type keywords by length descending so that longer keywords (extensions) are
    // tried before their shorter base-type prefixes — e.g. "tableextension" before "table".
    // This avoids false prefix matches like "pagepart" matching "page".
    let mut sorted_types: Vec<&str> = al_syntax::language_data::object_types()
        .iter()
        .map(|ot| ot.keyword.as_str())
        .collect();
    sorted_types.sort_by_key(|k| Reverse(k.len()));

    for line in text.lines().take(10) {
        let lower = line.trim().to_lowercase();
        for ot in &sorted_types {
            // Require a word boundary after the keyword (space, tab, or digit) to avoid
            // false prefix matches like "pagepart" matching "page".
            if let Some(after) = lower.strip_prefix(ot) {
                let boundary =
                    after.starts_with(|c: char| c.is_ascii_whitespace() || c.is_ascii_digit());
                if !boundary {
                    continue;
                }
                // e.g. "table 50100 \"My Table\""
                let rest = line.trim()[ot.len()..].trim();
                let (id_str, rest2) = split_id_and_name(rest);
                let id: u32 = id_str.parse().unwrap_or(0);
                let name = parse_object_name(rest2.trim());
                if !name.is_empty() || id > 0 {
                    return Some((capitalize(ot), id, name));
                }
            }
        }
    }
    None
}

/// Extract just the object NAME from the header remainder, stopping at the
/// closing quote (quoted identifiers) or the first whitespace (bare
/// identifiers). Extension headers continue with `extends "Base"` —
/// `trim_matches('"')` on the whole remainder swallowed that clause into the
/// name and mangled every xlf unit id for extension objects.
fn parse_object_name(rest: &str) -> String {
    let rest = rest.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = rest.strip_prefix(quote) {
            return inner.split(quote).next().unwrap_or_default().to_string();
        }
    }
    rest.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Split "50100 \"Name\"" into ("50100", "\"Name\"").
fn split_id_and_name(s: &str) -> (&str, &str) {
    let s = s.trim();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Parse a `field(50; MyField; ...)` or `field(MyField; ...)` declaration.
fn parse_field_declaration(line: &str) -> Option<u32> {
    let lower = line.to_lowercase();
    if lower.starts_with("field(") {
        // field(50; FieldName; Type) or field(FieldName; Type)
        let inner = &line[6..line.find(')')?];
        let first = inner.split(';').next()?.trim();
        if let Ok(id) = first.trim_matches('"').parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn parse_field_name(line: &str) -> Option<String> {
    if line.to_lowercase().starts_with("field(") {
        let inner = &line[6..line.find(')')?];
        let parts: Vec<&str> = inner.split(';').collect();
        // field(id; name; type) or field(name; type)
        let name_part = if parts.len() >= 3 {
            parts[1].trim()
        } else {
            parts.first()?.trim()
        };
        return Some(name_part.trim_matches('"').trim_matches('\'').to_string());
    }
    None
}

/// Parse a property like `Caption = 'Some text';` or `Caption = 'text', Comment = 'note';`
fn parse_property_value(line: &str, property: &str) -> Option<String> {
    let prefix = format!("{} =", property);
    let prefix_lower = prefix.to_lowercase();
    let line_lower = line.to_lowercase();
    if !line_lower.starts_with(&prefix_lower) {
        return None;
    }
    let after_eq = &line[prefix.len()..].trim_start_matches([' ', '\t']);
    extract_single_quoted(after_eq)
}

/// Parse a `Label` variable declaration like `MyLabel: Label 'Some text';`
fn parse_label_declaration(line: &str) -> Option<String> {
    // Pattern: <name>: Label '<text>' [, ...];
    let colon = line.find(':')?;
    let after_colon = line[colon + 1..].trim();
    let lower = after_colon.to_lowercase();
    if !lower.starts_with("label ") {
        return None;
    }
    let after_label = after_colon[6..].trim();
    extract_single_quoted(after_label)
}

fn extract_single_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let inner = &s[start + 1..];
    // Find closing quote (handle escaped '' as single quote)
    let mut result = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                result.push('\'');
            } else {
                break;
            }
        } else {
            result.push(ch);
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

fn make_translation_id(
    obj_type: &str,
    obj_id: u32,
    obj_name: &str,
    property: &str,
    field_id: u32,
    context: &str,
    _path: &Path,
) -> String {
    if field_id > 0 {
        format!(
            "{} {} {} - {} {} - {}",
            obj_type, obj_id, obj_name, property, field_id, context
        )
    } else {
        format!("{} {} {} - {}", obj_type, obj_id, obj_name, property)
    }
}

fn make_label_id(
    obj_type: &str,
    obj_id: u32,
    obj_name: &str,
    _field_id: u32,
    _path: &Path,
    index: usize,
) -> String {
    format!("{} {} {} - Label {}", obj_type, obj_id, obj_name, index)
}

/// Generate a `.g.xlf` XLIFF 1.2 file from translation units.
///
/// Returns the XML string. Write this to `Translations/<AppName>.g.xlf`.
pub fn generate_xliff(
    app_name: &str,
    source_language: &str,
    target_language: &str,
    units: &[TranslationUnit],
) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<xliff version=\"1.2\" xmlns=\"urn:oasis:names:tc:xliff:document:1.2\" ");
    xml.push_str("xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" ");
    xml.push_str("xsi:schemaLocation=\"urn:oasis:names:tc:xliff:document:1.2 xliff-core-1.2-transitional.xsd\">\n");

    xml.push_str(&format!(
        "  <file datatype=\"xml\" source-language=\"{}\" target-language=\"{}\" ",
        xml_escape(source_language),
        xml_escape(target_language)
    ));
    xml.push_str(&format!("original=\"{}\">\n", xml_escape(app_name)));
    xml.push_str("    <body>\n");
    xml.push_str(&format!("      <group id=\"{}\">\n", xml_escape(app_name)));

    for unit in units {
        xml.push_str(&format!(
            "        <trans-unit id=\"{}\" size-unit=\"char\" translate=\"yes\" xml:space=\"preserve\">\n",
            xml_escape(&unit.id)
        ));
        xml.push_str(&format!(
            "          <source xml:space=\"preserve\">{}</source>\n",
            xml_escape(&unit.source)
        ));
        if let Some(target) = &unit.target {
            xml.push_str(&format!(
                "          <target state=\"{}\" xml:space=\"preserve\">{}</target>\n",
                unit.state.as_xliff_state(),
                xml_escape(target)
            ));
        } else {
            xml.push_str(&format!(
                "          <target state=\"{}\" xml:space=\"preserve\"/>\n",
                unit.state.as_xliff_state()
            ));
        }
        if let Some(note) = &unit.note {
            xml.push_str(&format!("          <note>{}</note>\n", xml_escape(note)));
        }
        xml.push_str("        </trans-unit>\n");
    }

    xml.push_str("      </group>\n");
    xml.push_str("    </body>\n");
    xml.push_str("  </file>\n");
    xml.push_str("</xliff>\n");
    xml
}

/// Escape characters that are special in XML attribute values and text content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Parse an XLIFF 1.2 file into a map of `id → TranslationUnit`.
///
/// Uses a simple line-based parser that handles typical AL XLIFF output without
/// requiring a full XML parser dependency.
///
/// Multi-line `<source>` / `<target>` / `<note>` bodies are collected through
/// their closing tags.
pub fn parse_xliff(content: &str) -> HashMap<String, TranslationUnit> {
    let mut units: HashMap<String, TranslationUnit> = HashMap::new();
    let mut current_id: Option<String> = None;
    let mut current_source: Option<String> = None;
    let mut current_target: Option<String> = None;
    let mut current_state = TranslationState::New;
    let mut current_note: Option<String> = None;

    /// Per-tag state for multi-line accumulation. When `accumulator` is
    /// `Some(buf)` the next line(s) are body content of the named tag and
    /// get appended (with a leading `\n` between them) until we see the
    /// closing tag.
    #[derive(Default)]
    struct MultiLine {
        target_tag: Option<&'static str>, // "source" | "target" | "note"
        accumulator: String,
    }
    let mut multi = MultiLine::default();

    /// Try to extract a single-line `<tag …>body</tag>` payload from a line.
    /// Returns Some(body) if both opening `>` and closing `</tag>` are
    /// present on the same line. Returns None if the closing tag isn't
    /// on this line (caller will switch to multi-line accumulation).
    fn extract_single_line(line: &str, tag: &str) -> Option<String> {
        let open_end = line.find('>')?;
        let close_marker = format!("</{tag}>");
        let close_start = line[open_end + 1..].find(&close_marker)?;
        let body = &line[open_end + 1..open_end + 1 + close_start];
        // An empty body (`<source></source>`) is a valid, present element.
        // Return `Some(String::new())` rather than `None` so the caller treats
        // it as a found single-line element instead of switching to multi-line
        // mode and hunting for a closing tag that has already passed — which
        // would silently drop the trans-unit on round-trip.
        Some(xml_unescape(body))
    }

    /// Extract a partial body when the opening tag is on this line but the
    /// closing tag isn't. Returns the body portion (everything after `>`).
    fn extract_open_only(line: &str) -> Option<String> {
        let open_end = line.find('>')?;
        let after = &line[open_end + 1..];
        // Self-closing form `<tag .../>` — open_end falls on the `/` so the
        // body would be empty.
        if line[..open_end].trim_end().ends_with('/') {
            return None;
        }
        Some(xml_unescape(after))
    }

    fn append_to_accumulator(acc: &mut String, text: &str) {
        if !acc.is_empty() {
            acc.push('\n');
        }
        acc.push_str(text);
    }

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(tag) = multi.target_tag {
            let close_marker = format!("</{tag}>");
            if let Some(close_idx) = line.find(&close_marker) {
                // Last chunk — append everything up to the close marker.
                let last = &line[..close_idx];
                append_to_accumulator(&mut multi.accumulator, &xml_unescape(last));
                let body = std::mem::take(&mut multi.accumulator);
                match tag {
                    "source" => current_source = Some(body),
                    "target" => current_target = Some(body),
                    "note" => current_note = Some(body),
                    _ => {}
                }
                multi.target_tag = None;
            } else {
                append_to_accumulator(&mut multi.accumulator, &xml_unescape(line));
            }
            continue;
        }

        if trimmed.starts_with("<trans-unit ") {
            current_id = extract_xml_attr(trimmed, "id");
            current_source = None;
            current_target = None;
            current_state = TranslationState::New;
            current_note = None;
        } else if trimmed.starts_with("<source") {
            if let Some(body) = extract_single_line(trimmed, "source") {
                current_source = Some(body);
            } else if let Some(partial) = extract_open_only(trimmed) {
                multi.target_tag = Some("source");
                multi.accumulator = partial;
            }
        } else if trimmed.starts_with("<target") {
            let state_str = extract_xml_attr(trimmed, "state").unwrap_or_default();
            current_state = TranslationState::from_xliff_state(&state_str);
            if let Some(body) = extract_single_line(trimmed, "target") {
                current_target = Some(body);
            } else if let Some(partial) = extract_open_only(trimmed) {
                multi.target_tag = Some("target");
                multi.accumulator = partial;
            }
        } else if trimmed.starts_with("<note>") || trimmed.starts_with("<note ") {
            // MS-format `.xlf` notes carry attributes
            // (`<note from="Developer" annotates="general" priority="2">…`),
            // so match both the bare `<note>` form (what we generate) and the
            // attributed `<note …>` form. `extract_single_line` /
            // `extract_open_only` already skip past the attributes by anchoring
            // on the first `>` of the open tag.
            if let Some(body) = extract_single_line(trimmed, "note") {
                current_note = Some(body);
            } else if let Some(partial) = extract_open_only(trimmed) {
                multi.target_tag = Some("note");
                multi.accumulator = partial;
            }
        } else if trimmed.starts_with("</trans-unit>") {
            if let (Some(id), Some(source)) = (current_id.take(), current_source.take()) {
                let unit = TranslationUnit {
                    id: id.clone(),
                    object_type: String::new(), // reconstructed from id
                    object_id: 0,
                    object_name: String::new(),
                    source,
                    target: current_target.take(),
                    state: current_state.clone(),
                    note: current_note.take(),
                };
                units.insert(id, unit);
            }
            current_target = None;
        }
    }

    units
}

/// Extract an XML attribute value from a tag string.
fn extract_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let search = format!("{}=\"", attr);
    let start = tag.find(&search)? + search.len();
    let end = tag[start..].find('"')? + start;
    let raw = &tag[start..end];
    Some(xml_unescape(raw))
}

fn xml_unescape(s: &str) -> String {
    // `&amp;` MUST be unescaped last. With chained `str::replace`, doing it
    // first would let the `&` it produces be re-interpreted as the start of a
    // later entity: e.g. user text `&lt;` is escaped to `&amp;lt;`, and an
    // `&amp;`-first order would turn that back into `&lt;` → `<`, silently
    // losing the original literal. Unescaping the named entities first and
    // `&amp;` last keeps the escape/unescape cycle lossless.
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

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
            if lang_unit.source != gen_unit.source {
                result_units.push(TranslationUnit {
                    source: gen_unit.source.clone(),
                    target: lang_unit.target.clone(),
                    state: TranslationState::NeedsReviewTranslation,
                    ..lang_unit.clone()
                });
                refresh.changed.push(gen_unit.id.clone());
            } else {
                result_units.push(lang_unit.clone());
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

    for (id, lang_unit) in language {
        if !generated_ids.contains(id.as_str()) {
            result_units.push(TranslationUnit {
                state: TranslationState::Final,
                ..lang_unit.clone()
            });
            refresh.removed.push(id.clone());
        }
    }

    (result_units, refresh)
}

/// Find all translation units that have no target translation.
///
/// Returns units where `target` is `None` or empty, sorted by object type and ID.
pub fn find_untranslated(units: &[TranslationUnit]) -> Vec<&TranslationUnit> {
    units
        .iter()
        .filter(|u| {
            u.target
                .as_deref()
                .map(|t| t.trim().is_empty())
                .unwrap_or(true)
        })
        .filter(|u| u.state != TranslationState::Final)
        .collect()
}

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
fn make_suggestion(
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
const FUZZY_THRESHOLD: f32 = 0.5;
/// A fuzzy hit's confidence is `overlap * FUZZY_CONFIDENCE_SCALE`, clamped by
/// `FUZZY_CONFIDENCE_MAX` so it always ranks strictly below an exact match.
const FUZZY_CONFIDENCE_SCALE: f32 = 0.9;
const FUZZY_CONFIDENCE_MAX: f32 = 0.89;

/// Lowercase + collapse internal whitespace so trivially-different source
/// strings ("Customer " vs "customer") compare equal for an exact TM lookup.
fn normalize_source(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Distinct lowercased alphanumeric tokens, used for cheap token-overlap fuzzy
/// matching (no external fuzzy-distance crate — `strsim` is only a transitive
/// dependency, not a workspace dependency).
fn tokenize(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Sørensen–Dice token overlap of two token sets: `2|A∩B| / (|A|+|B|)`.
fn token_overlap(
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
struct TmEntry {
    /// Distinct lowercased alphanumeric tokens for cheap fuzzy overlap.
    tokens: std::collections::HashSet<String>,
    /// Original (un-normalized) source text, for provenance reporting.
    source: String,
    /// The existing human translation.
    target: String,
}

/// The single best translation-memory match for a source string.
struct TmMatch {
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

/// Suggest translations for untranslated units by matching against base app symbols.
///
/// Backward-compatible entry point (used by the LSP `xlf suggest` dispatch). It
/// carries no translation memory, so every suggestion has origin `name`. Prefer
/// [`suggest_translations_with_memory`] when the project's already-translated
/// units are available — that path adds the `tm-exact` / `tm-fuzzy` backends.
///
/// Returns suggestions sorted by confidence (highest first).
pub fn suggest_translations(
    untranslated: &[&TranslationUnit],
    workspace: &Workspace,
) -> Vec<TranslationSuggestion> {
    suggest_translations_with_memory(untranslated, &[], workspace)
}

/// Suggest translations for untranslated units, preferring the project's
/// translation memory and falling back to symbol-name matching.
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
pub fn suggest_translations_with_memory(
    untranslated: &[&TranslationUnit],
    memory: &[&TranslationUnit],
    workspace: &Workspace,
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
fn name_match_suggestions(
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

/// Generate the `.g.xlf` file for a workspace and write it to the Translations directory.
///
/// Creates `<project_root>/Translations/<AppName>.g.xlf`.
/// Returns:
/// - `Ok(Some((path, count)))` on success.
/// - `Ok(None)` when no translatable units exist (not an error).
/// - `Err(io::Error)` when directory creation or file write fails.
pub fn build_xliff(
    workspace: &Workspace,
    project_root: &Path,
) -> std::io::Result<Option<(PathBuf, usize)>> {
    let app_name = read_app_name(project_root).unwrap_or_else(|| "App".to_string());

    let units = extract_translation_units(workspace);
    if units.is_empty() {
        return Ok(None);
    }

    let xlf_content = generate_xliff(&app_name, "en-US", "en-US", &units);

    let translations_dir = project_root.join("Translations");
    std::fs::create_dir_all(&translations_dir)?;

    let xlf_path = translations_dir.join(format!("{}.g.xlf", app_name));
    std::fs::write(&xlf_path, xlf_content)?;

    Ok(Some((xlf_path, units.len())))
}

fn read_app_name(project_root: &Path) -> Option<String> {
    let bytes = std::fs::read(project_root.join("app.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("name")?
        .as_str()
        .map(|s| s.replace([' ', '"', '\''], ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_single_quoted() {
        assert_eq!(
            extract_single_quoted("'Hello world'"),
            Some("Hello world".to_string())
        );
        assert_eq!(
            extract_single_quoted("'It''s a test'"),
            Some("It's a test".to_string())
        );
        assert_eq!(extract_single_quoted("''"), None);
        assert_eq!(extract_single_quoted("no quotes"), None);
    }

    #[test]
    fn test_parse_property_value() {
        assert_eq!(
            parse_property_value("Caption = 'My Caption';", "Caption"),
            Some("My Caption".to_string())
        );
        assert_eq!(
            parse_property_value("ToolTip = 'Some tooltip text';", "ToolTip"),
            Some("Some tooltip text".to_string())
        );
        assert_eq!(
            parse_property_value("ApplicationArea = All;", "Caption"),
            None
        );
    }

    #[test]
    fn test_detect_object_header() {
        let text = "table 50100 \"Customer Extension\"\n{\n    fields\n    {};\n}";
        let result = detect_object_header(text).unwrap();
        assert_eq!(result.0, "Table");
        assert_eq!(result.1, 50100);
        assert_eq!(result.2, "Customer Extension");
    }

    /// the name must stop at the closing quote — extension
    /// headers carry an `extends` clause that was being swallowed into the
    /// name (`Sales Order Pageext" extends "Sales Order`), mangling every
    /// xlf unit id for extension objects.
    #[test]
    fn detect_object_header_quoted_name_stops_before_extends_clause() {
        let text = r#"pageextension 50101 "Sales Order Pageext" extends "Sales Order""#;
        let (ty, id, name) = detect_object_header(text).unwrap();
        assert_eq!(ty, "Pageextension");
        assert_eq!(id, 50101);
        assert_eq!(name, "Sales Order Pageext");
    }

    #[test]
    fn detect_object_header_unquoted_name_stops_before_extends_clause() {
        let text = "tableextension 50100 MyExt extends MyBase";
        let (ty, id, name) = detect_object_header(text).unwrap();
        assert_eq!(ty, "Tableextension");
        assert_eq!(id, 50100);
        assert_eq!(name, "MyExt");
    }

    #[test]
    fn test_generate_xliff_roundtrip() {
        let units = vec![
            TranslationUnit {
                id: "Table 50100 MyTable - Caption".to_string(),
                object_type: "Table".to_string(),
                object_id: 50100,
                object_name: "MyTable".to_string(),
                source: "My Table".to_string(),
                target: None,
                state: TranslationState::New,
                note: None,
            },
            TranslationUnit {
                id: "Table 50100 MyTable - ToolTip 10 Name".to_string(),
                object_type: "Table".to_string(),
                object_id: 50100,
                object_name: "MyTable".to_string(),
                source: "Specifies the name".to_string(),
                target: Some("Gibt den Namen an".to_string()),
                state: TranslationState::Translated,
                note: Some("ToolTip for Name".to_string()),
            },
        ];

        let xml = generate_xliff("MyApp", "en-US", "de-DE", &units);
        assert!(xml.contains("<?xml version=\"1.0\""));
        assert!(xml.contains("source-language=\"en-US\""));
        assert!(xml.contains("target-language=\"de-DE\""));
        assert!(xml.contains("Table 50100 MyTable - Caption"));
        assert!(xml.contains("My Table"));
        assert!(xml.contains("Gibt den Namen an"));
        assert!(xml.contains("state=\"translated\""));

        let parsed = parse_xliff(&xml);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains_key("Table 50100 MyTable - Caption"));
        assert_eq!(parsed["Table 50100 MyTable - Caption"].source, "My Table");
        let translated = &parsed["Table 50100 MyTable - ToolTip 10 Name"];
        assert_eq!(translated.target.as_deref(), Some("Gibt den Namen an"));
        assert_eq!(translated.state, TranslationState::Translated);
    }

    #[test]
    fn test_generate_xliff_roundtrip_escaped_chars() {
        // Source text containing every XML-special character, including
        // literal entity-looking strings, must survive a generate → parse
        // roundtrip without data loss. A naive unescape that handles `&amp;`
        // first would turn `&lt;` (escaped to `&amp;lt;`) back into `<`.
        let cases = [
            "Use &lt; for less-than",
            "A & B",
            "Cost > Limit & < Budget",
            "Quote: \"hello\" and 'world'",
            "XML: &amp;lt; nested",
        ];
        for (i, source) in cases.iter().enumerate() {
            let units = vec![TranslationUnit {
                id: format!("Table 50100 MyTable - Caption {i}"),
                object_type: "Table".to_string(),
                object_id: 50100,
                object_name: "MyTable".to_string(),
                source: (*source).to_string(),
                target: Some((*source).to_string()),
                state: TranslationState::Translated,
                note: Some((*source).to_string()),
            }];

            let xml = generate_xliff("MyApp", "en-US", "de-DE", &units);
            let parsed = parse_xliff(&xml);
            let id = format!("Table 50100 MyTable - Caption {i}");
            let unit = parsed.get(&id).expect("unit present after roundtrip");
            assert_eq!(
                unit.source, *source,
                "source roundtrip failed for {source:?}"
            );
            assert_eq!(
                unit.target.as_deref(),
                Some(*source),
                "target roundtrip failed for {source:?}"
            );
            assert_eq!(
                unit.note.as_deref(),
                Some(*source),
                "note roundtrip failed for {source:?}"
            );
        }
    }

    #[test]
    fn test_generate_xliff_roundtrip_quoted_id_attribute() {
        // AL object names can legitimately contain double quotes (quoted
        // identifiers), so a translation-unit id may too. On generation the
        // quote is escaped to `&quot;` inside the `id="..."` attribute; on
        // parse, `extract_xml_attr` must find the real closing quote (not the
        // escaped one) and unescape the value back to the exact original id.
        let id = r#"Table 50100 "My Table" - Property Caption"#;
        let units = vec![TranslationUnit {
            id: id.to_string(),
            object_type: "Table".to_string(),
            object_id: 50100,
            object_name: "My Table".to_string(),
            source: "Hello".to_string(),
            target: Some("Hallo".to_string()),
            state: TranslationState::Translated,
            note: None,
        }];

        let xml = generate_xliff("MyApp", "en-US", "de-DE", &units);
        assert!(
            xml.contains("&quot;"),
            "quote in id should be XML-escaped in the attribute value"
        );

        let parsed = parse_xliff(&xml);
        let unit = parsed
            .get(id)
            .expect("unit with quoted id present after roundtrip");
        assert_eq!(unit.id, id, "quoted id did not survive the roundtrip");
        assert_eq!(unit.source, "Hello");
        assert_eq!(unit.target.as_deref(), Some("Hallo"));
    }

    #[test]
    fn test_refresh_xliff_adds_new_removes_old() {
        let generated = vec![
            TranslationUnit {
                id: "T1".to_string(),
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                source: "Hello".to_string(),
                target: None,
                state: TranslationState::New,
                note: None,
            },
            TranslationUnit {
                id: "T2".to_string(),
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                source: "World - Updated".to_string(),
                target: None,
                state: TranslationState::New,
                note: None,
            },
        ];

        let mut existing: HashMap<String, TranslationUnit> = HashMap::new();
        existing.insert(
            "T2".to_string(),
            TranslationUnit {
                id: "T2".to_string(),
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                source: "World".to_string(), // old source
                target: Some("Welt".to_string()),
                state: TranslationState::Translated,
                note: None,
            },
        );
        existing.insert(
            "T_OLD".to_string(),
            TranslationUnit {
                id: "T_OLD".to_string(),
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                source: "Obsolete".to_string(),
                target: Some("Veraltet".to_string()),
                state: TranslationState::Translated,
                note: None,
            },
        );

        let (updated, result) = refresh_xliff(&generated, &existing);

        assert!(result.added.contains(&"T1".to_string()));
        assert!(result.changed.contains(&"T2".to_string()));
        assert!(result.removed.contains(&"T_OLD".to_string()));
        let t2 = updated.iter().find(|u| u.id == "T2").unwrap();
        assert_eq!(t2.target.as_deref(), Some("Welt"));
        assert_eq!(t2.state, TranslationState::NeedsReviewTranslation);
    }

    #[test]
    fn test_find_untranslated() {
        let units = vec![
            TranslationUnit {
                id: "T1".to_string(),
                source: "Hello".to_string(),
                target: None,
                state: TranslationState::New,
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                note: None,
            },
            TranslationUnit {
                id: "T2".to_string(),
                source: "World".to_string(),
                target: Some("Welt".to_string()),
                state: TranslationState::Translated,
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                note: None,
            },
            TranslationUnit {
                id: "T3".to_string(),
                source: "Old".to_string(),
                target: Some("Alt".to_string()),
                state: TranslationState::Final,
                object_type: "Table".to_string(),
                object_id: 1,
                object_name: "T".to_string(),
                note: None,
            },
        ];

        let untranslated = find_untranslated(&units);
        assert_eq!(untranslated.len(), 1);
        assert_eq!(untranslated[0].id, "T1");
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("a & b"), "a &amp; b");
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(xml_escape("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn test_extract_from_file() {
        let al = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            Caption = 'Name';
            ToolTip = 'Specifies the name of the record.';
        }
        field(2; Description; Text[250])
        {
            Caption = 'Description';
        }
    }
    var
        MyLabel: Label 'This is a label';
}"#;
        let mut units = Vec::new();
        extract_from_file(Path::new("test.al"), al, &mut units);
        assert!(
            units.iter().any(|u| u.source == "Name"),
            "Should extract Caption 'Name'"
        );
        assert!(
            units
                .iter()
                .any(|u| u.source == "Specifies the name of the record."),
            "Should extract ToolTip"
        );
        assert!(
            units.iter().any(|u| u.source == "This is a label"),
            "Should extract Label"
        );
        assert!(
            units.iter().any(|u| u.source == "Description"),
            "Should extract Caption 'Description'"
        );
    }

    #[test]
    fn label_ids_are_stable_across_extraction_order() {
        let al = r#"codeunit 50100 "My Codeunit"
{
    var
        FirstLabel: Label 'First';
        SecondLabel: Label 'Second';
}"#;

        let mut units_a = Vec::new();
        extract_from_file(Path::new("a.al"), al, &mut units_a);
        let ids_a: Vec<String> = units_a.iter().map(|u| u.id.clone()).collect();

        let mut units_b = vec![make_test_unit("preexisting one"), make_test_unit("two")];
        let pre_len = units_b.len();
        extract_from_file(Path::new("a.al"), al, &mut units_b);
        let ids_b: Vec<String> = units_b[pre_len..].iter().map(|u| u.id.clone()).collect();

        assert_eq!(
            ids_a, ids_b,
            "label IDs must not depend on prior extraction state"
        );
        assert_eq!(ids_a[0], "Codeunit 50100 My Codeunit - Label 0");
        assert_eq!(ids_a[1], "Codeunit 50100 My Codeunit - Label 1");
    }

    #[test]
    fn parse_xliff_preserves_empty_single_line_source() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<xliff version="1.2">
  <file datatype="xml" source-language="en-US" target-language="de-DE" original="MyApp">
    <body>
      <group id="MyApp">
        <trans-unit id="Table 1 T - Caption" size-unit="char" translate="yes" xml:space="preserve">
          <source xml:space="preserve"></source>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        let unit = parsed
            .get("Table 1 T - Caption")
            .expect("trans-unit with empty source must survive parsing");
        assert_eq!(unit.source, "", "empty source body should be preserved");
    }

    fn make_test_unit(source: &str) -> TranslationUnit {
        TranslationUnit {
            id: "test-id".to_string(),
            object_type: "Table".to_string(),
            object_id: 50100,
            object_name: "Test".to_string(),
            source: source.to_string(),
            target: None,
            state: TranslationState::New,
            note: None,
        }
    }

    #[test]
    fn test_suggest_translations_empty() {
        let ws = al_workspace::Workspace::new();
        let unit = make_test_unit("Customer");
        let result = suggest_translations(&[&unit], &ws);
        assert!(
            result.is_empty(),
            "empty workspace should produce no suggestions"
        );
    }

    #[test]
    fn test_suggest_translations_exact_match() {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "TestPkg".to_string(),
            ..Default::default()
        }]);
        let unit = make_test_unit("Customer");
        let result = suggest_translations(&[&unit], &ws);
        assert!(!result.is_empty(), "should find exact match suggestion");
        assert_eq!(
            result[0].confidence, 1.0,
            "exact match should have confidence 1.0"
        );
        assert_eq!(
            result[0].origin,
            SuggestionOrigin::Name,
            "symbol-name path must be tagged as a name suggestion"
        );
    }

    /// Build an already-translated unit for translation-memory tests.
    fn translated_unit(source: &str, target: &str) -> TranslationUnit {
        TranslationUnit {
            id: format!("Table 50100 Test - Caption {source}"),
            object_type: "Table".to_string(),
            object_id: 50100,
            object_name: "Test".to_string(),
            source: source.to_string(),
            target: Some(target.to_string()),
            state: TranslationState::Translated,
            note: None,
        }
    }

    #[test]
    fn tm_exact_match_suggests_existing_translation() {
        // Translated "Customer" -> "Kunde" should be reused verbatim for an
        // untranslated "Customer", even with an empty symbol workspace.
        let ws = al_workspace::Workspace::new();
        let translated = translated_unit("Customer", "Kunde");
        let unit = make_test_unit("Customer");
        let result = suggest_translations_with_memory(&[&unit], &[&translated], &ws);
        assert_eq!(result.len(), 1, "exactly one TM suggestion expected");
        assert_eq!(result[0].suggested_translation, "Kunde");
        assert_eq!(result[0].origin, SuggestionOrigin::TmExact);
        assert_eq!(result[0].confidence, 1.0);
    }

    #[test]
    fn tm_exact_match_is_case_and_whitespace_insensitive() {
        // Normalized source matching: "  customer " matches "Customer".
        let ws = al_workspace::Workspace::new();
        let translated = translated_unit("Customer", "Kunde");
        let unit = make_test_unit("  customer ");
        let result = suggest_translations_with_memory(&[&unit], &[&translated], &ws);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].suggested_translation, "Kunde");
        assert_eq!(result[0].origin, SuggestionOrigin::TmExact);
    }

    #[test]
    fn tm_fuzzy_match_suggests_near_translation() {
        // "Post Sales Document" -> "Post Sales Documents": tokens overlap 2/3
        // (Dice 0.667) — a fuzzy hit below an exact match's confidence.
        let ws = al_workspace::Workspace::new();
        let translated = translated_unit("Post Sales Document", "Verkaufsbeleg buchen");
        let unit = make_test_unit("Post Sales Documents");
        let result = suggest_translations_with_memory(&[&unit], &[&translated], &ws);
        assert_eq!(result.len(), 1, "near-match should produce a fuzzy hit");
        assert_eq!(result[0].suggested_translation, "Verkaufsbeleg buchen");
        assert_eq!(result[0].origin, SuggestionOrigin::TmFuzzy);
        assert!(
            result[0].confidence > 0.0 && result[0].confidence < 1.0,
            "fuzzy confidence must be lower than an exact match, got {}",
            result[0].confidence
        );
    }

    #[test]
    fn tm_falls_back_to_name_match_when_no_tm_hit() {
        // No usable TM entry for "Customer" (memory holds an unrelated pair),
        // so the symbol-name fallback fires and is tagged `name`.
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "TestPkg".to_string(),
            ..Default::default()
        }]);
        let translated = translated_unit("Vendor Ledger Entry", "Kreditorenposten");
        let unit = make_test_unit("Customer");
        let result = suggest_translations_with_memory(&[&unit], &[&translated], &ws);
        assert!(
            !result.is_empty(),
            "name fallback should produce a suggestion"
        );
        assert_eq!(result[0].origin, SuggestionOrigin::Name);
        assert_eq!(result[0].suggested_translation, "Customer");
        assert_eq!(result[0].confidence, 1.0);
    }

    #[test]
    fn tm_no_match_yields_nothing_when_workspace_empty() {
        // Unrelated source + empty symbol index: TM misses and the name
        // fallback finds nothing, so there is no suggestion at all.
        let ws = al_workspace::Workspace::new();
        let translated = translated_unit("Customer", "Kunde");
        let unit = make_test_unit("Totally Unrelated Phrase");
        let result = suggest_translations_with_memory(&[&unit], &[&translated], &ws);
        assert!(
            result.is_empty(),
            "no TM or name match should yield nothing"
        );
    }

    #[test]
    fn tm_ignores_unfinished_translations() {
        // A unit with a target but state `New`/`NeedsReviewTranslation` is not
        // a trustworthy translation and must not be indexed into the TM.
        let ws = al_workspace::Workspace::new();
        let mut not_done = translated_unit("Customer", "Kunde");
        not_done.state = TranslationState::New;
        let mut empty_target = translated_unit("Customer", "Kunde");
        empty_target.target = Some("   ".to_string());
        let unit = make_test_unit("Customer");
        let result = suggest_translations_with_memory(&[&unit], &[&not_done, &empty_target], &ws);
        assert!(
            result.is_empty(),
            "unfinished/empty translations must not seed the TM"
        );
    }

    #[test]
    fn parse_xliff_preserves_multi_line_source_body() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2">
  <file>
    <body>
      <group>
        <trans-unit id="multiline.1">
          <source>Line one
Line two
Line three</source>
          <target state="new"/>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        let unit = parsed.get("multiline.1").expect("unit should parse");
        assert_eq!(unit.source, "Line one\nLine two\nLine three");
    }

    #[test]
    fn parse_xliff_preserves_multi_line_target_body() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2">
  <file>
    <body>
      <group>
        <trans-unit id="multiline.2">
          <source>Hello</source>
          <target state="translated">Bonjour
le monde</target>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        let unit = parsed.get("multiline.2").expect("unit should parse");
        assert_eq!(unit.target.as_deref(), Some("Bonjour\nle monde"));
        assert_eq!(unit.state, TranslationState::Translated);
    }

    #[test]
    fn xlf_exceeds_cap_small_file_returns_some_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.xlf");
        std::fs::write(&path, b"<xliff/>").unwrap();
        assert_eq!(xlf_exceeds_cap(&path), Some(false));
    }

    #[test]
    fn xlf_exceeds_cap_huge_file_returns_some_true() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.xlf");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(100 * 1024 * 1024).unwrap();
        drop(f);
        assert_eq!(xlf_exceeds_cap(&path), Some(true));
    }

    #[test]
    fn xlf_exceeds_cap_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(xlf_exceeds_cap(&dir.path().join("nope.xlf")), None);
    }

    #[test]
    fn parse_xliff_single_line_body_still_works() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2">
  <file>
    <body>
      <group>
        <trans-unit id="single">
          <source>Hello</source>
          <target state="needs-review-translation">Bonjour</target>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        let unit = parsed.get("single").expect("unit should parse");
        assert_eq!(unit.source, "Hello");
        assert_eq!(unit.target.as_deref(), Some("Bonjour"));
    }

    #[test]
    fn parse_xliff_preserves_ms_format_note_with_attributes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2">
  <file>
    <body>
      <group>
        <trans-unit id="attr" size-unit="char" translate="yes" xml:space="preserve">
          <source>Customer Name</source>
          <target state="translated">Kundenname</target>
          <note from="Developer" annotates="general" priority="2">Shown on the card</note>
        </trans-unit>
        <trans-unit id="bare">
          <source>Hello</source>
          <note>plain note</note>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        let attr = parsed.get("attr").expect("attributed unit should parse");
        assert_eq!(
            attr.note.as_deref(),
            Some("Shown on the card"),
            "attributed <note …> must be captured, not dropped"
        );
        let bare = parsed.get("bare").expect("bare unit should parse");
        assert_eq!(bare.note.as_deref(), Some("plain note"));
    }
}
