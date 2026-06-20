//! Serialize extracted AL objects into alc's `SymbolReference.json` shape.
//!
//! Produces a `serde_json::Value` matching what `alc` writes: object-type
//! groupings, `TypeDefinition` objects, generated method `Id`s (via the cracked
//! [`super::method_id`]), property/attribute value normalisation, etc. Verified
//! by differential testing against the local `alc` (see the project's emit
//! example + the spike doc).

use serde_json::{json, Map, Value};

use super::method_id::{combine_hash, fnv1_hash, fnv1_hash_bytes, member_id, method_id, ParamSig};
use super::symbol_extract::{
    ControlChange, EmitObject, PageControl, PermissionDecl, QueryElement, ReportLayout,
};
use crate::symbols::model::{
    AttributeSymbol, EnumValueSymbol, FieldSymbol, KeySymbol, MethodSymbol, ObjectKind,
    ParameterSymbol, PropertyValue, SymbolEntry, VariableSymbol,
};
use crate::syntax::language_data::nav_type_kind_id;

/// Package-level metadata for the `SymbolReference.json` header/footer.
#[derive(Debug, Clone)]
pub struct SymbolRefMeta {
    pub runtime_version: String,
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// A resolvable object reference: its id, plus the defining module's app-id when
/// it lives in a *referenced* app (`None` for project-local objects). External
/// references emit a `Subtype.ModuleId`; project ones do not.
#[derive(Clone, Default)]
pub struct ObjectRef {
    pub id: i32,
    pub module_id: Option<String>,
}

/// Object name (lowercased) → its reference (id + optional defining module).
pub type Resolver = std::collections::HashMap<String, ObjectRef>;

/// Symbols loaded from *referenced* apps (`.alpackages`), used to resolve
/// cross-app object ids and the field types of fields added to base
/// pages/reports. All keys are lowercased.
#[derive(Default, Clone)]
pub struct ExternalSymbols {
    /// Object name → id + defining module.
    pub resolver: Resolver,
    /// (table name, field name) → field type.
    pub field_types: std::collections::HashMap<(String, String), String>,
    /// Page name → its SourceTable name.
    pub page_source_tables: std::collections::HashMap<String, String>,
}

/// Build the project resolver merged over `external` (referenced-app objects);
/// project objects shadow referenced ones of the same name.
fn merged_resolver(objects: &[EmitObject], external: &Resolver) -> Resolver {
    let mut resolver = external.clone();
    for o in objects {
        resolver.insert(
            o.entry.name.to_lowercase(),
            ObjectRef {
                id: o.entry.id,
                module_id: None,
            },
        );
    }
    resolver
}

/// Build the full `SymbolReference.json` document. `external` resolves object
/// references (and base table field types / page source tables) that live in
/// referenced apps (System/Base) to their id + module.
pub fn build_symbol_reference(
    objects: &[EmitObject],
    meta: &SymbolRefMeta,
    external: &ExternalSymbols,
) -> Value {
    let resolver = merged_resolver(objects, &external.resolver);

    // (table name, field name) → field type, for resolving page-field
    // SourceExpression types. Seeded from referenced apps; project tables override.
    let mut field_types = external.field_types.clone();
    // page name → its SourceTable, so page extensions can resolve added-field
    // types. Seeded from referenced apps; project pages override.
    let mut page_source_tables = external.page_source_tables.clone();
    // (report name, dataitem name) → related table, so report extensions can
    // resolve added-column types against the base report's dataitems.
    let mut report_dataitem_tables: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for o in objects {
        if matches!(o.entry.kind, ObjectKind::Table | ObjectKind::TableExtension) {
            for f in &o.entry.fields {
                field_types.insert(
                    (o.entry.name.to_lowercase(), f.name.to_lowercase()),
                    f.type_name.clone(),
                );
            }
        }
        if o.entry.kind == ObjectKind::Page {
            if let Some(st) = o
                .entry
                .properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("SourceTable"))
            {
                page_source_tables.insert(
                    o.entry.name.to_lowercase(),
                    st.value.trim_matches('"').to_string(),
                );
            }
        }
        if o.entry.kind == ObjectKind::Report {
            collect_dataitem_tables(
                &o.query_elements,
                &o.entry.name.to_lowercase(),
                &mut report_dataitem_tables,
            );
        }
    }

    // Before runtime 16.0, alc forces page-customization-added field controls to
    // be non-editable (records `Editable=False`); 16.0+ allows marking editable.
    let runtime_major: u32 = meta
        .runtime_version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let force_noneditable_customizations = runtime_major < 16;

    let mut root = Map::new();
    root.insert("RuntimeVersion".into(), json!(meta.runtime_version));

    // Object groups in alc's exact emission order. The `always` flag marks the
    // core groups alc emits even when empty; the rest appear only when non-empty.
    // `DotNetPackages` (kind `None`) is an always-empty group with no source
    // objects (DotNet is OnPrem-only). Verified against alc 17.x output.
    let groups: &[(&str, Option<ObjectKind>, bool)] = &[
        ("Tables", Some(ObjectKind::Table), false),
        ("Codeunits", Some(ObjectKind::Codeunit), true),
        ("Pages", Some(ObjectKind::Page), false),
        ("PageExtensions", Some(ObjectKind::PageExtension), false),
        (
            "PageCustomizations",
            Some(ObjectKind::PageCustomization),
            false,
        ),
        ("TableExtensions", Some(ObjectKind::TableExtension), false),
        ("Reports", Some(ObjectKind::Report), true),
        ("XmlPorts", Some(ObjectKind::XmlPort), true),
        ("Queries", Some(ObjectKind::Query), true),
        ("Profiles", Some(ObjectKind::Profile), false),
        (
            "ProfileExtensions",
            Some(ObjectKind::ProfileExtension),
            false,
        ),
        ("ControlAddIns", Some(ObjectKind::ControlAddIn), true),
        ("EnumTypes", Some(ObjectKind::Enum), true),
        ("EnumExtensionTypes", Some(ObjectKind::EnumExtension), false),
        ("DotNetPackages", None, true),
        ("Interfaces", Some(ObjectKind::Interface), true),
        ("PermissionSets", Some(ObjectKind::PermissionSet), true),
        (
            "PermissionSetExtensions",
            Some(ObjectKind::PermissionSetExtension),
            true,
        ),
        ("ReportExtensions", Some(ObjectKind::ReportExtension), true),
    ];

