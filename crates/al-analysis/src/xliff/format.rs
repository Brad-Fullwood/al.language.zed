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
    // The generated `.g.xlf` is written in the source language and carries
    // no `<target>`; a language file has one per unit, empty until
    // translated.
    let generated = source_language.eq_ignore_ascii_case(target_language);
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
    // alc names the group `body`, the app name is in `original`.
    xml.push_str("      <group id=\"body\">\n");

    for unit in units {
        let object_target = unit
            .object_target
            .as_ref()
            .map(|target| format!(" al-object-target=\"{}\"", xml_escape(target)))
            .unwrap_or_default();
        xml.push_str(&format!(
            "        <trans-unit id=\"{}\" size-unit=\"char\" translate=\"yes\" xml:space=\"preserve\"{object_target}>\n",
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
        } else if !generated {
            xml.push_str(&format!(
                "          <target state=\"{}\" xml:space=\"preserve\"/>\n",
                unit.state.as_xliff_state()
            ));
        }
        // alc's two notes, which translation tools key on: the developer's
        // `Comment`, then the generator's object path.
        // alc writes the Developer note on every generated unit, empty when
        // the text has no `Comment`.
        if let Some(note) = unit.developer_note.as_deref().or(generated.then_some("")) {
            xml.push_str(&format!(
                "          <note from=\"Developer\" annotates=\"general\" priority=\"2\">{}</note>\n",
                xml_escape(note)
            ));
        }
        if let Some(note) = unit
            .note
            .as_ref()
            .filter(|note| unit.developer_note.as_ref() != Some(*note))
        {
            xml.push_str(&format!(
                "          <note from=\"Xliff Generator\" annotates=\"general\" priority=\"3\">{}</note>\n",
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
pub(super) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Why `parse_xliff` refused a file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed XLIFF at byte {position}: {message}")]
pub struct XliffParseError {
    /// Byte offset in the input where the reader stopped.
    pub position: u64,
    pub message: String,
}

/// Parse an XLIFF 1.2 file into a map of `id → TranslationUnit`.
///
/// Reads any well-formed layout: one element per line, a whole unit on one
/// line, or anything between. Malformed XML returns an error, so a caller
/// that rewrites the file can stop before it loses translations.
pub fn parse_xliff(content: &str) -> Result<HashMap<String, TranslationUnit>, XliffParseError> {
    use quick_xml::events::{BytesStart, Event};

    /// The unit being read, between `<trans-unit>` and `</trans-unit>`.
    #[derive(Default)]
    struct OpenUnit {
        id: Option<String>,
        object_target: Option<String>,
        source: Option<String>,
        target: Option<String>,
        state: Option<TranslationState>,
        note: Option<String>,
        developer_note: Option<String>,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Body {
        Source,
        Target,
        Note,
        DeveloperNote,
    }

    fn attr(element: &BytesStart<'_>, name: &str) -> Result<Option<String>, quick_xml::Error> {
        match element.try_get_attribute(name)? {
            Some(attribute) => Ok(Some(
                attribute
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                    .into_owned(),
            )),
            None => Ok(None),
        }
    }

    let mut reader = quick_xml::Reader::from_str(content);
    let mut units: HashMap<String, TranslationUnit> = HashMap::new();
    let mut depth = 0usize;
    let mut unit: Option<OpenUnit> = None;
    // The body element being read, with its depth and the text so far.
    let mut body: Option<(Body, usize, String)> = None;

    let fail = |reader: &quick_xml::Reader<&[u8]>, error: &dyn std::fmt::Display| XliffParseError {
        position: reader.error_position(),
        message: error.to_string(),
    };

    loop {
        let event = reader.read_event().map_err(|error| fail(&reader, &error))?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let name = element.local_name();
                let name = name.as_ref();
                let read = |key: &str| attr(&element, key).map_err(|error| fail(&reader, &error));
                if name == b"trans-unit" {
                    unit = Some(OpenUnit {
                        id: read("id")?,
                        object_target: read("al-object-target")?,
                        ..OpenUnit::default()
                    });
                } else if let Some(open) = unit.as_mut().filter(|_| body.is_none()) {
                    let kind = match name {
                        b"source" => Some(Body::Source),
                        b"target" => {
                            let state = read("state")?.unwrap_or_default();
                            open.state = Some(TranslationState::from_xliff_state(&state));
                            Some(Body::Target)
                        }
                        b"note" if read("from")?.as_deref() == Some("Developer") => {
                            Some(Body::DeveloperNote)
                        }
                        b"note" => Some(Body::Note),
                        _ => None,
                    };
                    match (kind, empty) {
                        // `<source/>` is an empty source. `<target/>` and an
                        // empty note carry no text, as before.
                        (Some(Body::Source), true) => open.source = Some(String::new()),
                        (Some(kind), false) => body = Some((kind, depth + 1, String::new())),
                        _ => {}
                    }
                }
                if !empty {
                    depth += 1;
                }
            }
            Event::Text(text) => {
                if let Some((_, _, buffer)) = body.as_mut() {
                    buffer.push_str(
                        &text
                            .xml10_content()
                            .map_err(|error| fail(&reader, &error))?,
                    );
                }
            }
            Event::CData(data) => {
                if let Some((_, _, buffer)) = body.as_mut() {
                    buffer.push_str(
                        &data
                            .xml10_content()
                            .map_err(|error| fail(&reader, &error))?,
                    );
                }
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference
                    .resolve_char_ref()
                    .map_err(|error| fail(&reader, &error))?
                {
                    Some(ch) => ch.to_string(),
                    None => {
                        let name = reference.decode().map_err(|error| fail(&reader, &error))?;
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .ok_or_else(|| fail(&reader, &format!("unknown entity `&{name};`")))?
                            .to_string()
                    }
                };
                if let Some((_, _, buffer)) = body.as_mut() {
                    buffer.push_str(&resolved);
                }
            }
            Event::End(element) => {
                if let Some((kind, body_depth, _)) = &body {
                    if *body_depth == depth {
                        let kind = *kind;
                        let (_, _, text) = body.take().expect("body is open");
                        if let Some(open) = unit.as_mut() {
                            match kind {
                                Body::Source => open.source = Some(text),
                                Body::Target => open.target = Some(text),
                                Body::Note => open.note = Some(text),
                                Body::DeveloperNote => open.developer_note = Some(text),
                            }
                        }
                    }
                }
                depth -= 1;
                if element.local_name().as_ref() == b"trans-unit" {
                    if let Some(open) = unit.take() {
                        insert_unit(&mut units, open.id, open.source, |id, source| {
                            // Files that carry only a Developer note (older
                            // generators put the object path there) keep it
                            // as the unit's note too.
                            let note = open.note.or_else(|| open.developer_note.clone());
                            TranslationUnit {
                                object_type: object_type_from_id(&id),
                                object_id: 0,
                                object_name: object_name_from_note(note.as_deref()),
                                id,
                                source,
                                target: open.target,
                                state: open.state.unwrap_or(TranslationState::New),
                                note,
                                developer_note: open.developer_note,
                                object_target: open.object_target,
                            }
                        });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0 {
        return Err(XliffParseError {
            position: reader.buffer_position(),
            message: format!("{depth} element(s) not closed at end of file"),
        });
    }
    Ok(units)
}

/// Record a finished unit. A unit without an id or a source is skipped.
/// Duplicate ids are malformed input: the first occurrence is kept, in
/// document order, so a later one does not overwrite a reviewed translation.
fn insert_unit(
    units: &mut HashMap<String, TranslationUnit>,
    id: Option<String>,
    source: Option<String>,
    build: impl FnOnce(String, String) -> TranslationUnit,
) {
    let (Some(id), Some(source)) = (id, source) else {
        return;
    };
    match units.entry(id.clone()) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(build(id, source));
        }
        std::collections::hash_map::Entry::Occupied(existing) => {
            tracing::warn!(
                id = %existing.key(),
                "xliff: duplicate trans-unit id in input; keeping the first occurrence"
            );
        }
    }
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

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_compact_trans_unit_keeps_target() {
        let xml = r#"<xliff><file><body><group id="body"><trans-unit id="Table 1"><source>Open</source><target state="final">Öffnen</target></trans-unit></group></body></file></xliff>"#;
        let units = parse_xliff(xml).expect("valid xliff");
        let unit = &units["Table 1"];
        assert_eq!(unit.source, "Open");
        assert_eq!(unit.target.as_deref(), Some("Öffnen"));
        assert_eq!(unit.state, TranslationState::from_xliff_state("final"));
    }

    #[test]
    fn parse_numeric_entity() {
        let xml = "<xliff><trans-unit id=\"a\"><source>Open</source><target>&#246;ffnen &amp; &#x41;</target></trans-unit></xliff>";
        let units = parse_xliff(xml).expect("valid xliff");
        assert_eq!(units["a"].target.as_deref(), Some("öffnen & A"));
    }

    #[test]
    fn parse_compact_and_indented_files_give_the_same_units() {
        let compact = r#"<xliff><trans-unit id="a"><source>Open</source><target state="translated">Auf</target><note from="Developer" annotates="general">Dev</note><note from="Xliff Generator">Table T - Property Caption</note></trans-unit></xliff>"#;
        let indented = "<xliff>\n  <trans-unit id=\"a\">\n    <source>Open</source>\n    <target state=\"translated\">Auf</target>\n    <note from=\"Developer\" annotates=\"general\">Dev</note>\n    <note from=\"Xliff Generator\">Table T - Property Caption</note>\n  </trans-unit>\n</xliff>\n";
        let compact = parse_xliff(compact).expect("valid xliff");
        let indented = parse_xliff(indented).expect("valid xliff");
        assert_eq!(compact, indented);
        let unit = &compact["a"];
        assert_eq!(unit.developer_note.as_deref(), Some("Dev"));
        assert_eq!(unit.note.as_deref(), Some("Table T - Property Caption"));
        assert_eq!(unit.object_name, "T");
    }

    #[test]
    fn parse_malformed_returns_error() {
        assert!(parse_xliff(r#"<trans-unit id="a"><source>x</trans-unit>"#).is_err());
        assert!(parse_xliff(r#"<xliff><trans-unit id="a"><source>x</source>"#).is_err());
    }
}
