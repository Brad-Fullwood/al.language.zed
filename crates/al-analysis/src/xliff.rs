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
//!
//! IDs follow Microsoft's `GetLanguageSymbolId` scheme, which is what `alc`
//! writes into a `.g.xlf`: every name component is an FNV-1 hash (over the
//! name's UTF-16LE bytes) biased by `i32::MAX`, e.g.
//!
//! ```text
//! Table 3625681466 - Field 2879900210 - Property 2879900210
//! Page  3625681466 - Control 2718011747 - Property 1295455071
//! Codeunit 1535166296 - NamedType 3010734695
//! ```
//!
//! The hash is the same one `crates/al-emit/src/assemble.rs` implements and
//! verifies against `alc`, so `xlf refresh` matches IDs in an `alc`- or
//! Microsoft-produced translation file.
//!
//! **Known deviation:** `alc` folds an extension object's id-root onto the base
//! object when that base is part of the same project (emitting an
//! `al-object-target` attribute). This extractor works one file at a time and
//! has no project view, so it keeps the *declaring* object as the id root —
//! which is what `alc` also does for the dominant case of extending a
//! base-application object.

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

    // A `.g.xlf` with duplicate `trans-unit id=` values silently collapses to
    // one entry in `parse_xliff`'s map, so refresh/untranslated/suggest would
    // operate on corrupted data. Keep the first occurrence and warn.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    units.retain(|unit| {
        if seen.insert(unit.id.clone()) {
            return true;
        }
        tracing::warn!(
            id = %unit.id,
            source = %unit.source,
            "xliff: dropping duplicate translation-unit id"
        );
        false
    });

    units
}