    for (key, kind, always) in groups {
        let arr: Vec<Value> = match kind {
            Some(k) => objects
                .iter()
                .filter(|o| o.entry.kind == *k)
                .map(|o| {
                    object_json(
                        o,
                        &resolver,
                        &meta.app_id,
                        &meta.name,
                        &field_types,
                        &page_source_tables,
                        &report_dataitem_tables,
                        force_noneditable_customizations,
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        if !arr.is_empty() || *always {
            root.insert((*key).to_string(), Value::Array(arr));
        }
    }

    root.insert("InternalsVisibleToModules".into(), json!([]));
    root.insert("AppId".into(), json!(meta.app_id));
    root.insert("Name".into(), json!(meta.name));
    root.insert("Publisher".into(), json!(meta.publisher));
    root.insert("Version".into(), json!(meta.version));
    Value::Object(root)
}

/// Per-profile `ProfileSymbolReferences/<MetadataName>.json` files — one per
/// profile and profile extension. alc emits these alongside the main
/// `SymbolReference.json`, each wrapping the profile's object JSON as
/// `{ "PageCustomizations": [...], "Profiles"|"ProfileExtensions": [obj] }`. The
/// `PageCustomizations` array holds the customizations a profile *binds*; our
/// fixtures bind none, so it is empty. Each file carries alc's UTF-8 BOM.
pub fn build_profile_symbol_references(
    objects: &[EmitObject],
    meta: &SymbolRefMeta,
    external: &ExternalSymbols,
) -> Vec<(String, Vec<u8>)> {
    let resolver = merged_resolver(objects, &external.resolver);
    let runtime_major: u32 = meta
        .runtime_version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let force_noneditable = runtime_major < 16;
    let empty_ft: FieldTypes = std::collections::HashMap::new();
    let empty_pst: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let empty_rdt: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();

    let mut out = Vec::new();
    for o in objects {
        let group = match o.entry.kind {
            ObjectKind::Profile => "Profiles",
            ObjectKind::ProfileExtension => "ProfileExtensions",
            _ => continue,
        };
        let obj = object_json(
            o,
            &resolver,
            &meta.app_id,
            &meta.name,
            &empty_ft,
            &empty_pst,
            &empty_rdt,
            force_noneditable,
        );
        let mut doc = Map::new();
        doc.insert("PageCustomizations".into(), json!([]));
        doc.insert(group.into(), json!([obj]));
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(&serde_json::to_vec(&Value::Object(doc)).unwrap_or_default());
        let filename = format!(
            "ProfileSymbolReferences/{}.json",
            metadata_name(&o.entry.name)
        );
        out.push((filename, bytes));
    }
    out
}

type FieldTypes = std::collections::HashMap<(String, String), String>;

/// The auto-generated strong-name `PublicKeyToken` alc assigns to an inline
/// control add-in: the first 8 bytes of `SHA256(UTF8(app/module name))`, lowercase
/// hex. Reverse-engineered from `SourceControlAddInTypeSymbol.CalculatePublicKeyToken`
/// — note it keys on the *app name*, so every add-in in an app shares one token.
pub(super) fn control_addin_public_key_token(app_name: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(app_name.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// alc's `MetadataName` for a control add-in: non-identifier characters → `_`.
pub(super) fn metadata_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn object_json(
    obj: &EmitObject,
    resolver: &Resolver,
    app_id: &str,
    app_name: &str,
    field_types: &FieldTypes,
    page_source_tables: &std::collections::HashMap<String, String>,
    report_dataitem_tables: &std::collections::HashMap<(String, String), String>,
    force_noneditable_customizations: bool,
) -> Value {
    let e = &obj.entry;
    let mut m = Map::new();

    // Extension objects record the base object they extend. Report extensions
    // name it `Target`; every other extension uses `TargetObject`. A base object
    // in a *referenced* app is qualified `#<moduleid-no-dashes>#<name>`.
    if let Some(target) = &e.extends {
        let key = if e.kind == ObjectKind::ReportExtension {
            "Target"
        } else {
            "TargetObject"
        };
        let value = match resolver
            .get(&target.to_lowercase())
            .and_then(|r| r.module_id.as_deref())
        {
            Some(module_id) => format!("#{}#{}", module_id.replace('-', ""), target),
            None => target.clone(),
        };
        m.insert(key.into(), json!(value));
    }

    let locals: LocalTypes = e
        .variables
        .iter()
        .map(|v| (v.name.to_lowercase(), v.type_name.clone()))
        .collect();

    if matches!(e.kind, ObjectKind::Page | ObjectKind::PageExtension) {
        let source_table = e
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("SourceTable"))
            .map(|p| p.value.trim_matches('"').to_string())
            .unwrap_or_default();
        if !obj.page_controls.is_empty() {
            m.insert(
                "Controls".into(),
                Value::Array(
                    obj.page_controls
                        .iter()
                        .map(|c| {
                            control_json(
                                c,
                                e.id,
                                &source_table,
                                field_types,
                                resolver,
                                &locals,
                                false,
                            )
                        })
                        .collect(),
                ),
            );
        }
        if !obj.page_actions.is_empty() {
            m.insert(
                "Actions".into(),
                Value::Array(
                    obj.page_actions
                        .iter()
                        .map(|a| action_json(a, e.id))
                        .collect(),
                ),
            );
        }
    }

    // Page extensions and page customizations record layout change operations
    // against the base page, resolving added-field types through its SourceTable.
    if matches!(
        e.kind,
        ObjectKind::PageExtension | ObjectKind::PageCustomization
    ) && !obj.control_changes.is_empty()
    {
        let base_table = e
            .extends
            .as_deref()
            .and_then(|t| page_source_tables.get(&t.trim_matches('"').to_lowercase()))
            .cloned()
            .unwrap_or_default();
        let inject_editable_false =
            e.kind == ObjectKind::PageCustomization && force_noneditable_customizations;
        m.insert(
            "ControlChanges".into(),
            Value::Array(
                obj.control_changes
                    .iter()
                    .map(|ch| {
                        control_change_json(
                            ch,
                            e.id,
                            &base_table,
                            field_types,
                            resolver,
                            &locals,
                            inject_editable_false,
                        )
                    })
                    .collect(),
            ),
        );
    }

    if e.kind == ObjectKind::Report {
        if !e.variables.is_empty() {
            m.insert("Variables".into(), variables_json(&e.variables, resolver));
        }
        // alc always emits a report's request page (an empty one when the report
        // declares no `requestpage`); `Controls` appears only when non-empty.
        let mut request_page = Map::new();
        if !obj.page_controls.is_empty() {
            request_page.insert(
                "Controls".into(),
                Value::Array(
                    obj.page_controls
                        .iter()
                        .map(|c| control_json(c, e.id, "", field_types, resolver, &locals, false))
                        .collect(),
                ),
            );
        }
        request_page.insert("Id".into(), json!(0));
        request_page.insert("Name".into(), json!("RequestOptionsPage"));
        m.insert("RequestPage".into(), Value::Object(request_page));
        m.insert(
            "DataItems".into(),
            Value::Array(
                obj.query_elements
                    .iter()
                    .map(|el| report_dataitem_json(el, e.id, field_types, resolver))
                    .collect(),
            ),
        );
        m.insert("Labels".into(), json!([]));
        m.insert("Layouts".into(), report_layouts_json(&obj.report_layouts));
    }

    if e.kind == ObjectKind::ReportExtension {
        if !e.variables.is_empty() {
            m.insert("Variables".into(), variables_json(&e.variables, resolver));
        }
        // Request page layout changes become a RequestPageExtension object.
        if !obj.control_changes.is_empty() {
            let control_changes: Vec<Value> = obj
                .control_changes
                .iter()
                .map(|ch| control_change_json(ch, e.id, "", field_types, resolver, &locals, false))
                .collect();
            m.insert(
                "RequestPage".into(),
                json!({
                    "ControlChanges": control_changes,
                    "ReferenceSourceFileName": obj.source_file,
                    "Name": "RequestPageExtension",
                }),
            );
        }
        // Added dataitems (recursive) and the flattened added columns, resolved
        // against the base report's dataitem tables.
        let base_report = e.extends.as_deref().unwrap_or("").to_lowercase();
        let dataitems: Vec<Value> = obj
            .dataset_changes
            .iter()
            .flat_map(|dc| dc.dataitems.iter())
            .map(|el| report_dataitem_json(el, e.id, field_types, resolver))
            .collect();
        m.insert("DataItems".into(), Value::Array(dataitems));
        let mut columns: Vec<Value> = Vec::new();
        for dc in &obj.dataset_changes {
            let table = report_dataitem_tables
                .get(&(base_report.clone(), dc.anchor.to_lowercase()))
                .cloned()
                .unwrap_or_default();
            for (name, source) in &dc.columns {
                let ty = field_types
                    .get(&(
                        table.to_lowercase(),
                        source.trim_matches('"').to_lowercase(),
                    ))
                    .cloned()
                    .unwrap_or_else(|| "None".to_string());
                columns.push(json!({
                    "OwningDataItemName": dc.anchor,
                    "TypeDefinition": type_def_json(&ty, resolver),
                    "Id": member_id(name),
                    "Name": name,
                }));
            }
        }
        m.insert("Columns".into(), Value::Array(columns));
        m.insert("Labels".into(), json!([]));
        m.insert("Layouts".into(), report_layouts_json(&obj.report_layouts));
    }
    if !e.implements.is_empty() {
        m.insert("ImplementedInterfaces".into(), json!(e.implements));
    }
    if !e.fields.is_empty() {
        m.insert(
            "Fields".into(),
            Value::Array(e.fields.iter().map(|f| field_json(f, resolver)).collect()),
        );
    }
    if !e.keys.is_empty() {
        m.insert(
            "Keys".into(),
            Value::Array(e.keys.iter().map(key_json).collect()),
        );
    }
    if !obj.field_groups.is_empty() {
        m.insert(
            "FieldGroups".into(),
            Value::Array(
                obj.field_groups
                    .iter()
                    .map(|g| json!({ "FieldNames": g.field_names, "Name": g.name }))
                    .collect(),
            ),
        );
    }
    if e.kind == ObjectKind::Query {
        m.insert(
            "Elements".into(),
            Value::Array(obj.query_elements.iter().map(query_element_json).collect()),
        );
    }
    if matches!(
        e.kind,
        ObjectKind::PermissionSet | ObjectKind::PermissionSetExtension
    ) {
        m.insert(
            "Permissions".into(),
            Value::Array(
                obj.permissions
                    .iter()
                    .map(|p| permission_json(p, resolver))
                    .collect(),
            ),
        );
    }
    if matches!(e.kind, ObjectKind::Enum | ObjectKind::EnumExtension) {
        let values: Vec<Value> = e
            .enum_values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let props = obj
                    .enum_value_properties
                    .get(i)
                    .map(|p| p.as_slice())
                    .unwrap_or(&[]);
                enum_value_json(v, props)
            })
            .collect();
        m.insert("Values".into(), Value::Array(values));
    }
    // alc emits only the public method surface — `local` procedures are excluded.
    let public_methods: Vec<&MethodSymbol> = e.methods.iter().filter(|mth| !mth.is_local).collect();
    if !public_methods.is_empty() {
        let is_interface = e.kind == ObjectKind::Interface;
        m.insert(
            "Methods".into(),
            Value::Array(
                public_methods
                    .iter()
                    .map(|mth| method_json(mth, e.id, is_interface, resolver))
                    .collect(),
            ),
        );
    }
    // ControlAddIns carry an assembly identity instead of object members. An
    // explicit `PublicKeyToken` property (external add-ins) is used as-is;
    // otherwise alc derives the inline add-in's strong-name token from the app
    // name (`SHA256(app name)[..8]`, shared by every add-in in the app).
    if e.kind == ObjectKind::ControlAddIn {
        let token = e
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("PublicKeyToken"))
            .map(|p| p.value.clone())
            .unwrap_or_else(|| control_addin_public_key_token(app_name));
        m.insert("PublicKeyToken".into(), json!(token));
        m.insert("MetadataName".into(), json!(metadata_name(&e.name)));
    }
    m.insert("ReferenceSourceFileName".into(), json!(obj.source_file));
    if !e.properties.is_empty() && e.kind != ObjectKind::ControlAddIn {
        m.insert(
            "Properties".into(),
            Value::Array(
                e.properties
                    .iter()
                    .map(|p| object_property_json(p, resolver))
                    .collect(),
            ),
        );
    }
    // Profiles and control add-ins have no numeric object id.
    if !matches!(
        e.kind,
        ObjectKind::Profile | ObjectKind::ProfileExtension | ObjectKind::ControlAddIn
    ) {
        m.insert("Id".into(), json!(object_id(e, app_id)));
    }
    m.insert("Name".into(), json!(e.name));
    Value::Object(m)
}

/// Objects with a declared id use it; interfaces (no declared id in AL) get a
/// generated id reproduced from alc's `InterfaceObjectMembers` hash.
fn object_id(e: &SymbolEntry, app_id: &str) -> i32 {
    if e.id != 0 {
        e.id
    } else if e.kind == ObjectKind::Interface {
        interface_object_id(&e.name, app_id, e.implements.len(), e.methods.len())
    } else {
        0
    }
}

/// Generated interface object id — reverse-engineered from
/// `InterfaceObjectMembers` (alc): FNV of the quoted, upper-cased display name,
/// combined with FNV of the AppId GUID bytes, then combined with `-1` once per
/// extended-interface and once per method (alc hashes the property `Id`, which
/// is still `-1` during this computation — count, not value, contributes).
fn interface_object_id(name: &str, app_id: &str, extended: usize, methods: usize) -> i32 {
    let display = if needs_quoting(name) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    };
    let mut h = fnv1_hash(&display.to_uppercase());
    if let Some(bytes) = guid_to_bytes_le(app_id) {
        h = combine_hash(h, fnv1_hash_bytes(&bytes));
    }
    for _ in 0..(extended + methods) {
        h = combine_hash(h, -1);
    }
    h
}

/// AL quotes an identifier that isn't a bare identifier (contains a non
/// `[A-Za-z0-9_]` char, or starts with a digit). Matches alc's
/// `ToDisplayString` quoting for the interface-id hash.
fn needs_quoting(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => true,
        Some(c) if c.is_ascii_digit() => true,
        Some(c) if !(c.is_ascii_alphanumeric() || c == '_') => true,
        _ => name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_')),
    }
}

