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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single translatable text unit extracted from AL source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationUnit {
    /// Unique ID following MS AL format: `ObjectType ObjectId - PropertyName FieldId - PropertyType`
    pub id: String,
    /// Object type (e.g. "Table", "Page", "Codeunit")
    pub object_type: String,
    /// Object ID
    pub object_id: u32,
    /// Object name
    pub object_name: String,
    /// Source text (English caption/tooltip/label value)
    pub source: String,
    /// Translated text (if available — None means untranslated)
    pub target: Option<String>,
    /// Translation state
    pub state: TranslationState,
    /// Note (context from AL property name and field)
    pub note: Option<String>,
}

/// Translation state following XLIFF 1.2 conventions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TranslationState {
    /// Not yet translated
    New,
    /// Translated and ready
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

// ---------------------------------------------------------------------------
// Extraction from AL source
// ---------------------------------------------------------------------------

/// Extract all translatable text units from the workspace AL files.
///
/// Scans for `Caption`, `ToolTip`, and `Label` properties in all workspace .al files.
/// Returns units in the order they appear (deterministic output).
pub fn extract_translation_units(workspace: &Workspace) -> Vec<TranslationUnit> {
    let mut units = Vec::new();

    // Iterate all indexed .al files
    let mut paths: Vec<_> = workspace.file_index.files.iter()
        .map(|e| e.key().clone())
        .collect();
    paths.sort(); // deterministic order

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
    // Detect object declaration (type, id, name) from the first line matching pattern
    let (obj_type, obj_id, obj_name) = match detect_object_header(text) {
        Some(v) => v,
        None => return,
    };

    // Scan for Caption, ToolTip, Label assignments
    let mut field_id: u32 = 0;
    let mut current_field: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        // Track field declarations to associate captions with fields
        if let Some(fid) = parse_field_declaration(trimmed) {
            field_id = fid;
            current_field = parse_field_name(trimmed);
        }

        // Caption = 'text';
        if let Some(caption) = parse_property_value(trimmed, "Caption") {
            let context = current_field.as_deref().unwrap_or(&obj_name);
            let id = make_translation_id(&obj_type, obj_id, &obj_name, "Caption", field_id, context, path);
            units.push(TranslationUnit {
                id,
                object_type: obj_type.clone(),
                object_id: obj_id,
                object_name: obj_name.clone(),
                source: caption,
                target: None,
                state: TranslationState::New,
                note: current_field.as_ref().map(|f| format!("Caption for {}", f)),
            });
        }

        // ToolTip = 'text';
        if let Some(tooltip) = parse_property_value(trimmed, "ToolTip") {
            let context = current_field.as_deref().unwrap_or(&obj_name);
            let id = make_translation_id(&obj_type, obj_id, &obj_name, "ToolTip", field_id, context, path);
            units.push(TranslationUnit {
                id,
                object_type: obj_type.clone(),
                object_id: obj_id,
                object_name: obj_name.clone(),
                source: tooltip,
                target: None,
                state: TranslationState::New,
                note: current_field.as_ref().map(|f| format!("ToolTip for {}", f)),
            });
        }

        // Label 'varname': 'text'  or   MyLabel: Label 'text';
        if let Some(label_text) = parse_label_declaration(trimmed) {
            let id = make_label_id(&obj_type, obj_id, &obj_name, field_id, path, units.len());
            units.push(TranslationUnit {
                id,
                object_type: obj_type.clone(),
                object_id: obj_id,
                object_name: obj_name.clone(),
                source: label_text,
                target: None,
                state: TranslationState::New,
                note: Some("Label".to_string()),
            });
        }
    }
}

