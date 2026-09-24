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
    let result = parse_object_header_line("table 50100 \"Customer Extension\"").unwrap();
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
    let header = parse_object_header_line(text).unwrap();
    assert_eq!(header.kind_display, "PageExtension");
    assert_eq!(header.id, 50101);
    assert_eq!(header.name, "Sales Order Pageext");
}

#[test]
fn detect_object_header_unquoted_name_stops_before_extends_clause() {
    let text = "tableextension 50100 MyExt extends MyBase";
    let header = parse_object_header_line(text).unwrap();
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

/// AL allows several objects in one file. Attributing the whole file to
/// the first one gave the second table's captions the first table's name
/// hash, and a shared field name then produced a duplicate id whose unit
/// was dropped.
#[test]
fn every_object_in_a_multi_object_file_is_extracted() {
    let units = extract(
        r#"table 50100 "Shipment Header"
{
fields
{
    field(1; "Document No."; Code[20])
    {
        Caption = 'Header Document No.';
    }
}
}

table 50101 "Shipment Line"
{
fields
{
    field(1; "Document No."; Code[20])
    {
        Caption = 'Line Document No.';
    }
}
}"#,
    );
    assert_eq!(units.len(), 2, "{units:#?}");
    assert_eq!(units[0].object_name, "Shipment Header");
    assert_eq!(units[1].object_name, "Shipment Line");
    assert_ne!(units[0].id, units[1].id, "the two ids must differ");
    assert!(units[1].note.as_ref().unwrap().contains("Shipment Line"));
}

/// A brace inside a block comment used to push a frame that was never
/// popped, so every id built after it anchored one level too deep.
#[test]
fn an_unbalanced_brace_in_a_block_comment_is_ignored() {
    let balanced = extract(
        r#"table 50100 "T"
{
fields
{
    field(1; Name; Text[100])
    {
        Caption = 'Name';
    }
}
}"#,
    );
    let commented = extract(
        r#"table 50100 "T"
{
fields
{
    /* the old layout used a { here */
    field(1; Name; Text[100])
    {
        Caption = 'Name';
    }
}
}"#,
    );
    assert_eq!(commented.len(), 1);
    assert_eq!(commented[0].id, balanced[0].id);
}

/// alc emits `Action` for a group inside `actions`. Keying it `Control`
/// made refresh report the unit as both added and removed on every run,
/// losing the existing translation.
#[test]
fn an_action_group_is_keyed_as_an_action() {
    let units = extract(
        r#"page 50100 "P"
{
actions
{
    area(Processing)
    {
        group(Posting)
        {
            Caption = 'Posting';

            action(Post)
            {
                Caption = 'Post';
            }
        }
    }
}
}"#,
    );
    let group = units
        .iter()
        .find(|u| u.source == "Posting")
        .expect("group caption");
    assert!(
        group.note.as_ref().unwrap().contains("Action Posting"),
        "{:?}",
        group.note
    );
    let action = units
        .iter()
        .find(|u| u.source == "Post")
        .expect("action caption");
    assert!(action.note.as_ref().unwrap().contains("Action Post"));
}

/// A one-line member declares the block its property belongs to, so the
/// property has to anchor to the member, not to the enclosing section:
/// anchoring it to the object collides with the object's own caption, and
/// the duplicate-id filter then drops one of the two.
#[test]
fn a_one_line_member_anchors_its_own_property() {
    let units = extract(
        r#"table 50100 "T"
{
Caption = 'T';

fields
{
    field(1; "No."; Code[20]) { Caption = 'No.'; }
    field(2; "Name"; Text[100]) { Caption = 'Name'; ToolTip = 'The name.'; }
}
}"#,
    );
    let unit = |source: &str| {
        units
            .iter()
            .find(|unit| unit.source == source)
            .unwrap_or_else(|| panic!("no unit for {source:?}: {units:#?}"))
    };
    let number = unit("No.");
    let name = unit("Name");
    unit("The name.");
    assert_ne!(number.id, name.id);
    assert_ne!(unit("T").id, number.id, "{units:#?}");
    assert!(number.note.as_ref().unwrap().contains("Field No."));
    assert!(name.note.as_ref().unwrap().contains("Field Name"));
}

/// alc accepts any spacing around `=`; requiring exactly one space left
/// the caption out of the generated file with nothing said about it.
#[test]
fn a_caption_parses_with_any_spacing_around_equals() {
    let units = extract(
        r#"page 50100 "P"
{
Caption='Posted Shipment';
}"#,
    );
    assert_eq!(units.len(), 1, "{units:#?}");
    assert_eq!(units[0].source, "Posted Shipment");

    let padded = extract(
        r#"page 50100 "P"
{
Caption  =  'Posted Shipment';
}"#,
    );
    assert_eq!(padded.len(), 1, "{padded:#?}");
    assert_eq!(padded[0].source, "Posted Shipment");
}

/// The word inside a Comment is text, not the `Locked` modifier.
#[test]
fn a_comment_mentioning_locked_does_not_lock_the_caption() {
    let units = extract(
        r#"page 50100 "P"
{
Caption = 'Closed', Comment = 'Shown when the period is locked; %1 is the date';
}"#,
    );
    assert_eq!(units.len(), 1, "{units:#?}");
    assert_eq!(units[0].source, "Closed");

    let also = extract(
        r#"page 50100 "P"
{
Caption = 'Closed', Comment = 'locked, see the manual';
}"#,
    );
    assert_eq!(also.len(), 1, "{also:#?}");
}

/// A genuinely locked string still stays out of the generated file.
#[test]
fn a_locked_caption_is_still_dropped() {
    assert!(extract(
        r#"page 50100 "P"
{
Caption = 'SEPA', Locked = true;
}"#
    )
    .is_empty());
    assert!(extract(
        r#"codeunit 50100 "C"
{
var
    Tag: Label 'SEPA', Locked = true;
}"#
    )
    .is_empty());
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
    let result = suggest_translations(&ws, &[&unit], &[]);
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
    let result = suggest_translations(&ws, &[&unit], &[]);
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
    let result = suggest_translations(&ws, &[&unit], &[&translated]);
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
    let result = suggest_translations(&ws, &[&unit], &[&translated]);
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
    let result = suggest_translations(&ws, &[&unit], &[&translated]);
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
    let result = suggest_translations(&ws, &[&unit], &[&translated]);
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
    let result = suggest_translations(&ws, &[&unit], &[&translated]);
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
    let result = suggest_translations(&ws, &[&unit], &[&not_done, &empty_target]);
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
    assert!(units.iter().any(
        |u| u.id == format!("Page {page_hash} - Property {caption_hash}") && u.source == "My Page"
    ));
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

/// Every row of `xlf.untranslated` reported an empty object, for every
/// string, because nothing ever reconstructed the three fields.
#[test]
fn a_parsed_unit_carries_its_object_type_and_name() {
    let xlf = r#"<?xml version="1.0" encoding="utf-8"?>
<xliff version="1.2">
  <file datatype="xml" source-language="en-US" target-language="da-DK" original="App">
<body>
  <group id="body">
    <trans-unit id="Table 2764513245 - Field 1296262074 - Property 2879900210" size-unit="char" translate="yes" xml:space="preserve">
      <source>Document No.</source>
      <note from="Developer" annotates="general" priority="2">Table Shipment Header - Field Document No. - Property Caption</note>
    </trans-unit>
  </group>
</body>
  </file>
</xliff>"#;
    let units = parse_xliff(xlf);
    let unit = units.values().next().expect("one unit");
    assert_eq!(unit.object_type, "Table");
    assert_eq!(unit.object_name, "Shipment Header");
}

/// `units_map.into_values()` is HashMap order, so the query output moved
/// between runs until the sort was real.
#[test]
fn untranslated_units_come_back_in_id_order() {
    let unit = |id: &str| TranslationUnit {
        id: id.to_string(),
        object_type: "Table".to_string(),
        object_id: 0,
        object_name: String::new(),
        source: id.to_string(),
        target: None,
        state: TranslationState::New,
        note: None,
    };
    let units = vec![unit("Table 3 - Property 1"), unit("Table 1 - Property 1")];
    let ids: Vec<&str> = find_untranslated(&units)
        .iter()
        .map(|u| u.id.as_str())
        .collect();
    assert_eq!(ids, ["Table 1 - Property 1", "Table 3 - Property 1"]);
}

#[test]
fn header_detection_returns_none_when_the_file_has_no_object() {
    assert!(extract("// just a comment\n\n").is_empty());
    assert!(extract("").is_empty());
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