/// Parse a GUID string into .NET `Guid.ToByteArray()` byte order (first three
/// components little-endian, last eight as-written).
fn guid_to_bytes_le(guid: &str) -> Option<[u8; 16]> {
    let parts: Vec<&str> = guid.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    let tail_hex = format!("{}{}", parts[3], parts[4]); // 16 hex chars → 8 bytes
    if tail_hex.len() != 16 {
        return None;
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&d1.to_le_bytes());
    out[4..6].copy_from_slice(&d2.to_le_bytes());
    out[6..8].copy_from_slice(&d3.to_le_bytes());
    for i in 0..8 {
        out[8 + i] = u8::from_str_radix(&tail_hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The method's effective return type: an explicit one, or the implicit
/// `Boolean` of a `[TryFunction]`.
fn effective_return_type(mth: &MethodSymbol) -> Option<String> {
    if let Some(rt) = &mth.return_type {
        return Some(rt.clone());
    }
    if mth
        .attributes
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case("TryFunction"))
    {
        return Some("Boolean".to_string());
    }
    None
}

fn method_json(
    mth: &MethodSymbol,
    object_id: i32,
    is_interface: bool,
    resolver: &Resolver,
) -> Value {
    let mut m = Map::new();
    if let Some(rt) = effective_return_type(mth) {
        m.insert("ReturnTypeDefinition".into(), type_def_json(&rt, resolver));
    }
    if is_interface {
        // alc records interface methods with MethodKind 5 (interface method).
        m.insert("MethodKind".into(), json!(5));
    }
    if !mth.parameters.is_empty() {
        m.insert(
            "Parameters".into(),
            Value::Array(
                mth.parameters
                    .iter()
                    .map(|p| param_json(p, resolver))
                    .collect(),
            ),
        );
    }
    if !mth.attributes.is_empty() {
        m.insert(
            "Attributes".into(),
            Value::Array(mth.attributes.iter().map(attribute_json).collect()),
        );
    }
    m.insert("Id".into(), json!(method_id_for(mth, object_id, resolver)));
    m.insert("Name".into(), json!(mth.name));
    Value::Object(m)
}

fn method_id_for(mth: &MethodSymbol, object_id: i32, resolver: &Resolver) -> i32 {
    let return_kind = effective_return_type(mth)
        .as_deref()
        .map(nav_kind)
        .unwrap_or(0); // None = 0
    let disambiguate = method_requires_disambiguation(mth);
    let params: Vec<ParamSig> = mth
        .parameters
        .iter()
        .map(|p| ParamSig {
            kind: param_kind_for_hash(&p.type_name),
            is_var: p.is_var,
            subtype_hash: subtype_hash_for(&p.type_name, resolver),
        })
        .collect();
    method_id(
        &mth.name,
        return_kind,
        &params,
        disambiguate,
        object_id as i64,
    )
}

/// alc's `RequiresRuntimeOverloadDisambiguation`: the method is overloadable
/// (a normal procedure, not an event/subscriber) and has at least one parameter
/// whose type carries a subtype (Record/Enum/Codeunit/Page/Interface/…).
fn method_requires_disambiguation(mth: &MethodSymbol) -> bool {
    if is_event_or_subscriber(mth) {
        return false;
    }
    mth.parameters
        .iter()
        .any(|p| is_subtype_kind(&base_type_name(&p.type_name)))
}

fn is_event_or_subscriber(mth: &MethodSymbol) -> bool {
    mth.attributes.iter().any(|a| {
        matches!(
            a.name.to_ascii_lowercase().as_str(),
            "integrationevent"
                | "businessevent"
                | "internalevent"
                | "externalbusinessevent"
                | "eventsubscriber"
        )
    })
}

/// The kinds whose parameters carry a subtype (matching alc's
/// `IsParameterWithSubtype`).
fn is_subtype_kind(base: &str) -> bool {
    matches!(
        base.to_ascii_lowercase().as_str(),
        "record"
            | "table"
            | "codeunit"
            | "page"
            | "xmlport"
            | "query"
            | "enum"
            | "interface"
            | "dotnet"
            | "list"
            | "dictionary"
            | "testpage"
            | "testrequestpage"
    )
}

/// alc's `GetSubTypeHashCodeForNewVersions` (top-level): the referenced object's
/// id for object-reference kinds, `FNV(name)` for an interface, `1` for scalars.
fn subtype_hash_for(type_str: &str, resolver: &Resolver) -> i32 {
    let (base, subtype) = split_type(type_str);
    match base_type_name(type_str).to_ascii_lowercase().as_str() {
        "record" | "table" | "codeunit" | "page" | "xmlport" | "query" | "enum" | "testpage"
        | "testrequestpage" => subtype
            .as_deref()
            .and_then(|s| resolver.get(&s.to_lowercase()).map(|r| r.id))
            .unwrap_or(0),
        // Interface: FNV of the name (NOT upper-cased, unlike method names).
        "interface" => subtype.as_deref().map(fnv1_hash).unwrap_or(0),
        _ => {
            let _ = base;
            1
        }
    }
}

/// The base type keyword of an AL type string (before any `[len]` or subtype).
fn base_type_name(type_str: &str) -> String {
    let (base, _) = split_type(type_str);
    base.split('[').next().unwrap_or(&base).trim().to_string()
}

/// The `NavTypeKind` id for an AL type string, from the generated
/// `nav_type_kinds` data. Unknown types fall back to `None` (0).
fn nav_kind(type_str: &str) -> i32 {
    nav_type_kind_id(&base_type_name(type_str)).unwrap_or(0)
}

/// The parameter `NavTypeKind` id as it enters the method-id hash. alc masks the
/// `_ReturnValue`/`_ClrOption` flag bits (`~0x80001`) for the object-reference /
/// complex kinds in `ReturnValuesAddedForRuntimeVersion7` (`IgnoreReturnValue`).
fn param_kind_for_hash(type_str: &str) -> i32 {
    let kind = nav_kind(type_str);
    if ignore_return_value(&base_type_name(type_str)) {
        kind & !0x0008_0001
    } else {
        kind
    }
}

/// alc's `ReturnValuesAddedForRuntimeVersion7` set — the kinds whose param hash
/// masks the flag bits. RE'd from the compiler (not AL language data).
fn ignore_return_value(base: &str) -> bool {
    matches!(
        base.to_ascii_lowercase().as_str(),
        "table"
            | "testrequestpage"
            | "testpage"
            | "file"
            | "notification"
            | "recordref"
            | "fieldref"
            | "keyref"
            | "instream"
            | "outstream"
            | "variant"
            | "filterpagebuilder"
            | "sessionsettings"
            | "httpclient"
            | "httprequestmessage"
            | "httpresponsemessage"
            | "httpcontent"
            | "record"
            | "codeunit"
            | "page"
            | "report"
            | "xmlport"
            | "query"
            | "list"
            | "dictionary"
            | "interface"
            | "notificationscope"
            | "dateformula"
            | "recordid"
    )
}

fn param_json(p: &ParameterSymbol, resolver: &Resolver) -> Value {
    let mut m = Map::new();
    // alc orders a var parameter's `IsVar` flag before `Name`.
    if p.is_var {
        m.insert("IsVar".into(), json!(true));
    }
    m.insert("Name".into(), json!(p.name));
    m.insert(
        "TypeDefinition".into(),
        type_def_json(&p.type_name, resolver),
    );
    Value::Object(m)
}

/// Split an `OptionMembers` property value into its members, each unquoted
/// (`" ",Red` → `[" ", "Red"]`).
fn option_members(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|m| m.trim_matches('"').trim_matches('\'').to_string())
        .collect()
}

fn field_json(f: &FieldSymbol, resolver: &Resolver) -> Value {
    let mut m = Map::new();
    let mut type_def = type_def_json(&f.type_name, resolver);
    // An Option field embeds its members inside the TypeDefinition (after Name).
    if base_type_name(&f.type_name).eq_ignore_ascii_case("Option") {
        if let Some(p) = f
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("OptionMembers"))
        {
            if let Some(obj) = type_def.as_object_mut() {
                obj.insert(
                    "OptionMembers".into(),
                    Value::Array(
                        option_members(&p.value)
                            .into_iter()
                            .map(Value::String)
                            .collect(),
                    ),
                );
            }
        }
    }
    m.insert("TypeDefinition".into(), type_def);
    if !f.properties.is_empty() {
        m.insert(
            "Properties".into(),
            Value::Array(f.properties.iter().map(property_json).collect()),
        );
    }
    m.insert("Id".into(), json!(f.id));
    m.insert("Name".into(), json!(f.name));
    Value::Object(m)
}

