//! The two XLIFF extractors in this workspace must agree about trans-unit ids.
//!
//! `al-emit` writes `TextData/<App>.TextData.en-US.xliff` into the packaged
//! `.app` from tree-sitter-parsed objects. `al-analysis` writes
//! `Translations/<App>.g.xlf` for `al-explorer xlf generate` from a line-based
//! scan. They implement `name_hash` separately, on purpose, so that the emitter
//! does not have to depend on the analysis crate. Nothing else asserts the two
//! produce the same id for the same source, and a divergence makes
//! `xlf refresh` match zero ids.
//!
//! The expected ids below are not derived from either implementation. They were
//! captured from Microsoft's `alc` 17.0.34.45391 compiling the same object
//! declarations, with `"features": ["TranslationFile"]` for the `.g.xlf` side.
//! Format reference: "Working with translation files",
//! <https://learn.microsoft.com/dynamics365/business-central/dev-itpro/developer/devenv-work-with-translation-files>.
//!
//! The two files are *not* the same artifact, and alc treats them differently.
//! `packaged_text_data_follows_alc_not_the_g_xlf_rules` pins the differences.

use std::collections::BTreeSet;

/// The object shapes whose alc ids were captured, one object per file —
/// `al-analysis` scans a file at a time and keys every unit on the file's
/// first object declaration.
const FIXTURE: &[(&str, &str)] = &[
    (
        "src/Widget.al",
        r#"
table 50100 Widget
{
    Caption = 'Widget';
    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
    }
}
"#,
    ),
    (
        "src/WidgetCard.al",
        r#"
page 50100 WidgetCard
{
    PageType = Card;
    SourceTable = Widget;
    Caption = 'Widget Card';
    layout
    {
        area(content)
        {
            field(No; Rec."No.")
            {
                ApplicationArea = All;
                Caption = 'Number';
                ToolTip = 'Specifies the number.';
            }
        }
    }
    actions
    {
        area(processing)
        {
            action(DoIt)
            {
                ApplicationArea = All;
                Caption = 'Do It';
                ToolTip = 'Does it.';
            }
        }
    }
}
"#,
    ),
];

/// A codeunit with a plain and a locked `Label`, and a table whose field
/// caption is locked.
const LOCKED_FIXTURE: &[(&str, &str)] = &[
    (
        "src/Hello.al",
        r#"
codeunit 50100 Hello
{
    var
        GreetingLbl: Label 'Hello there';
        LockedLbl: Label 'SEPA CT', Locked = true;
}
"#,
    ),
    (
        "src/Locked.al",
        r#"
table 50101 Locked
{
    fields
    {
        field(1; Name; Text[50])
        {
            Caption = 'SEPA CT', Locked = true;
        }
    }
}
"#,
    ),
];

/// alc's ids for `FIXTURE`, from both the packaged TextData xliff and the
/// generated `.g.xlf` (the two agree on every unit in this fixture).
const ALC_FIXTURE_IDS: &[&str] = &[
    "Table 4006738456 - Property 2879900210",
    "Table 4006738456 - Field 4200184881 - Property 2879900210",
    "Page 3924384772 - Property 2879900210",
    "Page 3924384772 - Control 949215307 - Property 2879900210",
    "Page 3924384772 - Control 949215307 - Property 1295455071",
    "Page 3924384772 - Action 3334659418 - Property 2879900210",
    "Page 3924384772 - Action 3334659418 - Property 1295455071",
];

/// Trans-unit ids in an XLIFF document, in document order.
fn trans_unit_ids(xml: &str) -> Vec<String> {
    xml.match_indices("<trans-unit id=\"")
        .map(|(at, needle)| {
            let rest = &xml[at + needle.len()..];
            rest[..rest.find('"').expect("unterminated trans-unit id")].to_string()
        })
        .collect()
}

/// The packaged XLIFF the emitter writes into the `.app`.
fn emitted_ids(files: &[(&str, &str)]) -> BTreeSet<String> {
    let objects: Vec<_> = files
        .iter()
        .flat_map(|(path, source)| al_emit::extract_objects(source, path))
        .collect();
    let xml = al_emit::assemble::xliff_xml(&objects, 15)
        .map(|bytes| String::from_utf8(bytes).expect("XLIFF is UTF-8"))
        .unwrap_or_default();
    trans_unit_ids(&xml).into_iter().collect()
}

/// The ids `al-explorer xlf generate` writes into `Translations/<App>.g.xlf`.
fn generated_ids(files: &[(&str, &str)]) -> BTreeSet<String> {
    let workspace = al_workspace::Workspace::new();
    for (path, source) in files {
        workspace
            .file_index
            .add_file(std::path::PathBuf::from(path), (*source).into());
    }
    al_analysis::xliff::extract_translation_units(&workspace)
        .into_iter()
        .map(|unit| unit.id)
        .collect()
}

#[test]
fn both_extractors_produce_alcs_caption_and_tooltip_ids() {
    let expected: BTreeSet<String> = ALC_FIXTURE_IDS.iter().map(|id| id.to_string()).collect();
    assert_eq!(
        emitted_ids(FIXTURE),
        expected,
        "the packaged XLIFF disagrees with alc"
    );
    assert_eq!(
        generated_ids(FIXTURE),
        expected,
        "the generated .g.xlf disagrees with alc"
    );
}

#[test]
fn label_ids_use_alcs_named_type_shape() {
    // Labels reach the `.g.xlf` only. `Hello` hashes to 4288192982 and
    // `GreetingLbl` to 514421613 in alc's output for the same declaration.
    let ids = generated_ids(LOCKED_FIXTURE);
    assert!(
        ids.contains("Codeunit 4288192982 - NamedType 514421613"),
        "missing the Label unit: {ids:?}"
    );
}

#[test]
fn packaged_text_data_follows_alc_not_the_g_xlf_rules() {
    // Verified against alc 17.0.34.45391 on the same declarations: the
    // packaged `TextData/<App>.TextData.en-US.xliff` carries no `NamedType`
    // (Label) or `ReportLabel` unit, and it carries locked captions and
    // tooltips as `translate="yes"`. The `.g.xlf` is the opposite on both
    // counts. Neither file is a bug in the other's terms.
    let packaged = emitted_ids(LOCKED_FIXTURE);
    assert!(
        !packaged.iter().any(|id| id.contains(" - NamedType ")),
        "alc keeps Label units out of the packaged XLIFF: {packaged:?}"
    );
    assert!(
        packaged.contains("Table 3846472926 - Field 2961552353 - Property 2879900210"),
        "alc keeps the locked field caption in the packaged XLIFF: {packaged:?}"
    );

    let generated = generated_ids(LOCKED_FIXTURE);
    assert!(
        generated
            .iter()
            .any(|id| id == "Codeunit 4288192982 - NamedType 514421613"),
        "the .g.xlf carries the unlocked Label: {generated:?}"
    );
    assert!(
        !generated
            .iter()
            .any(|id| id == "Table 3846472926 - Field 2961552353 - Property 2879900210"),
        "the .g.xlf drops the locked field caption: {generated:?}"
    );
}