/// Detect the first AL object declaration line: (type, id, name).
fn detect_object_header(text: &str) -> Option<(String, u32, String)> {
    // Extension types MUST appear before their base types so that `starts_with` doesn't
    // falsely match "tableextension" as "table", "pageextension" as "page", etc.
    let object_types = [
        "tableextension", "pageextension", "reportextension", "enumextension",
        "permissionsetextension", "profileextension",
        "table", "page", "codeunit", "report", "query", "xmlport",
        "enum", "interface", "permissionset", "profile",
    ];
    for line in text.lines().take(10) {
        let lower = line.trim().to_lowercase();
        for ot in &object_types {
            // Require a word boundary after the keyword (space, tab, or digit) to avoid
            // false prefix matches like "pagepart" matching "page".
            if let Some(after) = lower.strip_prefix(ot) {
                let boundary = after.starts_with(|c: char| c.is_ascii_whitespace() || c.is_ascii_digit());
                if !boundary {
                    continue;
                }
                // e.g. "table 50100 \"My Table\""
                let rest = line.trim()[ot.len()..].trim();
                let (id_str, rest2) = split_id_and_name(rest);
                let id: u32 = id_str.parse().unwrap_or(0);
                let name = rest2.trim().trim_matches('"').trim_matches('\'').to_string();
                if !name.is_empty() || id > 0 {
                    return Some((capitalize(ot), id, name));
                }
            } // end if let Some(after)
        }
    }
    None
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
/// Returns the field ID if present.
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

/// Parse the field name from a field declaration line.
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
    // Extract the single-quoted string value
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

/// Extract the first single-quoted string from the input.
fn extract_single_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let inner = &s[start + 1..];
    // Find closing quote (handle escaped '' as single quote)
    let mut result = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek() == Some(&'\'') {
                // Escaped quote
                chars.next();
                result.push('\'');
            } else {
                break;
            }
        } else {
            result.push(ch);
        }
    }
    if result.is_empty() { None } else { Some(result) }
}

/// Build a deterministic translation unit ID.
fn make_translation_id(
    obj_type: &str, obj_id: u32, obj_name: &str,
    property: &str, field_id: u32, context: &str,
    _path: &Path,
) -> String {
    if field_id > 0 {
        format!("{} {} {} - {} {} - {}", obj_type, obj_id, obj_name, property, field_id, context)
    } else {
        format!("{} {} {} - {}", obj_type, obj_id, obj_name, property)
    }
}

/// Build an ID for a Label variable.
fn make_label_id(
    obj_type: &str, obj_id: u32, obj_name: &str,
    _field_id: u32, _path: &Path, index: usize,
) -> String {
    format!("{} {} {} - Label {}", obj_type, obj_id, obj_name, index)
}

// ---------------------------------------------------------------------------
// XLIFF generation
// ---------------------------------------------------------------------------

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
        xml_escape(source_language), xml_escape(target_language)
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
            xml.push_str(&format!(
                "          <note>{}</note>\n",
                xml_escape(note)
            ));
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

// ---------------------------------------------------------------------------
// XLIFF parsing
// ---------------------------------------------------------------------------