fn key_json(k: &KeySymbol) -> Value {
    let mut m = Map::new();
    m.insert("FieldNames".into(), json!(k.field_names));
    if !k.properties.is_empty() {
        m.insert(
            "Properties".into(),
            Value::Array(k.properties.iter().map(property_json).collect()),
        );
    }
    m.insert("Name".into(), json!(k.name));
    Value::Object(m)
}

/// `ControlKind` value for a page-control keyword (RE'd from alc's enum).
fn control_kind(keyword: &str) -> i32 {
    match keyword {
        "area" => 0,
        "group" => 1,
        "cuegroup" => 2,
        "repeater" => 3,
        "fixed" => 4,
        "grid" => 5,
        "part" => 6,
        "systempart" => 7,
        "field" => 8,
        "label" => 9,
        "usercontrol" => 10,
        "chartpart" => 11,
        _ => 0,
    }
}

/// `ActionKind` value for a page-action keyword.
fn action_kind(keyword: &str) -> i32 {
    match keyword {
        "area" => 0,
        "group" => 1,
        "action" => 2,
        "separator" => 3,
        "actionref" => 4,
        "customaction" => 5,
        "systemaction" => 6,
        "fileuploadaction" => 7,
        _ => 0,
    }
}

fn member_id_for(object_id: i32, name: &str) -> i32 {
    member_id(&format!("{object_id}{name}"))
}