/// Extract translation units from a single AL file.
///
/// The scan is structural: brace nesting decides which member (table field,
/// page control, page action) a `Caption`/`ToolTip` belongs to. The previous
/// line-oriented scan tracked only a numeric `field_id` that page-layout
/// members never set and that was never reset when a field block ended, so
/// every page control produced the *same* trans-unit id and object-level
/// properties were attributed to the last field seen.
fn extract_from_file(path: &Path, text: &str, units: &mut Vec<TranslationUnit>) {
    let _ = path;
    let Some(header) = detect_object_header(text) else {
        return;
    };
    let obj_type = header.kind_display.clone();
    let obj_id = header.id;
    let obj_name = header.name.clone();
    let object_hash = name_hash(&obj_name);

    // One entry per open brace; `Some(member)` for a named member block.
    let mut stack: Vec<Option<MemberBlock>> = Vec::new();
    let mut pending: Option<MemberBlock> = None;

    for line in text.lines() {
        let code = strip_literals_for_structure(line);
        let trimmed_code = code.trim();
        if let Some(member) = parse_member_block(trimmed_code) {
            pending = Some(member);
        }

        let trimmed = line.trim();
        let anchor = stack.iter().rev().flatten().next();

        for property in ["Caption", "ToolTip"] {
            let Some(value) = parse_property_value(trimmed, property) else {
                continue;
            };
            if property_is_locked(trimmed) {
                continue;
            }
            let (id, note) = match anchor {
                Some(member) => (
                    format!(
                        "{obj_type} {object_hash} - {} {} - Property {}",
                        member.id_kind(&header),
                        name_hash(&member.name),
                        name_hash(property)
                    ),
                    format!(
                        "{obj_type} {obj_name} - {} {} - Property {property}",
                        member.id_kind(&header),
                        member.name
                    ),
                ),
                None => (
                    format!(
                        "{obj_type} {object_hash} - Property {}",
                        name_hash(property)
                    ),
                    format!("{obj_type} {obj_name} - Property {property}"),
                ),
            };
            units.push(make_translation_unit(
                id,
                &obj_type,
                obj_id,
                &obj_name,
                value,
                Some(note),
            ));
        }

        // `MyLabel: Label 'text';` — alc keys labels by the NamedType name.
        if let Some((label_name, label_text)) = parse_label_declaration(trimmed) {
            if !property_is_locked(trimmed) {
                let id = format!(
                    "{obj_type} {object_hash} - NamedType {}",
                    name_hash(&label_name)
                );
                units.push(make_translation_unit(
                    id,
                    &obj_type,
                    obj_id,
                    &obj_name,
                    label_text,
                    Some(format!("{obj_type} {obj_name} - NamedType {label_name}")),
                ));
            }
        }

        for ch in code.chars() {
            match ch {
                '{' => stack.push(pending.take()),
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }
}

/// A named member block (`field(…)`, `action(…)`, `group(…)`, …) whose
/// properties are translated relative to it.
#[derive(Debug, Clone)]
struct MemberBlock {
    /// Lower-cased declaration keyword.
    keyword: String,
    /// Member name used in the translation id.
    name: String,
    /// Whether the member sits inside an `actions` section.
    in_actions: bool,
}

impl MemberBlock {
    /// alc's id component for this member: `Field` for a table field,
    /// `Action` for anything under `actions`, `Control` otherwise.
    fn id_kind(&self, header: &ObjectHeader) -> &'static str {
        if self.in_actions || self.keyword == "action" || self.keyword == "actionref" {
            "Action"
        } else if header.is_table_like && self.keyword == "field" {
            "Field"
        } else {
            "Control"
        }
    }
}

/// Declaration keywords that open a *named* member block.
const MEMBER_BLOCK_KEYWORDS: &[&str] = &[
    "field",
    "action",
    "actionref",
    "group",
    "part",
    "systempart",
    "usercontrol",
    "label",
    "repeater",
    "cuegroup",
    "fixed",
    "grid",
    "dataitem",
    "column",
];

/// Parse a `keyword(args)` member header, or the bare `actions` section.
fn parse_member_block(trimmed: &str) -> Option<MemberBlock> {
    let lower_head = trimmed
        .split(|c: char| c == '(' || c.is_whitespace() || c == '{')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if lower_head == "actions" {
        // Marks the section; unnamed, so it never anchors a property itself.
        return Some(MemberBlock {
            keyword: "actions".to_string(),
            name: String::new(),
            in_actions: true,
        });
    }
    let open = trimmed.find('(')?;
    let keyword = trimmed[..open].trim().to_lowercase();
    if !MEMBER_BLOCK_KEYWORDS.contains(&keyword.as_str()) {
        return None;
    }
    let close = trimmed.rfind(')')?;
    if close < open {
        return None;
    }
    let args: Vec<&str> = trimmed[open + 1..close].split(';').collect();
    // `field(1; Name; Text[50])` (table) vs `field(Name; Rec.Name)` (page):
    // the name is the second segment only when the first is a numeric id.
    let first = args.first().map(|a| a.trim()).unwrap_or("");
    let name_part = if first.parse::<u32>().is_ok() && args.len() >= 2 {
        args[1].trim()
    } else {
        first
    };
    let name = name_part.trim_matches('"').trim_matches('\'').trim();
    if name.is_empty() {
        return None;
    }
    Some(MemberBlock {
        keyword,
        name: name.to_string(),
        in_actions: false,
    })
}

/// Blank out string-literal contents so braces inside AL captions do not
/// corrupt the nesting count. Quotes themselves are kept so token shape is
/// unchanged.
fn strip_literals_for_structure(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some((_, ch)) = chars.next() {
        if !in_single && !in_double && ch == '/' && chars.peek().is_some_and(|(_, n)| *n == '/') {
            break;
        }
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
            }
            _ if in_single => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Whether a `Caption`/`ToolTip`/`Label` declaration carries `Locked = true`.
///
/// Microsoft's AL excludes locked strings from the generated translation file;
/// emitting them made non-translatable text look translatable.
fn property_is_locked(line: &str) -> bool {
    // Only the part *after* the (single-quoted) value can hold the modifier.
    let Some(quote) = line.find('\'') else {
        return false;
    };
    let mut rest = &line[quote..];
    // Skip the literal, honouring the doubled-quote escape.
    let bytes = rest.as_bytes();
    let mut index = 1usize;
    while index < rest.len() {
        if bytes[index] == b'\'' {
            if bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            index += 1;
            break;
        }
        index += 1;
    }
    rest = &rest[index.min(rest.len())..];
    let lower = rest.to_lowercase();
    let Some(position) = lower.find("locked") else {
        return false;
    };
    let after = lower[position + "locked".len()..].trim_start();
    match after.strip_prefix('=') {
        // `Locked = true`; the value may be followed by `;` or `,`.
        Some(value) => {
            let value = value.trim_start();
            let word: String = value
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            word == "true"
        }
        // `Locked` on its own is shorthand for `Locked = true`.
        None => after.is_empty() || after.starts_with(';') || after.starts_with(','),
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

/// FNV-1 hash over `s`'s UTF-16LE bytes, biased by `i32::MAX` — Microsoft's
/// `Hash.GetFNVHashCode(string)` as used by `GetLanguageSymbolId`.
///
/// This is the same algorithm `crates/al-emit/src/method_id.rs` implements and
/// verifies against `alc`; it is copied rather than shared so `al-analysis`
/// does not have to depend on the emitter.
fn name_hash(s: &str) -> i64 {
    const FNV_OFFSET_BIAS: i32 = -2128831035;
    const FNV_PRIME: i32 = 16777619;
    let mut hash = FNV_OFFSET_BIAS;
    for unit in s.encode_utf16() {
        for byte in unit.to_le_bytes() {
            hash = (hash ^ byte as i32).wrapping_mul(FNV_PRIME);
        }
    }
    hash as i64 + 2_147_483_647
}

/// The first AL object declaration in a file.
#[derive(Debug, Clone)]
struct ObjectHeader {
    /// Canonical object-kind spelling used in translation ids (`TableExtension`).
    kind_display: String,
    id: u32,
    name: String,
    is_table_like: bool,
}

/// Detect the AL object declaration: (type, id, name).
///
/// Leading blank lines, `//` and `/* */` comments, and `namespace`/`using`
/// directives are skipped, then the first meaningful line must be the
/// declaration. The previous implementation only looked at the first 10 lines,
/// so any file with a longer licence header was silently skipped by XLIFF
/// extraction — no units, no warning.
fn detect_object_header(text: &str) -> Option<ObjectHeader> {
    let mut in_block_comment = false;
    for raw in text.lines() {
        let mut line = raw.trim().to_string();
        if in_block_comment {
            match line.find("*/") {
                Some(end) => {
                    in_block_comment = false;
                    line = line[end + 2..].trim().to_string();
                }
                None => continue,
            }
        }
        while let Some(start) = line.find("/*") {
            match line[start + 2..].find("*/") {
                Some(end) => {
                    let after = start + 2 + end + 2;
                    line = format!("{} {}", &line[..start], &line[after..])
                        .trim()
                        .to_string();
                }
                None => {
                    in_block_comment = true;
                    line = line[..start].trim().to_string();
                    break;
                }
            }
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("namespace ") || lower.starts_with("using ") {
            continue;
        }

        // Sort object type keywords by length descending so longer keywords
        // (extensions) win over their shorter base-type prefixes.
        let mut sorted_types: Vec<&str> = al_syntax::language_data::object_types()
            .iter()
            .map(|ot| ot.keyword.as_str())
            .collect();
        sorted_types.sort_by_key(|k| Reverse(k.len()));

        for ot in &sorted_types {
            let Some(after) = lower.strip_prefix(ot) else {
                continue;
            };
            // Require a word boundary after the keyword.
            if !after.starts_with(|c: char| c.is_ascii_whitespace() || c.is_ascii_digit()) {
                continue;
            }
            let rest = line[ot.len()..].trim();
            let (id_str, rest2) = split_id_and_name(rest);
            let id: u32 = id_str.parse().unwrap_or(0);
            let name = parse_object_name(rest2.trim());
            if name.is_empty() && id == 0 {
                continue;
            }
            let kind = ot.parse::<al_symbols::ObjectKind>().ok();
            let kind_display = kind
                .map(|k| k.to_string())
                .unwrap_or_else(|| capitalize(ot));
            let is_table_like = matches!(
                kind,
                Some(al_symbols::ObjectKind::Table) | Some(al_symbols::ObjectKind::TableExtension)
            );
            return Some(ObjectHeader {
                kind_display,
                id,
                name,
                is_table_like,
            });
        }
        // The first meaningful line is not an object declaration.
        return None;
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

/// Parse a `Label` variable declaration like `MyLabel: Label 'Some text';`,
/// returning `(label_name, text)`.
fn parse_label_declaration(line: &str) -> Option<(String, String)> {
    // Pattern: <name>: Label '<text>' [, ...];
    let colon = line.find(':')?;
    let name = line[..colon].trim().trim_matches('"').trim();
    if name.is_empty() {
        return None;
    }
    let after_colon = line[colon + 1..].trim();
    let lower = after_colon.to_lowercase();
    if !lower.starts_with("label ") {
        return None;
    }
    let after_label = after_colon[6..].trim();
    let text = extract_single_quoted(after_label)?;
    Some((name.to_string(), text))
}

/// Body of the first single-quoted AL literal in `s`, with `''` unescaped.
///
/// An *empty* literal (`Caption = '';`) is a legal AL construct that suppresses
/// the default caption, and alc emits an empty-source unit for it — returning
/// `None` silently dropped it. `None` now means "no literal here at all".
fn extract_single_quoted(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let inner = &s[start + 1..];
    let mut result = String::new();
    let mut chars = inner.chars().peekable();
    let mut closed = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                result.push('\'');
            } else {
                closed = true;
                break;
            }
        } else {
            result.push(ch);
        }
    }
    if closed {
        Some(result)
    } else {
        // Unterminated literal — treat as no value rather than guessing.
        None
    }
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
                // Duplicate ids are malformed input. Keep the *first*
                // occurrence (deterministic and document-order) rather than
                // letting a later one silently overwrite an already-reviewed
                // translation, and make the condition observable.
                match units.entry(id) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(unit);
                    }
                    std::collections::hash_map::Entry::Occupied(existing) => {
                        tracing::warn!(
                            id = %existing.key(),
                            "xliff: duplicate trans-unit id in input; keeping the first occurrence"
                        );
                    }
                }
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
    let app_name = read_app_name(project_root)?;

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

fn read_app_name(project_root: &Path) -> std::io::Result<String> {
    let manifest = al_project::project::load_app_manifest(project_root)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let name = al_types::sanitize_filename_component(&manifest.name.replace([' ', '"', '\''], ""));
    if name == "_" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} must contain a non-empty application name",
                project_root.join("app.json").display()
            ),
        ));
    }
    Ok(name)
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
        // `Caption = '';` is legal AL that suppresses the default caption; alc
        // emits an empty-source unit for it, so an empty literal is a *value*.
        assert_eq!(extract_single_quoted("''"), Some(String::new()));
        assert_eq!(extract_single_quoted("no quotes"), None);
        // An unterminated literal is not a value.
        assert_eq!(extract_single_quoted("'oops"), None);
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
        assert_eq!(result.kind_display, "Table");
        assert_eq!(result.id, 50100);
        assert_eq!(result.name, "Customer Extension");
    }

    /// the name must stop at the closing quote — extension
    /// headers carry an `extends` clause that was being swallowed into the
    /// name (`Sales Order Pageext" extends "Sales Order`), mangling every
    /// xlf unit id for extension objects.
    #[test]
    fn detect_object_header_quoted_name_stops_before_extends_clause() {
        let text = r#"pageextension 50101 "Sales Order Pageext" extends "Sales Order""#;
        let header = detect_object_header(text).unwrap();
        assert_eq!(header.kind_display, "PageExtension");
        assert_eq!(header.id, 50101);
        assert_eq!(header.name, "Sales Order Pageext");
    }

    #[test]
    fn detect_object_header_unquoted_name_stops_before_extends_clause() {
        let text = "tableextension 50100 MyExt extends MyBase";
        let header = detect_object_header(text).unwrap();
        assert_eq!(header.kind_display, "TableExtension");
        assert_eq!(header.id, 50100);
        assert_eq!(header.name, "MyExt");
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
        // Labels are keyed by their NamedType name (alc's scheme), not by a
        // positional counter, so inserting a label above another does not
        // renumber every following id.
        assert_eq!(
            ids_a[0],
            format!(
                "Codeunit {} - NamedType {}",
                name_hash("My Codeunit"),
                name_hash("FirstLabel")
            )
        );
        assert_eq!(
            ids_a[1],
            format!(
                "Codeunit {} - NamedType {}",
                name_hash("My Codeunit"),
                name_hash("SecondLabel")
            )
        );
        assert_ne!(ids_a[0], ids_a[1]);
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

    #[test]
    fn app_name_errors_on_a_malformed_manifest_instead_of_using_a_fake_default() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("app.json"), b"{not json").unwrap();

        let error = read_app_name(project.path()).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("Invalid app.json"));
    }

    #[test]
    fn app_name_is_a_single_safe_filename_component() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("app.json"),
            br#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "../../Dangerous App",
                "publisher": "Publisher",
                "version": "1.0.0.0"
            }"#,
        )
        .unwrap();

        let name = read_app_name(project.path()).unwrap();

        assert_eq!(name, ".._.._DangerousApp");
        assert_eq!(
            std::path::Path::new(&name).components().count(),
            1,
            "manifest name must not create a nested output path"
        );
    }

    fn extract(al: &str) -> Vec<TranslationUnit> {
        let mut units = Vec::new();
        extract_from_file(Path::new("test.al"), al, &mut units);
        units
    }

    /// Every page control used to get the identical id
    /// (`Page 50100 X - Caption`) because page-layout `field(Name; Rec.Name)`
    /// never set the numeric `field_id`.
    #[test]
    fn page_controls_get_distinct_translation_ids() {
        let al = r#"page 50100 "My Page"
{
    Caption = 'My Page';
    SourceTable = "My Table";

    layout
    {
        area(Content)
        {
            field(Name; Rec.Name)
            {
                Caption = 'Name';
                ToolTip = 'Specifies the name.';
            }
            field(Amount; Rec.Amount)
            {
                Caption = 'Amount';
                ToolTip = 'Specifies the amount.';
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(DoIt)
            {
                Caption = 'Do It';
                ToolTip = 'Runs the thing.';
            }
        }
    }
}"#;
        let units = extract(al);
        let ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "every unit must have a distinct id: {ids:?}"
        );
        assert_eq!(units.len(), 7, "{ids:?}");

        let page_hash = name_hash("My Page");
        let caption_hash = name_hash("Caption");
        let tooltip_hash = name_hash("ToolTip");
        assert!(units.iter().any(|u| u.id
            == format!("Page {page_hash} - Property {caption_hash}")
            && u.source == "My Page"));
        assert!(units.iter().any(|u| u.id
            == format!(
                "Page {page_hash} - Control {} - Property {tooltip_hash}",
                name_hash("Name")
            )
            && u.source == "Specifies the name."));
        assert!(units.iter().any(|u| u.id
            == format!(
                "Page {page_hash} - Control {} - Property {caption_hash}",
                name_hash("Amount")
            )
            && u.source == "Amount"));
        // An action lives under `actions`, so alc keys it as `Action`.
        assert!(
            units.iter().any(|u| u.id
                == format!(
                    "Page {page_hash} - Action {} - Property {caption_hash}",
                    name_hash("DoIt")
                )),
            "{ids:?}"
        );
    }

    #[test]
    fn page_extension_controls_are_extracted() {
        let al = r#"pageextension 50101 "My Page Ext" extends "Customer Card"
{
    layout
    {
        addlast(General)
        {
            field(Loyalty; Rec.Loyalty)
            {
                Caption = 'Loyalty';
                ToolTip = 'Specifies the loyalty level.';
            }
        }
    }
}"#;
        let units = extract(al);
        assert_eq!(units.len(), 2, "{units:?}");
        let object_hash = name_hash("My Page Ext");
        assert!(units.iter().any(|u| u.id
            == format!(
                "PageExtension {object_hash} - Control {} - Property {}",
                name_hash("Loyalty"),
                name_hash("Caption")
            )));
        assert!(units
            .iter()
            .all(|u| u.object_type == "PageExtension" && u.object_name == "My Page Ext"));
    }

    /// `field_id`/`current_field` were never reset when a field block ended, so
    /// object-level properties after the last field were attributed to it.
    #[test]
    fn properties_after_the_last_field_are_not_attributed_to_it() {
        let al = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            Caption = 'Name';
        }
    }

    Caption = 'My Table';
}"#;
        let units = extract(al);
        let table_caption = units
            .iter()
            .find(|u| u.source == "My Table")
            .expect("object caption extracted");
        assert_eq!(
            table_caption.id,
            format!(
                "Table {} - Property {}",
                name_hash("My Table"),
                name_hash("Caption")
            ),
            "object caption must not carry the last field's id"
        );
        assert_eq!(
            table_caption.note.as_deref(),
            Some("Table My Table - Property Caption")
        );
    }

    /// Locked strings are not translatable and must stay out of the `.g.xlf`.
    #[test]
    fn locked_strings_are_excluded() {
        let al = r#"codeunit 50100 "My Codeunit"
{
    var
        TranslatableLbl: Label 'Please translate me';
        TechnicalLbl: Label 'SOME_TOKEN', Locked = true;
        AlsoLockedLbl: Label 'OTHER_TOKEN', Locked;
}"#;
        let units = extract(al);
        let sources: Vec<&str> = units.iter().map(|u| u.source.as_str()).collect();
        assert_eq!(sources, vec!["Please translate me"], "{units:?}");
    }

    #[test]
    fn locked_captions_and_tooltips_are_excluded() {
        let al = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Code; Code[20])
        {
            Caption = 'CODE', Locked = true;
        }
        field(2; Name; Text[100])
        {
            Caption = 'Name';
        }
    }
}"#;
        let units = extract(al);
        let sources: Vec<&str> = units.iter().map(|u| u.source.as_str()).collect();
        assert_eq!(sources, vec!["Name"], "{units:?}");
    }

    /// `Caption = '';` legally suppresses the default caption; alc emits an
    /// empty-source unit rather than dropping it.
    #[test]
    fn empty_caption_emits_an_empty_source_unit() {
        let al = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            Caption = '';
        }
    }
}"#;
        let units = extract(al);
        assert_eq!(units.len(), 1, "{units:?}");
        assert_eq!(units[0].source, "");
        assert_eq!(
            units[0].id,
            format!(
                "Table {} - Field {} - Property {}",
                name_hash("My Table"),
                name_hash("Name"),
                name_hash("Caption")
            )
        );
    }

    /// The header scan stopped after 10 lines, so a file with a longer licence
    /// banner produced no units and no warning.
    #[test]
    fn object_header_is_found_behind_a_long_comment_banner() {
        let mut al = String::new();
        for i in 0..40 {
            al.push_str(&format!("// licence header line {i}\n"));
        }
        al.push_str("/* a block\n   comment too */\n\n");
        al.push_str("namespace MyCompany.MyApp;\nusing Microsoft.Sales;\n\n");
        al.push_str("table 50100 \"My Table\"\n{\n    Caption = 'My Table';\n}\n");

        let units = extract(&al);
        assert_eq!(units.len(), 1, "{units:?}");
        assert_eq!(units[0].source, "My Table");
        assert_eq!(units[0].object_type, "Table");
    }

    #[test]
    fn header_detection_returns_none_when_the_file_has_no_object() {
        assert!(detect_object_header("// just a comment\n\n").is_none());
        assert!(detect_object_header("").is_none());
    }

    /// A `.g.xlf` with duplicate ids silently collapses in `parse_xliff`'s map;
    /// document order decides which entry survives.
    #[test]
    fn parse_xliff_keeps_the_first_of_two_duplicate_trans_unit_ids() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<xliff version="1.2">
  <file datatype="xml" source-language="en-US" target-language="de-DE" original="MyApp">
    <body>
      <group id="MyApp">
        <trans-unit id="Table 1 - Property 2" size-unit="char" translate="yes" xml:space="preserve">
          <source>First</source>
          <target state="translated">Erste</target>
        </trans-unit>
        <trans-unit id="Table 1 - Property 2" size-unit="char" translate="yes" xml:space="preserve">
          <source>Second</source>
          <target state="new"></target>
        </trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;
        let parsed = parse_xliff(xml);
        assert_eq!(parsed.len(), 1);
        let unit = parsed.get("Table 1 - Property 2").unwrap();
        assert_eq!(
            unit.source, "First",
            "a later duplicate must not overwrite an already-reviewed translation"
        );
        assert_eq!(unit.target.as_deref(), Some("Erste"));
    }

    /// Extraction across the workspace must never emit two units with the same id.
    #[test]
    fn workspace_extraction_drops_duplicate_ids() {
        let ws = Workspace::new();
        // Two files declaring the same object name produce the same id root.
        for (index, name) in ["/src/A.al", "/src/B.al"].iter().enumerate() {
            ws.file_index.add_file(
                std::path::PathBuf::from(name),
                format!(
                    "table 50{:03} \"Same Name\"\n{{\n    Caption = 'Same';\n}}\n",
                    100 + index
                ),
            );
        }
        let units = extract_translation_units(&ws);
        let ids: std::collections::HashSet<&str> = units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids.len(), units.len(), "duplicate ids leaked: {units:?}");
    }

    /// The FNV-1 hash must match `alc`'s `Hash.GetFNVHashCode` (the same
    /// algorithm `al-emit` verifies against the compiler).
    #[test]
    fn name_hash_matches_the_emitter_algorithm() {
        // Reference values computed with the FNV-1/UTF-16LE + i32::MAX bias
        // definition shared with crates/al-emit/src/method_id.rs.
        assert_eq!(name_hash(""), 2_147_483_647 + (-2128831035i64));
        // Distinct names hash distinctly, and the value is stable.
        assert_ne!(name_hash("Caption"), name_hash("ToolTip"));
        assert_eq!(name_hash("Caption"), name_hash("Caption"));
        // Hashing is case-sensitive, as in alc.
        assert_ne!(name_hash("Caption"), name_hash("caption"));
    }

    #[test]
    fn refresh_appends_obsolete_units_in_a_stable_order() {
        let generated: Vec<TranslationUnit> = Vec::new();
        let mut language = HashMap::new();
        for id in ["Z-unit", "A-unit", "M-unit"] {
            language.insert(
                id.to_string(),
                TranslationUnit {
                    id: id.to_string(),
                    object_type: "Table".to_string(),
                    object_id: 1,
                    object_name: "T".to_string(),
                    source: id.to_string(),
                    target: Some("x".to_string()),
                    state: TranslationState::Translated,
                    note: None,
                },
            );
        }
        let (units, result) = refresh_xliff(&generated, &language);
        let ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["A-unit", "M-unit", "Z-unit"]);
        assert_eq!(result.removed, vec!["A-unit", "M-unit", "Z-unit"]);
    }
}
