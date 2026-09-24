//! Reading and writing XLIFF 1.2 files.

use super::*;

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
pub(super) fn xml_escape(s: &str) -> String {
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
                let note = current_note.take();
                let unit = TranslationUnit {
                    object_type: object_type_from_id(&id),
                    object_id: 0,
                    object_name: object_name_from_note(note.as_deref()),
                    id: id.clone(),
                    source,
                    target: current_target.take(),
                    state: current_state.clone(),
                    note,
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
        }
    }

    units
}

/// The object type from a translation-unit id.
///
/// The id opens with the object kind: `Table 1234 - Field 5678 - Property 90`.
/// Leaving it empty made every row of `xlf.untranslated` report no object at
/// all, so a translator asking which object a missing string belongs to got
/// nothing back.
pub(super) fn object_type_from_id(id: &str) -> String {
    id.split_whitespace().next().unwrap_or_default().to_string()
}

/// The object name from a developer note.
///
/// alc and this module both write the note as
/// `<Type> <Object name> - <Member kind> <Member name> - Property <name>`, so
/// the object name is what sits between the kind and the first ` - `.
pub(super) fn object_name_from_note(note: Option<&str>) -> String {
    let Some(note) = note else {
        return String::new();
    };
    let head = note.split(" - ").next().unwrap_or_default().trim();
    match head.split_once(char::is_whitespace) {
        Some((_kind, name)) => name.trim().to_string(),
        None => String::new(),
    }
}

/// Extract an XML attribute value from a tag string.
pub(super) fn extract_xml_attr(tag: &str, attr: &str) -> Option<String> {
    let search = format!("{}=\"", attr);
    let start = tag.find(&search)? + search.len();
    let end = tag[start..].find('"')? + start;
    let raw = &tag[start..end];
    Some(xml_unescape(raw))
}

pub(super) fn xml_unescape(s: &str) -> String {
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