type LocalTypes = std::collections::HashMap<String, String>;

/// Resolve a field's `SourceExpression` to a type: `Rec."No."` → the source
/// table's field type; a bare name → a local variable's type (report request
/// pages). Project-local resolution only.
fn resolve_field_type(
    source_expr: Option<&String>,
    source_table: &str,
    ft: &FieldTypes,
    locals: &LocalTypes,
) -> String {
    let Some(expr) = source_expr else {
        return "None".to_string();
    };
    if let Some(field) = expr.strip_prefix("Rec.") {
        let field = field.trim_matches('"');
        return ft
            .get(&(source_table.to_lowercase(), field.to_lowercase()))
            .cloned()
            .unwrap_or_else(|| "None".to_string());
    }
    locals
        .get(&expr.trim_matches('"').to_lowercase())
        .cloned()
        .unwrap_or_else(|| "None".to_string())
}

/// Control/action property value. alc encodes option-valued props
/// (e.g. `ApplicationArea`) with a leading `#`.
fn control_property_json(p: &PropertyValue) -> Value {
    let value = if p.name.eq_ignore_ascii_case("ApplicationArea") {
        format!("#{}", p.value)
    } else {
        normalize_property_value(&p.value)
    };
    json!({ "Name": p.name, "Value": value })
}

