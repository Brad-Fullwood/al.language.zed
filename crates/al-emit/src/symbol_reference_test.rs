//! Differential regression: the native `SymbolReference.json` emitter must
//! reproduce alc's output for a representative project. The fixtures are real
//! alc 17.0.34 output (`testdata/rich_alc_symbolreference.json`) for the AL in
//! `testdata/rich_sample.al`, so this guards the emitter without needing the
//! toolchain at test time. Regenerate the fixtures if the targeted toolchain
//! changes (see the spike doc).

use serde_json::Value;

use super::symbol_extract::extract_objects;
use super::symbol_reference::{build_symbol_reference, ExternalSymbols, ObjectRef, SymbolRefMeta};

/// A subtype defined in a referenced app resolves to its `ModuleId` + `Id`, and
/// that id flows into the method-signature hash (verified byte-identical to alc
/// for `Record Customer` against Base App, /tmp/xapp).
#[test]
fn external_subtype_emits_module_id_and_id() {
    let mut external = ExternalSymbols::default();
    external.resolver.insert(
        "customer".to_string(),
        ObjectRef {
            id: 18,
            module_id: Some("437dbf0e-84ff-417a-965d-ed2bb9650972".to_string()),
        },
    );
    let src = "codeunit 50100 \"CU\" { procedure DoIt(var Cust: Record Customer): Boolean \
               begin exit(true); end; }";
    let objects = extract_objects(src, "src/Lib.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &external);
    let method = &doc["Codeunits"][0]["Methods"][0];
    let subtype = &method["Parameters"][0]["TypeDefinition"]["Subtype"];
    assert_eq!(subtype["ModuleId"], "437dbf0e-84ff-417a-965d-ed2bb9650972");
    assert_eq!(subtype["Name"], "Customer");
    assert_eq!(subtype["Id"], 18);
    // The resolved id (18) folds into the disambiguating method-signature hash.
    assert_eq!(method["Id"], 168268624);
}

fn min_app_meta() -> SymbolRefMeta {
    SymbolRefMeta {
        runtime_version: "14.0".to_string(),
        app_id: "aaaaaaaa-1111-2222-3333-444444444444".to_string(),
        name: "Min App".to_string(),
        publisher: "Spike".to_string(),
        version: "1.0.0.0".to_string(),
    }
}

#[test]
fn namespaces_are_emitted_as_nested_segments_and_round_trip_with_identity() {
    let source = r#"namespace Contoso.Sales;

codeunit 50100 "Namespaced API"
{
    procedure Run()
    begin
    end;
}
"#;
    let objects = extract_objects(source, "src/Api.al");
    assert_eq!(objects[0].entry.namespace, "Contoso.Sales");

    let document = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    assert_eq!(document["Namespaces"][0]["Name"], "Contoso");
    assert_eq!(document["Namespaces"][0]["Namespaces"][0]["Name"], "Sales");
    assert_eq!(
        document["Namespaces"][0]["Namespaces"][0]["Codeunits"][0]["Name"],
        "Namespaced API"
    );

    let bytes = serde_json::to_vec(&document).unwrap();
    let entries = al_symbols::read_symbol_reference_bytes(&bytes, "Min App").unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.name == "Namespaced API")
        .unwrap();
    assert_eq!(entry.namespace, "Contoso.Sales");
}

#[test]
fn permission_grants_round_trip_in_public_symbol_surface() {
    let source = r#"codeunit 50100 "Target API"
{
}

permissionset 50101 "API User"
{
    Assignable = true;
    Permissions = codeunit "Target API" = X;
}
"#;
    let objects = extract_objects(source, "src/Permissions.al");
    let document = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    let bytes = serde_json::to_vec(&document).unwrap();
    let entries = al_symbols::read_symbol_reference_bytes(&bytes, "Min App").unwrap();
    let permission_set = entries
        .iter()
        .find(|entry| entry.name == "API User")
        .unwrap();
    assert_eq!(
        permission_set.permissions,
        vec![al_symbols::PermissionSymbol {
            permission_object: 5,
            object_id: 50100,
            value: 16,
        }]
    );
}