/// Parse an XLIFF 1.2 file into a map of `id → TranslationUnit`.
///
/// Uses a simple line-based parser that handles typical AL XLIFF output without
/// requiring a full XML parser dependency.
pub fn parse_xliff(content: &str) -> HashMap<String, TranslationUnit> {
    let mut units: HashMap<String, TranslationUnit> = HashMap::new();
    let mut current_id: Option<String> = None;
    let mut current_source: Option<String> = None;
    let mut current_target: Option<String> = None;
    let mut current_state = TranslationState::New;
    let mut current_note: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("<trans-unit ") {
            current_id = extract_xml_attr(trimmed, "id");
            current_source = None;
            current_target = None;
            current_state = TranslationState::New;
            current_note = None;
        } else if trimmed.starts_with("<source") {
            current_source = extract_xml_text(trimmed);
        } else if trimmed.starts_with("<target") {
            let state_str = extract_xml_attr(trimmed, "state").unwrap_or_default();
            current_state = TranslationState::from_xliff_state(&state_str);
            current_target = extract_xml_text(trimmed);
        } else if trimmed.starts_with("<note>") {
            current_note = extract_xml_text(trimmed);
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

/// Extract text content between XML tags (handles `<tag>text</tag>` on one line).
fn extract_xml_text(tag: &str) -> Option<String> {
    // Find > ... </
    let content_start = tag.find('>')? + 1;
    let content = &tag[content_start..];
    if content.starts_with('/') || content.is_empty() {
        // Self-closing or no content
        return None;
    }
    let content_end = content.find('<').unwrap_or(content.len());
    let raw = &content[..content_end];
    if raw.is_empty() { None } else { Some(xml_unescape(raw)) }
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

// ---------------------------------------------------------------------------
// XLIFF refresh
// ---------------------------------------------------------------------------

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

    // Build a set of generated IDs for detecting obsolete units
    let generated_ids: std::collections::HashSet<&str> =
        generated.iter().map(|u| u.id.as_str()).collect();

    // Process each unit from the generated file
    for gen_unit in generated {
        if let Some(lang_unit) = language.get(&gen_unit.id) {
            if lang_unit.source != gen_unit.source {
                // Source changed — mark for review
                result_units.push(TranslationUnit {
                    source: gen_unit.source.clone(),
                    target: lang_unit.target.clone(),
                    state: TranslationState::NeedsReviewTranslation,
                    ..lang_unit.clone()
                });
                refresh.changed.push(gen_unit.id.clone());
            } else {
                // Unchanged — preserve existing translation
                result_units.push(lang_unit.clone());
                refresh.preserved += 1;
            }
        } else {
            // New unit
            result_units.push(TranslationUnit {
                state: TranslationState::New,
                ..gen_unit.clone()
            });
            refresh.added.push(gen_unit.id.clone());
        }
    }

    // Collect obsolete units (in language but not in generated)
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

// ---------------------------------------------------------------------------
// Find untranslated
// ---------------------------------------------------------------------------

/// Find all translation units that have no target translation.
///
/// Returns units where `target` is `None` or empty, sorted by object type and ID.
pub fn find_untranslated(units: &[TranslationUnit]) -> Vec<&TranslationUnit> {
    units.iter()
        .filter(|u| u.target.as_deref().map(|t| t.trim().is_empty()).unwrap_or(true))
        .filter(|u| u.state != TranslationState::Final)
        .collect()
}

// ---------------------------------------------------------------------------
// Translation suggestions from base app symbols
// ---------------------------------------------------------------------------

/// A translation suggestion from the base app symbol data.
#[derive(Debug, Clone, Serialize)]
pub struct TranslationSuggestion {
    pub unit_id: String,
    pub source: String,
    pub suggested_translation: String,
    pub confidence: f32,
    pub source_object: String,
}

/// Suggest translations for untranslated units by matching against base app symbols.
///
/// Compares captions and tooltips in workspace symbols against untranslated source texts.
/// Returns suggestions sorted by confidence (highest first).
pub fn suggest_translations(
    untranslated: &[&TranslationUnit],
    workspace: &Workspace,
) -> Vec<TranslationSuggestion> {
    let mut suggestions = Vec::new();

    for unit in untranslated {
        // Search symbols for matching captions/tooltips
        let source_lower = unit.source.to_lowercase();

        // Search in symbol index by caption similarity
        let matches = workspace.symbols.search(&unit.source, 5);
        for entry in &matches {
            // Check object name caption match
            let entry_name_lower = entry.name.to_lowercase();
            if entry_name_lower == source_lower {
                // Exact match — confidence 1.0
                suggestions.push(TranslationSuggestion {
                    unit_id: unit.id.clone(),
                    source: unit.source.clone(),
                    suggested_translation: entry.name.clone(),
                    confidence: 1.0,
                    source_object: format!("{:?} {}", entry.kind, entry.name),
                });
                continue;
            }

            // Check field captions
            for field in &entry.fields {
                if field.name.to_lowercase() == source_lower {
                    suggestions.push(TranslationSuggestion {
                        unit_id: unit.id.clone(),
                        source: unit.source.clone(),
                        suggested_translation: field.name.clone(),
                        confidence: 0.9,
                        source_object: format!("{:?} {} - Field {}", entry.kind, entry.name, field.name),
                    });
                }
            }
        }
    }

    // Sort by confidence descending
    suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    suggestions
}

// ---------------------------------------------------------------------------
// Build integration
// ---------------------------------------------------------------------------

/// Generate the `.g.xlf` file for a workspace and write it to the Translations directory.
///
/// Creates `<project_root>/Translations/<AppName>.g.xlf`.
/// Returns the path to the generated file and the number of units generated.
pub fn build_xliff(workspace: &Workspace, project_root: &Path) -> Option<(PathBuf, usize)> {
    // Read app name from app.json
    let app_name = read_app_name(project_root).unwrap_or_else(|| "App".to_string());

    let units = extract_translation_units(workspace);
    if units.is_empty() {
        return None;
    }

    let xlf_content = generate_xliff(&app_name, "en-US", "en-US", &units);

    let translations_dir = project_root.join("Translations");
    std::fs::create_dir_all(&translations_dir).ok()?;

    let xlf_path = translations_dir.join(format!("{}.g.xlf", app_name));
    std::fs::write(&xlf_path, xlf_content).ok()?;

    Some((xlf_path, units.len()))
}

fn read_app_name(project_root: &Path) -> Option<String> {
    let bytes = std::fs::read(project_root.join("app.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("name")?.as_str().map(|s| s.replace([' ', '"', '\''], ""))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_single_quoted() {
        assert_eq!(extract_single_quoted("'Hello world'"), Some("Hello world".to_string()));
        assert_eq!(extract_single_quoted("'It''s a test'"), Some("It's a test".to_string()));
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

        // Parse back
        let parsed = parse_xliff(&xml);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains_key("Table 50100 MyTable - Caption"));
        assert_eq!(
            parsed["Table 50100 MyTable - Caption"].source,
            "My Table"
        );
        let translated = &parsed["Table 50100 MyTable - ToolTip 10 Name"];
        assert_eq!(translated.target.as_deref(), Some("Gibt den Namen an"));
        assert_eq!(translated.state, TranslationState::Translated);
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
        existing.insert("T2".to_string(), TranslationUnit {
            id: "T2".to_string(),
            object_type: "Table".to_string(),
            object_id: 1,
            object_name: "T".to_string(),
            source: "World".to_string(),  // old source
            target: Some("Welt".to_string()),
            state: TranslationState::Translated,
            note: None,
        });
        existing.insert("T_OLD".to_string(), TranslationUnit {
            id: "T_OLD".to_string(),
            object_type: "Table".to_string(),
            object_id: 1,
            object_name: "T".to_string(),
            source: "Obsolete".to_string(),
            target: Some("Veraltet".to_string()),
            state: TranslationState::Translated,
            note: None,
        });

        let (updated, result) = refresh_xliff(&generated, &existing);

        // T1 should be added
        assert!(result.added.contains(&"T1".to_string()));
        // T2 source changed — should be marked needs-review
        assert!(result.changed.contains(&"T2".to_string()));
        // T_OLD should be marked removed
        assert!(result.removed.contains(&"T_OLD".to_string()));
        // Translation for T2 should be preserved
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
        assert!(units.iter().any(|u| u.source == "Name"), "Should extract Caption 'Name'");
        assert!(units.iter().any(|u| u.source == "Specifies the name of the record."), "Should extract ToolTip");
        assert!(units.iter().any(|u| u.source == "This is a label"), "Should extract Label");
        assert!(units.iter().any(|u| u.source == "Description"), "Should extract Caption 'Description'");
    }
}