#[allow(clippy::too_many_arguments)]
fn control_json(
    c: &PageControl,
    object_id: i32,
    source_table: &str,
    field_types: &FieldTypes,
    resolver: &Resolver,
    locals: &LocalTypes,
    inject_editable_false: bool,
) -> Value {
    let mut m = Map::new();
    let kind = control_kind(&c.keyword);
    if kind != 0 {
        m.insert("Kind".into(), json!(kind));
    }
    if !c.children.is_empty() {
        m.insert(
            "Controls".into(),
            Value::Array(
                c.children
                    .iter()
                    .map(|ch| {
                        control_json(
                            ch,
                            object_id,
                            source_table,
                            field_types,
                            resolver,
                            locals,
                            inject_editable_false,
                        )
                    })
                    .collect(),
            ),
        );
    }
    // Fields carry the resolved source type + a SourceExpression property;
    // containers get TypeDefinition None.
    if c.keyword == "field" {
        let ty = resolve_field_type(c.source_expr.as_ref(), source_table, field_types, locals);
        m.insert("TypeDefinition".into(), type_def_json(&ty, resolver));
    } else {
        m.insert("TypeDefinition".into(), json!({ "Name": "None" }));
    }
    let mut props: Vec<Value> = c.properties.iter().map(control_property_json).collect();
    // Page customizations force added fields non-editable (runtime < 16.0) unless
    // the author set Editable explicitly. alc records it before SourceExpression.
    if inject_editable_false
        && c.keyword == "field"
        && !c
            .properties
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case("Editable"))
    {
        props.push(json!({ "Name": "Editable", "Value": "False" }));
    }
    if c.keyword == "field" {
        if let Some(expr) = &c.source_expr {
            props.push(json!({ "Name": "SourceExpression", "Value": expr }));
        }
    }
    if !props.is_empty() {
        m.insert("Properties".into(), Value::Array(props));
    }
    m.insert("Id".into(), json!(member_id_for(object_id, &c.name)));
    m.insert("Name".into(), json!(c.name));
    Value::Object(m)
}