/// The inline control add-in `PublicKeyToken` is the first 8 bytes of
/// `SHA256(app name)` in lowercase hex — derived from the app name, not the
/// add-in name. Known-answer vector verified against alc.
#[test]
fn control_addin_public_key_token_is_sha256_of_app_name() {
    let objects = extract_objects(
        "controladdin \"Spike Addin\" { RequestedHeight = 100; }",
        "src/Lib.al",
    );
    let doc = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    let addins = doc
        .get("ControlAddIns")
        .and_then(|v| v.as_array())
        .expect("ControlAddIns");
    let token = addins[0]
        .get("PublicKeyToken")
        .and_then(|v| v.as_str())
        .unwrap();
    // SHA256("Min App")[..8] — independent of the add-in's own name.
    assert_eq!(token, "223c864cfb3cf5d7");
    let meta_name = addins[0]
        .get("MetadataName")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(meta_name, "Spike_Addin");
}

const SAMPLE_AL: &str = include_str!("testdata/rich_sample.al");
const ALC_GOLDEN: &str = include_str!("testdata/rich_alc_symbolreference.json");

/// Drop empty arrays so "absent" and "[]" compare equal (alc always emits a
/// core set of empty group arrays; the native emitter omits them — semantically
/// identical).
fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter_map(|(k, val)| {
                    let nv = normalize(val);
                    if matches!(&nv, Value::Array(a) if a.is_empty()) {
                        None
                    } else {
                        Some((k.clone(), nv))
                    }
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

/// A field added to a *base* page (in a referenced app) resolves its type through
/// the referenced base table: page→SourceTable name→(table,field)→type. Verified
/// byte-identical to alc for `Rec."Balance (LCY)"` on Customer Card (/tmp/xapp).
#[test]
fn external_base_table_field_type_resolves_in_page_extension() {
    let mut external = ExternalSymbols::default();
    external.resolver.insert(
        "customer card".to_string(),
        ObjectRef {
            id: 21,
            module_id: Some("437dbf0e-84ff-417a-965d-ed2bb9650972".to_string()),
        },
    );
    external
        .page_source_tables
        .insert("customer card".to_string(), "Customer".to_string());
    external.field_types.insert(
        ("customer".to_string(), "balance (lcy)".to_string()),
        "Decimal".to_string(),
    );
    let src = "pageextension 50100 \"Ext\" extends \"Customer Card\" { layout { \
               addlast(General) { field(MyAmt; Rec.\"Balance (LCY)\") { ApplicationArea = All; } } } }";
    let objects = extract_objects(src, "src/Lib.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &external);
    let field = &doc["PageExtensions"][0]["ControlChanges"][0]["Controls"][0];
    assert_eq!(
        field["TypeDefinition"],
        serde_json::json!({ "Name": "Decimal" })
    );
}

#[test]
fn page_modify_properties_and_customization_defaults_match_alc() {
    let src = r#"
        table 50100 T { fields { field(1; Name; Text[100]) { } } }
        page 50100 P { SourceTable = T; layout { area(content) { field(Name; Rec.Name) { } } } }
        pageextension 50101 PE extends P {
            layout { modify(Name) { Caption = 'Changed'; ToolTip = 'Changed tip'; } }
        }
        pagecustomization PC customizes P {
            layout {
                modify(Name) { Visible = true; }
                addlast(content) { field(CustomizedName; Rec.Name) { Caption = 'Custom'; } }
            }
        }
    "#;
    let objects = extract_objects(src, "src/Pages.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &Default::default());

    assert_eq!(
        doc["PageExtensions"][0]["ControlChanges"][0],
        serde_json::json!({
            "Anchor": "Name",
            "ChangeKind": 9,
            "Properties": [
                { "Name": "Caption", "Value": "Changed" },
                { "Name": "ToolTip", "Value": "Changed tip" }
            ]
        })
    );
    assert_eq!(
        doc["PageCustomizations"][0]["ControlChanges"][0],
        serde_json::json!({
            "Anchor": "Name",
            "ChangeKind": 9,
            "Properties": [{ "Name": "Visible", "Value": "true" }]
        })
    );
    let customized =
        &doc["PageCustomizations"][0]["ControlChanges"][1]["Controls"][0]["Properties"];
    assert!(
        customized
            .as_array()
            .unwrap()
            .iter()
            .any(|property| property
                == &serde_json::json!({
                    "Name": "Editable",
                    "Value": "False"
                })),
        "page-customization-added fields must publish Editable=False"
    );
}

#[test]
fn external_report_dataitem_uses_module_qualified_related_table() {
    let mut external = ExternalSymbols::default();
    external.resolver.insert(
        "customer".to_string(),
        ObjectRef {
            id: 18,
            module_id: Some("437dbf0e-84ff-417a-965d-ed2bb9650972".to_string()),
        },
    );
    let src = r#"report 50100 R {
        dataset { dataitem(Customer; Customer) { column(No; "No.") { } } }
    }"#;
    let objects = extract_objects(src, "src/Report.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &external);
    assert_eq!(
        doc["Reports"][0]["DataItems"][0]["RelatedTable"],
        "#437dbf0e84ff417a965ded2bb9650972#Customer"
    );
}

/// `IncludedPermissionSets` keeps each referenced name quoted only when it needs
/// quoting (alc emits `"PS A",PSC`), unlike resolved object-reference properties.
#[test]
fn included_permission_sets_keep_quoting() {
    let src = "permissionset 50100 \"PS A\" { Assignable = true; \
               IncludedPermissionSets = \"PS A\", PSC; }";
    let objects = extract_objects(src, "src/Lib.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    let props = doc["PermissionSets"][0]["Properties"].as_array().unwrap();
    let inc = props
        .iter()
        .find(|p| p["Name"] == "IncludedPermissionSets")
        .expect("IncludedPermissionSets");
    assert_eq!(inc["Value"], "\"PS A\",PSC");
}

/// A `system`-type permission resolves to the platform system-object id from the
/// generated `system_objects.json` table (not in `.alpackages`). Verified
/// byte-identical to alc (Id 5210 for `Tools, Object Designer`, /tmp/hunt).
#[test]
fn system_permission_resolves_platform_object_id() {
    let src = "permissionset 50100 \"PS\" { Assignable = true; \
               Permissions = system \"Tools, Object Designer\" = X; }";
    let objects = extract_objects(src, "src/Lib.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    assert_eq!(
        doc["PermissionSets"][0]["Permissions"][0],
        serde_json::json!({ "PermissionObject": 10, "Value": 16, "Id": 5210 })
    );
}

/// A report's `rendering { layout(...) { ... } }` becomes a `Layouts` entry with
/// the layout's declared properties; a report always carries a `RequestPage`.
#[test]
fn report_rendering_layout_serializes() {
    let src = "table 50100 \"R\" { fields { field(1; \"No.\"; Code[20]) { } } }\n\
               report 50100 \"Rep\" { dataset { dataitem(I; \"R\") { column(N; \"No.\") { } } } \
               rendering { layout(L1) { Type = RDLC; LayoutFile = 'src/l.rdl'; } } }";
    let objects = extract_objects(src, "src/Lib.al");
    let doc = build_symbol_reference(&objects, &min_app_meta(), &Default::default());
    let report = &doc["Reports"][0];
    assert_eq!(
        report["Layouts"],
        serde_json::json!([{
            "Properties": [
                { "Name": "Type", "Value": "RDLC" },
                { "Name": "LayoutFile", "Value": "src/l.rdl" }
            ],
            "Name": "L1"
        }])
    );
    // Even with no `requestpage`, alc emits the default request page.
    assert_eq!(
        report["RequestPage"],
        serde_json::json!({ "Id": 0, "Name": "RequestOptionsPage" })
    );
}

#[test]
fn native_symbol_reference_matches_alc() {
    // The fixture was built from app id aaaaaaaa-…-444444444444 / "Min App".
    let objects = extract_objects(SAMPLE_AL, "src/Lib.al");
    let meta = SymbolRefMeta {
        runtime_version: "14.0".to_string(),
        app_id: "aaaaaaaa-1111-2222-3333-444444444444".to_string(),
        name: "Min App".to_string(),
        publisher: "Spike".to_string(),
        version: "1.0.0.0".to_string(),
    };
    let mine = build_symbol_reference(&objects, &meta, &Default::default());

    let alc: Value = serde_json::from_str(ALC_GOLDEN).expect("golden json");
    assert_eq!(
        normalize(&mine),
        normalize(&alc),
        "native SymbolReference.json diverged from alc output"
    );

    // Stronger guarantee: the serialized bytes are byte-identical to alc's
    // (exact key order, no spaces, every group emitted as alc emits it). The
    // golden was captured verbatim from alc, compact and BOM-stripped.
    let mine_bytes = serde_json::to_string(&mine).expect("serialize");
    assert_eq!(
        mine_bytes,
        ALC_GOLDEN.trim_end_matches('\n'),
        "native SymbolReference.json is not byte-identical to alc output"
    );
}