/// `ChangeKind` value for a page/report-extension change keyword (RE'd from
/// alc's `ChangeKind` enum).
fn change_kind(keyword: &str) -> i32 {
    match keyword {
        "add" => 0,
        "addfirst" => 1,
        "addlast" => 2,
        "addbefore" => 3,
        "addafter" => 4,
        "movefirst" => 5,
        "movelast" => 6,
        "movebefore" => 7,
        "moveafter" => 8,
        "modify" => 9,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn control_change_json(
    ch: &ControlChange,
    object_id: i32,
    source_table: &str,
    field_types: &FieldTypes,
    resolver: &Resolver,
    locals: &LocalTypes,
    inject_editable_false: bool,
) -> Value {
    let controls: Vec<Value> = ch
        .controls
        .iter()
        .map(|c| {
            control_json(
                c,
                object_id,
                source_table,
                field_types,
                resolver,
                locals,
                inject_editable_false,
            )
        })
        .collect();
    json!({
        "Anchor": ch.anchor,
        "ChangeKind": change_kind(&ch.kind),
        "Controls": controls,
    })
}

fn action_json(a: &PageControl, object_id: i32) -> Value {
    let mut m = Map::new();
    let kind = action_kind(&a.keyword);
    if kind != 0 {
        m.insert("Kind".into(), json!(kind));
    }
    if !a.children.is_empty() {
        m.insert(
            "Actions".into(),
            Value::Array(
                a.children
                    .iter()
                    .map(|ch| action_json(ch, object_id))
                    .collect(),
            ),
        );
    }
    let props: Vec<Value> = a.properties.iter().map(control_property_json).collect();
    if !props.is_empty() {
        m.insert("Properties".into(), Value::Array(props));
    }
    m.insert("Id".into(), json!(member_id_for(object_id, &a.name)));
    m.insert("Name".into(), json!(a.name));
    Value::Object(m)
}

fn permission_json(p: &PermissionDecl, resolver: &Resolver) -> Value {
    let mut m = Map::new();
    let code = permission_object_code(&p.object_type);
    if code != 0 {
        m.insert("PermissionObject".into(), json!(code));
    }
    m.insert("Value".into(), json!(permission_value(&p.permission)));
    let name = p.object_name.trim_matches('"');
    // System (code 10) objects are platform built-ins, resolved from the
    // generated system-object table; every other kind resolves against the
    // project + referenced-app objects.
    let id = if code == 10 {
        crate::syntax::language_data::system_object_id(name).unwrap_or(0)
    } else {
        resolver
            .get(&name.to_lowercase())
            .map(|r| r.id)
            .unwrap_or(0)
    };
    m.insert("Id".into(), json!(id));
    Value::Object(m)
}

/// `PermissionObjectType` codes (RE'd from alc; not AL language data).
fn permission_object_code(t: &str) -> i32 {
    match t.to_ascii_lowercase().as_str() {
        "tabledata" => 0,
        "table" => 1,
        "report" => 3,
        "codeunit" => 5,
        "xmlport" => 6,
        "page" => 8,
        "query" => 9,
        "system" => 10,
        _ => 0,
    }
}

/// Permission flags bitmask: R=1, I=2, M=4, D=8, X=16.
fn permission_value(perm: &str) -> i32 {
    perm.chars().fold(0, |acc, c| {
        acc | match c.to_ascii_uppercase() {
            'R' => 1,
            'I' => 2,
            'M' => 4,
            'D' => 8,
            'X' => 16,
            _ => 0,
        }
    })
}

/// Report / report-extension rendering layouts, in alc's `Layouts` shape:
/// `{ "Properties": [{Name,Value}…], "Name": … }` per `layout(...)`.
fn report_layouts_json(layouts: &[ReportLayout]) -> Value {
    Value::Array(
        layouts
            .iter()
            .map(|l| {
                let mut m = Map::new();
                if !l.properties.is_empty() {
                    m.insert(
                        "Properties".into(),
                        Value::Array(l.properties.iter().map(property_json).collect()),
                    );
                }
                m.insert("Name".into(), json!(l.name));
                Value::Object(m)
            })
            .collect(),
    )
}

/// Report / report-extension global variables, in alc's `Variables` shape.
fn variables_json(vars: &[VariableSymbol], resolver: &Resolver) -> Value {
    Value::Array(
        vars.iter()
            .map(|v| json!({ "TypeDefinition": type_def_json(&v.type_name, resolver), "Name": v.name }))
            .collect(),
    )
}

/// Record each report dataitem's related table (recursively), keyed by
/// `(report name, dataitem name)`, for resolving report-extension column types.
fn collect_dataitem_tables(
    els: &[QueryElement],
    report: &str,
    map: &mut std::collections::HashMap<(String, String), String>,
) {
    for el in els {
        if let Some(rt) = &el.related_table {
            map.insert((report.to_string(), el.name.to_lowercase()), rt.clone());
        }
        collect_dataitem_tables(&el.children, report, map);
    }
}

fn report_dataitem_json(
    el: &QueryElement,
    object_id: i32,
    field_types: &FieldTypes,
    resolver: &Resolver,
) -> Value {
    let mut m = Map::new();
    if let Some(rt) = &el.related_table {
        m.insert("RelatedTable".into(), json!(rt));
    }
    // GetFilterControlId: member_id(Name + "Report" + objectId).
    m.insert(
        "FilterControlId".into(),
        json!(member_id(&format!("{}Report{object_id}", el.name))),
    );
    let table = el.related_table.clone().unwrap_or_default().to_lowercase();
    m.insert(
        "Columns".into(),
        Value::Array(
            el.columns
                .iter()
                .map(|(name, source)| {
                    let ty = field_types
                        .get(&(table.clone(), source.trim_matches('"').to_lowercase()))
                        .cloned()
                        .unwrap_or_else(|| "None".to_string());
                    json!({
                        "OwningDataItemName": el.name,
                        "TypeDefinition": type_def_json(&ty, resolver),
                        "Id": member_id(name),
                        "Name": name,
                    })
                })
                .collect(),
        ),
    );
    m.insert(
        "DataItems".into(),
        Value::Array(
            el.children
                .iter()
                .map(|c| report_dataitem_json(c, object_id, field_types, resolver))
                .collect(),
        ),
    );
    m.insert("Id".into(), json!(member_id(&el.name)));
    m.insert("Name".into(), json!(el.name));
    Value::Object(m)
}

fn query_element_json(el: &QueryElement) -> Value {
    let mut m = Map::new();
    if let Some(rt) = &el.related_table {
        m.insert("RelatedTable".into(), json!(rt));
    }
    m.insert(
        "DataItems".into(),
        Value::Array(el.children.iter().map(query_element_json).collect()),
    );
    m.insert(
        "Columns".into(),
        Value::Array(
            el.columns
                .iter()
                .map(|(name, source)| {
                    json!({ "SourceColumn": source, "Id": member_id(name), "Name": name })
                })
                .collect(),
        ),
    );
    m.insert("Filters".into(), json!([]));
    m.insert("Id".into(), json!(member_id(&el.name)));
    m.insert("Name".into(), json!(el.name));
    Value::Object(m)
}

fn enum_value_json(v: &EnumValueSymbol, properties: &[PropertyValue]) -> Value {
    let mut m = Map::new();
    // alc orders Ordinal (omitted when 0) before Properties.
    if v.ordinal != 0 {
        m.insert("Ordinal".into(), json!(v.ordinal));
    }
    if !properties.is_empty() {
        m.insert(
            "Properties".into(),
            Value::Array(properties.iter().map(property_json).collect()),
        );
    }
    m.insert("Name".into(), json!(v.name));
    Value::Object(m)
}

fn property_json(p: &PropertyValue) -> Value {
    let value = if p.name.eq_ignore_ascii_case("OptionMembers") {
        // alc stores each option member unquoted (`" ",Red` → ` ,Red`).
        option_members(&p.value).join(",")
    } else {
        normalize_property_value(&p.value)
    };
    json!({ "Name": p.name, "Value": value })
}

/// Object-level property serialization. Object-reference properties that name
/// another object (`SourceTable`) are emitted as the resolved object id;
/// `IncludedPermissionSets` keeps each referenced name quoted-as-needed.
fn object_property_json(p: &PropertyValue, resolver: &Resolver) -> Value {
    if p.name.eq_ignore_ascii_case("SourceTable") {
        let name = p.value.trim_matches('"').to_lowercase();
        if let Some(r) = resolver.get(&name) {
            return json!({ "Name": p.name, "Value": r.id.to_string() });
        }
    }
    if p.name.eq_ignore_ascii_case("ApplicationArea") {
        // Object-level ApplicationArea is `#`-prefixed, like the control-level one.
        return json!({ "Name": p.name, "Value": format!("#{}", p.value) });
    }
    if p.name.eq_ignore_ascii_case("IncludedPermissionSets") {
        // A comma list of permission-set names, each quoted only when it isn't a
        // bare identifier (alc keeps `"PS A",PSC`).
        let value = p
            .value
            .split(',')
            .map(|s| {
                let n = s.trim().trim_matches('"').trim_matches('\'');
                if needs_quoting(n) {
                    format!("\"{n}\"")
                } else {
                    n.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        return json!({ "Name": p.name, "Value": value });
    }
    property_json(p)
}

fn attribute_json(a: &AttributeSymbol) -> Value {
    let mut m = Map::new();
    m.insert("Name".into(), json!(a.name));
    if !a.arguments.is_empty() {
        let args: Vec<Value> = a
            .arguments
            .iter()
            .map(|arg| json!({ "Value": normalize_attr_value(arg) }))
            .collect();
        m.insert("Arguments".into(), Value::Array(args));
    }
    Value::Object(m)
}

/// `{ "Name": ... }`, with a `Subtype { [ModuleId,] Name, Id }` for subtype-bearing
/// types (`Enum "X"`, `Record "X"`, …). A subtype defined in a *referenced* app
/// carries its defining module id first (`ModuleId`). Scalar/length types keep the
/// length in `Name` (alc emits `Text[100]` as the bare `Name`).
fn type_def_json(type_str: &str, resolver: &Resolver) -> Value {
    let (base, subtype) = split_type(type_str);
    if let Some(sub) = subtype {
        let mut st = Map::new();
        let r = resolver.get(&sub.to_lowercase());
        // External subtypes record the defining module id (before Name).
        if let Some(module_id) = r.and_then(|r| r.module_id.as_deref()) {
            st.insert("ModuleId".into(), json!(module_id));
        }
        st.insert("Name".into(), json!(sub));
        // alc emits Subtype.Id for objects with a real id (Enum/Record/…) but
        // not for interfaces (no declared numeric id).
        if let Some(id) = r.map(|r| r.id).filter(|&id| id != 0) {
            st.insert("Id".into(), json!(id));
        }
        json!({ "Name": base, "Subtype": Value::Object(st) })
    } else {
        json!({ "Name": base })
    }
}

/// Split a type string into (base name, optional subtype). `Enum "Color"` →
/// (`Enum`, `Color`); `Text[100]` → (`Text[100]`, None); `Integer` →
/// (`Integer`, None).
fn split_type(type_str: &str) -> (String, Option<String>) {
    if let Some(q1) = type_str.find('"') {
        let base = type_str[..q1].trim().to_string();
        let rest = &type_str[q1 + 1..];
        let sub = rest.rfind('"').map(|q2| rest[..q2].to_string());
        (base, sub)
    } else {
        (type_str.to_string(), None)
    }
}

/// Boolean property values serialise as `"1"`/`"0"` in `SymbolReference.json`.
fn normalize_property_value(v: &str) -> String {
    match v.to_ascii_lowercase().as_str() {
        "true" => "1".to_string(),
        "false" => "0".to_string(),
        _ => v.to_string(),
    }
}

/// alc capitalises boolean literal attribute arguments (`false` → `False`).
fn normalize_attr_value(v: &str) -> String {
    match v.to_ascii_lowercase().as_str() {
        "true" => "True".to_string(),
        "false" => "False".to_string(),
        _ => v.to_string(),
    }
}
