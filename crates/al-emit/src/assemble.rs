//! Assemble a complete `.app` from its parts.
//!
//! Combines manifest, symbols, metadata, resources, and source files into a
//! NAVX package.

use super::manifest::AppManifest;
use super::package::{write_app_package, EmitError};
use super::symbol_extract::EmitObject;
use al_symbols::model::ObjectKind;

/// A source file as it is stored in the `.app`. `archive_path` is the
/// in-archive path; alc stores project sources under a `src/` prefix, so a
/// project file `src/Hello.al` becomes archive path `src/src/Hello.al`.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub archive_path: String,
    pub content: String,
}

impl SourceFile {
    /// Build the archive path for a project-relative source path, matching alc's
    /// `src/` prefix.
    pub fn from_project_path(project_relative: &str, content: impl Into<String>) -> Self {
        let rel = project_relative.replace('\\', "/");
        SourceFile {
            archive_path: format!("src/{}", rel.trim_start_matches('/')),
            content: content.into(),
        }
    }
}

/// The OPC `[Content_Types].xml` part, with a `<Default>` per distinct file
/// extension present (in first-appearance order, as alc emits). Prefixed with the
/// UTF-8 BOM alc emits.
pub fn content_types_xml(extensions: &[String]) -> Vec<u8> {
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\
          <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    );
    for ext in extensions {
        out.extend_from_slice(
            format!("<Default Extension=\"{ext}\" ContentType=\"\" />").as_bytes(),
        );
    }
    out.extend_from_slice(b"</Types>");
    out
}

/// Distinct lowercase file extensions across `entries`, in first-appearance order
/// (the `[Content_Types].xml` ordering alc uses).
fn distinct_extensions(entries: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut exts: Vec<String> = Vec::new();
    for (name, _) in entries {
        if let Some(ext) = name.rsplit('.').next() {
            // The archive path "[Content_Types].xml" itself is added afterwards;
            // skip names without a real extension segment.
            if ext != name {
                let ext = ext.to_ascii_lowercase();
                if !exts.contains(&ext) {
                    exts.push(ext);
                }
            }
        }
    }
    exts
}

/// The `DocComments.xml` part (application header + an empty members list when
/// no doc comments are supplied).
pub fn doc_comments_xml(id: &str, name: &str, publisher: &str, version: &str) -> String {
    let e = super::manifest::xml_escape_text;
    format!(
        "<?xml version=\"1.0\"?>\n<doc>\n    <application>\n        <id>{}</id>\n        \
         <name>{}</name>\n        <publisher>{}</publisher>\n        <version>{}</version>\n    \
         </application>\n    <members>\n    </members>\n</doc>\n",
        e(id),
        e(name),
        e(publisher),
        e(version),
    )
}

/// The `MediaIdListing.xml` part (no media). Static, with alc's UTF-8 BOM.
pub fn media_id_listing_xml(logo_file: Option<&str>) -> Vec<u8> {
    let logo_file = logo_file.unwrap_or("");
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(
        format!(
            "<MediaIdListing LogoFileName=\"{}\" LogoId=\"\" \
             xmlns=\"http://schemas.microsoft.com/navx/2016/mediaidlisting\">\n  \
             <MediaSetIds />\n</MediaIdListing>",
            super::manifest::xml_escape_attr(logo_file)
        )
        .as_bytes(),
    );
    out
}

/// alc's implicit-entitlement rule (`CreateDefaultEntitlements`): per permission
/// object kind in `PermissionObjectType` order, emit a `<Permission>` for each
/// object of the mapped `ObjectKind`. `TableData` grants RIMD (15); everything
/// else grants Execute (16). A table object thus yields both a `TableData` and a
/// `Table` row. Returns `None` when the app has no permission-bearing objects.
pub fn entitlement_xml(app_id: &str, objects: &[EmitObject]) -> Option<Vec<u8>> {
    // (PermissionObjectType code, ObjectKind it lists, Value).
    const RULES: &[(i32, ObjectKind, i32)] = &[
        (0, ObjectKind::Table, 15),
        (1, ObjectKind::Table, 16),
        (3, ObjectKind::Report, 16),
        (5, ObjectKind::Codeunit, 16),
        (6, ObjectKind::XmlPort, 16),
        (8, ObjectKind::Page, 16),
        (9, ObjectKind::Query, 16),
    ];
    let mut perms = String::new();
    for (type_code, kind, value) in RULES {
        let mut ids: Vec<i32> = objects
            .iter()
            .filter(|o| o.entry.kind == *kind)
            .map(|o| o.entry.id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            perms.push_str(&format!(
                "    <Permission Type=\"{type_code}\" ID=\"{id}\" Value=\"{value}\" />\n"
            ));
        }
    }
    if perms.is_empty() {
        return None;
    }
    let app_id = super::manifest::xml_escape_text(app_id);
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Entitlement MetadataVersion=\"130000\" \
             Name=\"{app_id}\" Type=\"Implicit\" \
             xmlns=\"urn:schemas-microsoft-com:dynamics:NAV:MetaObjects\">\n  \
             <ObjectEntitlements>\n{perms}  </ObjectEntitlements>\n</Entitlement>"
        )
        .as_bytes(),
    );
    Some(out)
}

/// The XLIFF trans-unit id segment hash: `(long)FNV(name) + int.MaxValue`
/// (`LanguageFileUtilities.GetNameHash`).
fn name_hash(s: &str) -> i64 {
    super::method_id::fnv1_hash(s) as i64 + 2_147_483_647
}

fn caption_value(props: &[al_symbols::model::PropertyValue]) -> Option<&str> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("Caption"))
        .map(|p| p.value.as_str())
}

fn property_value<'a>(
    props: &'a [al_symbols::model::PropertyValue],
    name: &str,
) -> Option<&'a str> {
    props
        .iter()
        .find(|property| property.name.eq_ignore_ascii_case(name))
        .map(|property| property.value.as_str())
}

fn trans_unit(id: &str, source: &str, note: &str, object_target: Option<&str>) -> String {
    let src = super::manifest::xml_escape_text(source);
    let target_attr = match object_target {
        Some(t) => format!(
            " al-object-target=\"{}\"",
            super::manifest::xml_escape_text(t)
        ),
        None => String::new(),
    };
    format!(
        "        <trans-unit id=\"{}\" size-unit=\"char\" translate=\"yes\" xml:space=\"preserve\"{target_attr}>\n          \
         <source>{src}</source>\n          <target>{src}</target>\n          \
         <note from=\"Developer\" annotates=\"general\" priority=\"2\"></note>\n          \
         <note from=\"Xliff Generator\" annotates=\"general\" priority=\"3\">{}</note>\n        \
         </trans-unit>\n",
        super::manifest::xml_escape_text(id),
        super::manifest::xml_escape_text(note),
    )
}

/// The base object kind name for an extension/customization object (the object it
/// folds onto for translation ids), or `None` for a non-extension object. Matches
/// alc's `IsExtensionOrCustomizationObject` set.
fn base_object_kind(kind: ObjectKind) -> Option<&'static str> {
    match kind {
        ObjectKind::TableExtension => Some("Table"),
        ObjectKind::PageExtension => Some("Page"),
        ObjectKind::ReportExtension => Some("Report"),
        ObjectKind::EnumExtension => Some("Enum"),
        ObjectKind::PermissionSetExtension => Some("PermissionSet"),
        ObjectKind::ProfileExtension => Some("Profile"),
        ObjectKind::PageCustomization => Some("Page"),
        _ => None,
    }
}

/// One pending xliff item before id-ordering.
struct XliffItem {
    id: String,
    source: String,
    note: String,
    object_target: Option<String>,
}

struct XliffObjectContext<'a> {
    root_kind: &'a str,
    root_name: &'a str,
    declaration_kind: &'a str,
    declaration_name: &'a str,
    object_target: &'a Option<String>,
}

fn push_page_control_xliff_items(
    items: &mut Vec<XliffItem>,
    controls: &[super::symbol_extract::PageControl],
    member_kind: &str,
    context: &XliffObjectContext<'_>,
) {
    for control in controls {
        // A control name is the stable handle used in SymbolReference and the
        // translation id. Anonymous grammar-recovery nodes have no compatible
        // address, so do not manufacture an unstable XLIFF unit for them.
        if !control.name.is_empty() {
            let control_hash = name_hash(&control.name);
            for property in ["Caption", "ToolTip"] {
                let Some(value) = property_value(&control.properties, property) else {
                    continue;
                };
                items.push(XliffItem {
                    id: format!(
                        "{} {} - {member_kind} {control_hash} - Property {}",
                        context.root_kind,
                        name_hash(context.root_name),
                        name_hash(property)
                    ),
                    source: value.to_string(),
                    note: format!(
                        "{} {} - {member_kind} {} - Property {property}",
                        context.declaration_kind, context.declaration_name, control.name
                    ),
                    object_target: context.object_target.clone(),
                });
            }
        }
        push_page_control_xliff_items(items, &control.children, member_kind, context);
    }
}

fn push_page_change_xliff_items(
    items: &mut Vec<XliffItem>,
    changes: &[super::symbol_extract::ControlChange],
    context: &XliffObjectContext<'_>,
) {
    for change in changes {
        // alc emits a modified control's ToolTip, but not its Caption, into the
        // extension's translation source.
        if change.kind.eq_ignore_ascii_case("modify") {
            if let Some(value) = property_value(&change.properties, "ToolTip") {
                items.push(XliffItem {
                    id: format!(
                        "{} {} - Change {} - Property {}",
                        context.root_kind,
                        name_hash(context.root_name),
                        name_hash(&change.anchor),
                        name_hash("ToolTip")
                    ),
                    source: value.to_string(),
                    note: format!(
                        "{} {} - Change {} - Property ToolTip",
                        context.declaration_kind, context.declaration_name, change.anchor
                    ),
                    object_target: context.object_target.clone(),
                });
            }
        }
        push_page_control_xliff_items(items, &change.controls, "Control", context);
    }
}

/// The `TextData/*.xliff` translation source, reproducing alc's `TextDataVisitor`
/// (runtime Fall2024 / 14.x). Items are emitted for:
/// - Table/TableExtension object captions and their *field* captions,
/// - Page/PageExtension object captions, named control/action captions and
///   tooltips, plus page-extension added-control captions/ToolTips and modified
///   control ToolTips (runtime ≥ 14.0),
/// - Report/RequestPage captions and named request-page control captions and
///   tooltips (runtime ≥ 15.0).
///
/// Enum/Query/PermissionSet/Profile/Codeunit/Interface/XmlPort captions are never
/// emitted. Extension members fold their id-root onto the base object and carry an
/// `al-object-target`; the developer note keeps the declaring object. Trans-unit
/// ids use the cracked `GetLanguageSymbolId` path hash (`{Kind} {FNV(name)+i32::MAX}`).
/// `original` is alc's literal `"TextDataApp"`. Returns `None` when nothing is
/// translatable. Same-app extension targets fold to the base; cross-app targets
/// (base not in this project) keep the declaring object as the id-root.
pub fn xliff_xml(objects: &[EmitObject], runtime_major: u32) -> Option<Vec<u8>> {
    let cap_hash = name_hash("Caption");
    let present: std::collections::HashSet<String> = objects
        .iter()
        .map(|o| o.entry.name.to_lowercase())
        .collect();
    let mut items: Vec<XliffItem> = Vec::new();
    for o in objects {
        let kind = o.entry.kind;
        let decl_kind = kind.to_string();
        let base_kind = base_object_kind(kind);
        let base_name = o.entry.extends.clone();
        let (root_kind, root_name) = match (base_kind, &base_name) {
            // Fold the id-root onto the base only when it is in this project.
            (Some(bk), Some(bn)) if present.contains(&bn.to_lowercase()) => (bk, bn.clone()),
            _ => (decl_kind.as_str(), o.entry.name.clone()),
        };
        let object_target = match (base_kind, &base_name) {
            (Some(bk), Some(bn)) => Some(format!("{bk} {}", name_hash(bn))),
            _ => None,
        };
        let context = XliffObjectContext {
            root_kind,
            root_name: &root_name,
            declaration_kind: &decl_kind,
            declaration_name: &o.entry.name,
            object_target: &object_target,
        };

        let emit_object_caption = matches!(kind, ObjectKind::Table | ObjectKind::TableExtension)
            || (matches!(kind, ObjectKind::Page | ObjectKind::PageExtension)
                && runtime_major >= 14)
            || (matches!(kind, ObjectKind::Report | ObjectKind::ReportExtension)
                && runtime_major >= 15);
        if emit_object_caption {
            if let Some(cap) = caption_value(&o.entry.properties) {
                items.push(XliffItem {
                    id: format!(
                        "{root_kind} {} - Property {cap_hash}",
                        name_hash(&root_name)
                    ),
                    source: cap.to_string(),
                    note: format!("{decl_kind} {} - Property Caption", o.entry.name),
                    object_target: object_target.clone(),
                });
            }
        }
        if matches!(kind, ObjectKind::Table | ObjectKind::TableExtension) {
            for f in &o.entry.fields {
                if let Some(cap) = caption_value(&f.properties) {
                    items.push(XliffItem {
                        id: format!(
                            "{root_kind} {} - Field {} - Property {cap_hash}",
                            name_hash(&root_name),
                            name_hash(&f.name)
                        ),
                        source: cap.to_string(),
                        note: format!(
                            "{decl_kind} {} - Field {} - Property Caption",
                            o.entry.name, f.name
                        ),
                        object_target: object_target.clone(),
                    });
                }
            }
        }
        if matches!(kind, ObjectKind::Page | ObjectKind::PageExtension) && runtime_major >= 14 {
            push_page_control_xliff_items(&mut items, &o.page_controls, "Control", &context);
            push_page_control_xliff_items(&mut items, &o.page_actions, "Action", &context);
            if kind == ObjectKind::PageExtension {
                push_page_change_xliff_items(&mut items, &o.control_changes, &context);
            }
        }
        if matches!(kind, ObjectKind::Report | ObjectKind::ReportExtension) && runtime_major >= 15 {
            push_page_control_xliff_items(&mut items, &o.page_controls, "Control", &context);
        }
    }
    if items.is_empty() {
        return None;
    }
    // alc orders trans-units by id, ordinal-ignore-case (stable).
    items.sort_by_key(|it| it.id.to_ascii_uppercase());
    let mut units = String::new();
    for it in &items {
        units.push_str(&trans_unit(
            &it.id,
            &it.source,
            &it.note,
            it.object_target.as_deref(),
        ));
    }
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<xliff version=\"1.2\" \
             xmlns=\"urn:oasis:names:tc:xliff:document:1.2\" \
             xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xsi:schemaLocation=\"urn:oasis:names:tc:xliff:document:1.2 \
             xliff-core-1.2-transitional.xsd\">\n  <file datatype=\"xml\" \
             source-language=\"en-US\" target-language=\"en-US\" original=\"TextDataApp\">\n    \
             <body>\n      <group id=\"body\">\n"
            .as_bytes(),
    );
    out.extend_from_slice(units.as_bytes());
    out.extend_from_slice(b"      </group>\n    </body>\n  </file>\n</xliff>");
    Some(out)
}

/// The `navigation.xml` part. alc's `CanGenerateNavigationMetadata` emits:
/// - an `ActionContainers/Departments` **new entry** for every Page/Report/Query
///   with a non-`None` `UsageCategory` (run-object action + caption / search-term
///   translation keys), and
/// - a `NavigationChanges/ActionChange` **delta entry** for every page extension
///   (targeting the base page's id + kind).
///
/// New-entry `ControlGUID`s are `Guid.NewGuid()` in alc — inherently
/// non-deterministic, so this part is functionally faithful but not byte-stable
/// (like the NAVX package GUID). The delta path is deterministic. `app_name` is
/// the action-group caption. Returns `None` when both sections are empty.
pub fn navigation_xml(
    objects: &[EmitObject],
    app_name: &str,
) -> Result<Option<Vec<u8>>, EmitError> {
    let ids: std::collections::HashMap<String, i32> = objects
        .iter()
        .map(|o| (o.entry.name.to_lowercase(), o.entry.id))
        .collect();
    let resolve = |name: &str| ids.get(&name.trim_matches('"').to_lowercase()).copied();
    let cap_hash = name_hash("Caption");
    let terms_hash = name_hash("AdditionalSearchTerms");
    let e = super::manifest::xml_escape_attr;

    let mut new_entries = String::new();
    for o in objects {
        if !matches!(
            o.entry.kind,
            ObjectKind::Page | ObjectKind::Report | ObjectKind::Query
        ) {
            continue;
        }
        let usage = o
            .entry
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("UsageCategory"))
            .map(|p| p.value.clone());
        let Some(usage) = usage.filter(|u| !u.eq_ignore_ascii_case("None")) else {
            continue;
        };
        let kind = o.entry.kind.to_string();
        // Run-object source table: a page's SourceTable, else a report/query's
        // first related dataitem table.
        let src_table_id = match o.entry.kind {
            ObjectKind::Page => o
                .entry
                .properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("SourceTable"))
                .and_then(|p| resolve(&p.value)),
            _ => o
                .query_elements
                .first()
                .and_then(|el| el.related_table.as_deref())
                .and_then(resolve),
        }
        .unwrap_or(0);
        let department = if usage.eq_ignore_ascii_case("ReportsAndAnalysis") {
            "Reports and Analysis".to_string()
        } else {
            usage
        };
        let mut attrs = format!(
            " xsi:type=\"ActionDefinition\" PushAction=\"RunObject\" RunObjectType=\"{kind}\" \
             TargetID=\"{}\" DepartmentCategory=\"{}\" RunObjectSrcTable=\"{src_table_id}\" \
             ControlGUID=\"{}\" Name=\"{}\"",
            o.entry.id,
            e(&department),
            super::package::random_guid_braced()?,
            e(&o.entry.name),
        );
        // alc materializes the object's name as both defaults when the AL
        // source omits Caption/AdditionalSearchTerms. Omitting the XML
        // attributes changes navigation search and translation behavior even
        // though the source-level properties appear optional.
        let caption = caption_value(&o.entry.properties).unwrap_or(&o.entry.name);
        attrs.push_str(&format!(" CaptionML=\"ENU={}\"", e(caption)));
        if let Some(area) = o
            .entry
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("ApplicationArea"))
        {
            attrs.push_str(&format!(" ApplicationArea=\"#{}\"", e(&area.value)));
        }
        let terms = o
            .entry
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("AdditionalSearchTerms"))
            .map_or_else(|| o.entry.name.clone(), |p| p.value.clone());
        attrs.push_str(&format!(" AdditionalSearchTermsML=\"ENU={}\"", e(&terms)));
        attrs.push_str(&format!(
            " CaptionTranslationKey=\"{kind} {} - Property {cap_hash}\"",
            name_hash(&o.entry.name)
        ));
        attrs.push_str(&format!(
            " AdditionalSearchTermsTranslationKey=\"{kind} {} - Property {terms_hash}\"",
            name_hash(&o.entry.name)
        ));
        new_entries.push_str(&format!("      <Actions{attrs} />\n"));
    }

    let mut changes = String::new();
    for o in objects {
        if o.entry.kind != ObjectKind::PageExtension {
            continue;
        }
        let Some(tid) = o.entry.extends.as_deref().and_then(resolve) else {
            continue;
        };
        let mut attrs = format!(" TargetID=\"{tid}\" TargetType=\"Page\"");
        if let Some(cap) = caption_value(&o.entry.properties) {
            attrs.push_str(&format!(" CaptionML=\"{}\"", e(cap)));
        }
        changes.push_str(&format!("    <ActionChange{attrs} />\n"));
    }

    if new_entries.is_empty() && changes.is_empty() {
        return Ok(None);
    }
    let mut body = String::new();
    if !new_entries.is_empty() {
        body.push_str(&format!(
            "  <ActionContainers ActionContainerType=\"Departments\">\n    \
             <Actions xsi:type=\"ActionGroupDefinition\" Caption=\"{}\" ControlGUID=\"{}\">\n\
             {new_entries}    </Actions>\n  </ActionContainers>\n",
            e(app_name),
            super::package::random_guid_braced()?,
        ));
    }
    if !changes.is_empty() {
        body.push_str(&format!(
            "  <NavigationChanges>\n{changes}  </NavigationChanges>\n"
        ));
    }
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<NavigationDefinition \
             xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" \
             xmlns=\"urn:schemas-microsoft-com:dynamics:NAV:MetaObjects\">\n\
             {body}</NavigationDefinition>"
        )
        .as_bytes(),
    );
    Ok(Some(out))
}

/// A control add-in's resolved resources: local file references (bundled into the
/// add-in zip) split from external URLs, plus the inline script contents.
#[derive(Default)]
struct AddinResources {
    local_scripts: Vec<String>,
    local_stylesheets: Vec<String>,
    images: Vec<String>,
    script_urls: Vec<String>,
    stylesheet_urls: Vec<String>,
    startup_script: Option<String>,
    refresh_script: Option<String>,
    recreate_script: Option<String>,
}

/// Split a list-valued property (`Scripts = 'a', 'b';`) into trimmed items.
fn list_prop(props: &[al_symbols::model::PropertyValue], name: &str) -> Vec<String> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| {
            p.value
                .split(',')
                .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Resolve a control add-in's resources, reading local script/stylesheet/image
/// and inline-script files relative to `project_root` (skipped when `None`).
fn resolve_addin_resources(
    props: &[al_symbols::model::PropertyValue],
    project_root: Option<&std::path::Path>,
) -> Result<AddinResources, EmitError> {
    let read = |rel: &str| -> Result<Option<String>, EmitError> {
        project_root
            .map(|root| std::fs::read_to_string(root.join(rel)).map_err(EmitError::from))
            .transpose()
    };
    let mut r = AddinResources::default();
    for s in list_prop(props, "Scripts") {
        if is_url(&s) {
            r.script_urls.push(s);
        } else {
            r.local_scripts.push(s);
        }
    }
    for s in list_prop(props, "StyleSheets") {
        if is_url(&s) {
            r.stylesheet_urls.push(s);
        } else {
            r.local_stylesheets.push(s);
        }
    }
    r.images = list_prop(props, "Images");
    let inline = |name: &str| -> Result<Option<String>, EmitError> {
        match props.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
            Some(property) => read(&property.value),
            None => Ok(None),
        }
    };
    r.startup_script = inline("StartupScript")?;
    r.refresh_script = inline("RefreshScript")?;
    r.recreate_script = inline("RecreateScript")?;
    Ok(r)
}

/// A control add-in's `manifest.xml`, reproducing alc's
/// `ControlAddInManifest.ToString()` element order exactly: `Resources` (local
/// Script/StyleSheet/Image refs), `Script` (StartupScript, CDATA), `ScriptUrls`,
/// `StyleSheetUrls`, `RefreshScript`/`RecreateScript` (CDATA), the six dimension
/// props (when set), the four stretch/shrink booleans (true only, in order
/// VerticalShrink/VerticalStretch/HorizontalShrink/HorizontalStretch), then
/// `Version`. alc renders this via `XDocument.ToString()`: 2-space indented, no
/// XML prolog, no BOM.
fn control_addin_manifest_xml(
    props: &[al_symbols::model::PropertyValue],
    res: &AddinResources,
) -> String {
    let e = super::manifest::xml_escape_text;
    let prop = |name: &str| {
        props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.value.as_str())
    };

    let mut x = String::from("<Manifest>\n");
    if res.local_scripts.is_empty() && res.local_stylesheets.is_empty() && res.images.is_empty() {
        x.push_str("  <Resources />\n");
    } else {
        x.push_str("  <Resources>\n");
        for s in &res.local_scripts {
            x.push_str(&format!("    <Script>{}</Script>\n", e(s)));
        }
        for s in &res.local_stylesheets {
            x.push_str(&format!("    <StyleSheet>{}</StyleSheet>\n", e(s)));
        }
        for s in &res.images {
            x.push_str(&format!("    <Image>{}</Image>\n", e(s)));
        }
        x.push_str("  </Resources>\n");
    }
    // CDATA inline scripts (`]]>` split so it cannot terminate the section early).
    let cdata = |x: &mut String, tag: &str, content: &Option<String>| {
        if let Some(c) = content {
            let safe = c.replace("]]>", "]]]]><![CDATA[>");
            x.push_str(&format!("  <{tag}><![CDATA[{safe}]]></{tag}>\n"));
        }
    };
    cdata(&mut x, "Script", &res.startup_script);

    let push_url_list = |x: &mut String, container: &str, item: &str, list: &[String]| {
        if list.is_empty() {
            x.push_str(&format!("  <{container} />\n"));
        } else {
            x.push_str(&format!("  <{container}>\n"));
            for u in list {
                x.push_str(&format!("    <{item}>{}</{item}>\n", e(u)));
            }
            x.push_str(&format!("  </{container}>\n"));
        }
    };
    push_url_list(&mut x, "ScriptUrls", "ScriptUrl", &res.script_urls);
    push_url_list(
        &mut x,
        "StyleSheetUrls",
        "StyleSheetUrl",
        &res.stylesheet_urls,
    );
    cdata(&mut x, "RefreshScript", &res.refresh_script);
    cdata(&mut x, "RecreateScript", &res.recreate_script);

    for dim in [
        "RequestedHeight",
        "RequestedWidth",
        "MinimumHeight",
        "MinimumWidth",
        "MaximumHeight",
        "MaximumWidth",
    ] {
        if let Some(v) = prop(dim).and_then(|v| v.trim().parse::<i64>().ok()) {
            x.push_str(&format!("  <{dim}>{v}</{dim}>\n"));
        }
    }
    for b in [
        "VerticalShrink",
        "VerticalStretch",
        "HorizontalShrink",
        "HorizontalStretch",
    ] {
        if prop(b)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            x.push_str(&format!("  <{b}>True</{b}>\n"));
        }
    }
    x.push_str("  <Version>2</Version>\n</Manifest>");
    x
}

/// The control-add-in resource bundle entries: one `addin/<MetadataName>.zip`
/// (an OPC zip of the add-in's bundled local resource files, then `manifest.xml`,
/// then `[Content_Types].xml`) per add-in, plus a single `addin/controladdins.dock`
/// listing them all. Empty when the project has no control add-ins. `app_name`
/// derives each add-in's strong-name token; `project_root` resolves local
/// resource files (skipped when `None`).
fn control_addin_bundle(
    objects: &[EmitObject],
    app_name: &str,
    project_root: Option<&std::path::Path>,
) -> Result<Vec<(String, Vec<u8>)>, EmitError> {
    let addins: Vec<&EmitObject> = objects
        .iter()
        .filter(|o| o.entry.kind == ObjectKind::ControlAddIn)
        .collect();
    if addins.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut docket = String::from("<ControlAddInDocket>\n");
    for o in &addins {
        let meta_name = super::symbol_reference::metadata_name(&o.entry.name);
        let token = o
            .entry
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("PublicKeyToken"))
            .map(|p| p.value.clone())
            .unwrap_or_else(|| super::symbol_reference::control_addin_public_key_token(app_name));
        let version = o
            .entry
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("Version"))
            .map(|p| p.value.clone())
            .unwrap_or_default();
        let res = resolve_addin_resources(&o.entry.properties, project_root)?;

        // Inner OPC zip: bundled local resource files (scripts, stylesheets,
        // images, in that order), then manifest.xml, then [Content_Types].xml
        // (whose Defaults cover every extension present).
        let mut inner_entries: Vec<(String, Vec<u8>)> = Vec::new();
        for rel in res
            .local_scripts
            .iter()
            .chain(&res.local_stylesheets)
            .chain(&res.images)
        {
            let content = match project_root {
                Some(root) => std::fs::read(root.join(rel))?,
                None => Vec::new(),
            };
            inner_entries.push((rel.clone(), content));
        }
        inner_entries.push((
            "manifest.xml".to_string(),
            control_addin_manifest_xml(&o.entry.properties, &res).into_bytes(),
        ));
        let exts = distinct_extensions(&inner_entries);
        inner_entries.push(("[Content_Types].xml".to_string(), content_types_xml(&exts)));
        out.push((
            format!("addin/{meta_name}.zip"),
            super::package::write_zip(&inner_entries)?,
        ));
        // alc also places each local add-in asset in the outer package beneath
        // `addin/src/`, in addition to bundling it in the add-in ZIP.  The
        // duplicated outer copy is part of the package layout consumers see.
        for rel in res
            .local_scripts
            .iter()
            .chain(&res.local_stylesheets)
            .chain(&res.images)
        {
            let content = match project_root {
                Some(root) => std::fs::read(root.join(rel))?,
                None => Vec::new(),
            };
            out.push((format!("addin/src/{rel}"), content));
        }

        let e = super::manifest::xml_escape_attr;
        docket.push_str(&format!(
            "  <ControlAddIn Name=\"{}\" Version=\"{}\" Token=\"{}\" FileName=\"{}.zip\" \
             Type=\"JavaScriptControlAddIn\" />\n",
            e(&meta_name),
            e(&version),
            e(&token),
            e(&meta_name),
        ));
    }
    docket.push_str("</ControlAddInDocket>");
    out.push(("addin/controladdins.dock".to_string(), docket.into_bytes()));
    Ok(out)
}

/// Read report layout files relative to the project root.
fn project_relative_resource_path(
    value: &str,
    kind: &str,
) -> Result<std::path::PathBuf, EmitError> {
    let normalized = value.replace('\\', "/");
    let path = std::path::Path::new(&normalized);
    if normalized.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(EmitError::Project(format!(
            "{kind} path must be a non-empty project-relative path: {value:?}"
        )));
    }
    Ok(path.to_path_buf())
}

fn read_project_resource(
    root: &std::path::Path,
    value: &str,
    kind: &str,
) -> Result<(String, Vec<u8>), EmitError> {
    let relative = project_relative_resource_path(value, kind)?;
    let root = root.canonicalize().map_err(|error| {
        EmitError::Project(format!(
            "resolving project root {}: {error}",
            root.display()
        ))
    })?;
    let candidate = root.join(&relative);
    let resolved = candidate.canonicalize().map_err(|error| {
        EmitError::Project(format!("reading {kind} {}: {error}", candidate.display()))
    })?;
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return Err(EmitError::Project(format!(
            "{kind} must resolve to a regular file inside the project: {value:?}"
        )));
    }
    let archive_relative = relative.to_string_lossy().replace('\\', "/");
    let content = std::fs::read(&resolved).map_err(|error| {
        EmitError::Project(format!("reading {kind} {}: {error}", resolved.display()))
    })?;
    Ok((archive_relative, content))
}

fn validate_logo_resource(
    manifest: &AppManifest,
    project_root: Option<&std::path::Path>,
) -> Result<Option<String>, EmitError> {
    if manifest.logo.trim().is_empty() {
        return Ok(None);
    }
    let root = project_root.ok_or_else(|| {
        EmitError::Project("cannot resolve app.json logo without a project root".to_string())
    })?;
    let (relative, _) = read_project_resource(root, &manifest.logo, "app.json logo")?;
    if !relative.eq_ignore_ascii_case("res") && !relative.to_ascii_lowercase().starts_with("res/") {
        return Err(EmitError::Project(format!(
            "app.json logo must live under res/: {:?}",
            manifest.logo
        )));
    }
    let file_name = std::path::Path::new(&relative)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            EmitError::Project(format!("invalid app.json logo path: {:?}", manifest.logo))
        })?;
    Ok(Some(format!("logo/{file_name}")))
}

fn report_layout_files(
    objects: &[EmitObject],
    project_root: Option<&std::path::Path>,
) -> Result<Vec<(String, Vec<u8>)>, EmitError> {
    let mut out = Vec::new();
    for o in objects {
        if !matches!(
            o.entry.kind,
            ObjectKind::Report | ObjectKind::ReportExtension
        ) {
            continue;
        }
        for layout in &o.report_layouts {
            if let Some(file) = layout
                .properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("LayoutFile"))
            {
                let root = project_root.ok_or_else(|| {
                    EmitError::Project(format!(
                        "cannot resolve layout file {} without a project root",
                        file.value
                    ))
                })?;
                let (rel, content) = read_project_resource(root, &file.value, "report layout")?;
                out.push((format!("layout/{rel}"), content));
            }
        }
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// Percent-encode a `.app` archive path component the way alc does (spaces →
/// `%20`), keeping unreserved characters.
fn encode_path_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Assemble a complete `.app` from its manifest, source files, extracted
/// `objects` (for the implicit entitlement), and a `SymbolReference.json`
/// payload. Entry order matches alc's layout (manifest, sources, doc comments,
/// entitlement, symbols, media listing, content-types).
pub fn assemble_app(
    manifest: &AppManifest,
    sources: &[SourceFile],
    objects: &[EmitObject],
    symbol_reference_json: &[u8],
    package_guid: [u8; 16],
    project_root: Option<&std::path::Path>,
) -> Result<Vec<u8>, EmitError> {
    let runtime_major = if manifest.runtime.is_empty() {
        0
    } else {
        manifest
            .runtime
            .split('.')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| {
                EmitError::Project(format!("invalid runtime version: {:?}", manifest.runtime))
            })?
    };

    // `app.json` names the source file under `res/`, but alc records and
    // packages application logos as `/logo/<file-name>`.
    let logo_file = validate_logo_resource(manifest, project_root)?;
    let mut package_manifest = manifest.clone();
    if let Some(archive_path) = &logo_file {
        package_manifest.logo = format!("/{archive_path}");
    }

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(sources.len() + 8);
    entries.push((
        "NavxManifest.xml".to_string(),
        package_manifest.to_navx_xml().into_bytes(),
    ));
    for s in sources {
        entries.push((s.archive_path.clone(), s.content.clone().into_bytes()));
    }
    entries.push((
        "DocComments.xml".to_string(),
        doc_comments_xml(
            &manifest.id,
            &manifest.name,
            &manifest.publisher,
            &manifest.version,
        )
        .into_bytes(),
    ));
    if let Some(ent) = entitlement_xml(&manifest.id, objects) {
        entries.push((format!("entitlement/{}.xml", manifest.id), ent));
    }
    // alc prefixes the symbol JSON files with a UTF-8 BOM.
    entries.push((
        "SymbolReference.json".to_string(),
        with_bom(symbol_reference_json),
    ));
    // Per-profile symbol reference files follow the main one.
    let meta = super::symbol_reference::SymbolRefMeta {
        runtime_version: manifest.runtime.clone(),
        app_id: manifest.id.clone(),
        name: manifest.name.clone(),
        publisher: manifest.publisher.clone(),
        version: manifest.version.clone(),
    };
    // Profiles reference no cross-app object ids, so an empty external resolver
    // suffices for their per-profile symbol files.
    let empty_external = super::symbol_reference::ExternalSymbols::default();
    for (path, bytes) in
        super::symbol_reference::build_profile_symbol_references(objects, &meta, &empty_external)
    {
        entries.push((path, bytes));
    }
    entries.push((
        "MediaIdListing.xml".to_string(),
        media_id_listing_xml(logo_file.as_deref()),
    ));
    if let Some(archive_path) = logo_file {
        let root = project_root.expect("validated app.json logo has a project root");
        let (_, content) = read_project_resource(root, &manifest.logo, "app.json logo")?;
        // alc stores the app icon under `logo/<file-name>` rather than under
        // the source `res/` path and records that archive path in MediaIdListing.
        entries.push((archive_path, content));
    }
    for entry in control_addin_bundle(objects, &manifest.name, project_root)? {
        entries.push(entry);
    }
    for entry in report_layout_files(objects, project_root)? {
        entries.push(entry);
    }
    if let Some(nav) = navigation_xml(objects, &manifest.name)? {
        entries.push(("navigation.xml".to_string(), nav));
    }
    if let Some(xliff) = xliff_xml(objects, runtime_major) {
        let file = format!(
            "TextData/{}.TextData.en-US.xliff",
            encode_path_component(&manifest.name)
        );
        entries.push((file, xliff));
    }
    let mut seen = std::collections::HashSet::new();
    if let Some((duplicate, _)) = entries.iter().find(|(path, _)| !seen.insert(path)) {
        return Err(EmitError::Project(format!(
            "multiple package parts resolve to the same archive path: {duplicate}"
        )));
    }
    // `[Content_Types].xml` is last and lists every distinct extension present.
    let extensions = distinct_extensions(&entries);
    entries.push((
        "[Content_Types].xml".to_string(),
        content_types_xml(&extensions),
    ));
    write_app_package(&entries, package_guid)
}

/// Prepend the UTF-8 BOM to `data` unless it already starts with one.
fn with_bom(data: &[u8]) -> Vec<u8> {
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + 3);
    out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::app_inspect::list_app_entries;
    use al_symbols::manifest::parse_manifest;

    fn manifest() -> AppManifest {
        AppManifest::from_app_json(
            &serde_json::json!({
                "id": "aaaaaaaa-1111-2222-3333-444444444444",
                "name": "Min App", "publisher": "Spike", "version": "1.0.0.0",
                "platform": "26.0.0.0", "application": "26.5.0.0", "runtime": "14.0",
                "target": "Cloud", "idRanges": [{ "from": 50100, "to": 50149 }]
            }),
            "17.0.34.45391",
            "2026-06-15T20:58:33.0000000Z",
        )
    }

    #[test]
    fn source_archive_path_gets_src_prefix() {
        let sf = SourceFile::from_project_path("src/Hello.al", "codeunit 50100 X {}");
        assert_eq!(sf.archive_path, "src/src/Hello.al");
    }

    #[test]
    fn assembles_app_with_alc_matching_entry_set() {
        let sources = vec![SourceFile::from_project_path(
            "src/Hello.al",
            "codeunit 50100 \"Spike Hello\" { procedure Greet(): Text begin exit('hi'); end; }",
        )];
        let app = assemble_app(
            &manifest(),
            &sources,
            &[],
            br#"{"RuntimeVersion":"14.0","Codeunits":[]}"#,
            [9u8; 16],
            None,
        )
        .unwrap();

        // Reads back through our own unpacker with the expected entries (no
        // entitlement here — `objects` is empty).
        let contents = list_app_entries(&app).unwrap();
        let names: Vec<&str> = contents.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "NavxManifest.xml",
                "src/src/Hello.al",
                "DocComments.xml",
                "SymbolReference.json",
                "MediaIdListing.xml",
                "[Content_Types].xml",
            ]
        );
        assert!(contents.has_source());
        assert!(!contents.has_compiled_code());

        let mx = contents
            .entries
            .iter()
            .find(|e| e.name == "NavxManifest.xml")
            .unwrap();
        assert_eq!(mx.kind, al_symbols::app_inspect::AppEntryKind::Xml);
    }

    #[test]
    fn navigation_xml_emits_action_change_for_page_extension() {
        let src = "page 50100 \"Card\" { PageType = Card; }\n\
                   pageextension 50101 \"Card Ext\" extends \"Card\" { }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let nav = navigation_xml(&objects, "App")
            .unwrap()
            .expect("navigation.xml expected");
        assert_eq!(&nav[..3], &[0xEF, 0xBB, 0xBF], "BOM");
        let s = String::from_utf8_lossy(&nav);
        assert!(s.contains("<NavigationChanges>"));
        assert!(s.contains("<ActionChange TargetID=\"50100\" TargetType=\"Page\" />"));
    }

    #[test]
    fn navigation_xml_none_without_extensions_or_usage_category() {
        let src = "page 50100 \"Card\" { PageType = Card; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        assert!(navigation_xml(&objects, "App").unwrap().is_none());
    }

    #[test]
    fn navigation_xml_emits_new_entry_for_usage_category_page() {
        let src = "table 50100 \"Rec\" { fields { field(1; \"No.\"; Code[20]) { } } }\n\
                   page 50100 \"My List\" { PageType = List; SourceTable = \"Rec\"; \
                   Caption = 'My List'; UsageCategory = Lists; ApplicationArea = All; \
                   AdditionalSearchTerms = 'foo,bar'; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let nav = navigation_xml(&objects, "Nav App")
            .unwrap()
            .expect("navigation.xml");
        let s = String::from_utf8_lossy(&nav);
        assert!(s.contains("<ActionContainers ActionContainerType=\"Departments\">"));
        assert!(s.contains("xsi:type=\"ActionGroupDefinition\" Caption=\"Nav App\""));
        assert!(s.contains("PushAction=\"RunObject\" RunObjectType=\"Page\" TargetID=\"50100\""));
        assert!(s.contains("DepartmentCategory=\"Lists\" RunObjectSrcTable=\"50100\""));
        assert!(s.contains("Name=\"My List\" CaptionML=\"ENU=My List\" ApplicationArea=\"#All\""));
        assert!(s.contains("AdditionalSearchTermsML=\"ENU=foo,bar\""));
        // Caption/search-term translation keys use the same FNV path hash as xliff.
        assert!(s.contains(&format!(
            "CaptionTranslationKey=\"Page {} - Property {}\"",
            name_hash("My List"),
            name_hash("Caption")
        )));
        assert!(s.contains(&format!(
            "AdditionalSearchTermsTranslationKey=\"Page {} - Property {}\"",
            name_hash("My List"),
            name_hash("AdditionalSearchTerms")
        )));
    }

    #[test]
    fn navigation_xml_materializes_alc_name_defaults() {
        let src = "table 50100 \"Rec\" { fields { field(1; F; Integer) { } } }\n\
                   page 50100 \"Default Caption\" { PageType = List; SourceTable = \"Rec\"; \
                   UsageCategory = Lists; ApplicationArea = All; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let nav = navigation_xml(&objects, "Nav App")
            .unwrap()
            .expect("navigation.xml");
        let text = String::from_utf8_lossy(&nav);
        assert!(text.contains("CaptionML=\"ENU=Default Caption\""));
        assert!(text.contains("AdditionalSearchTermsML=\"ENU=Default Caption\""));
        assert!(text.contains(&format!(
            "CaptionTranslationKey=\"Page {} - Property {}\"",
            name_hash("Default Caption"),
            name_hash("Caption")
        )));
        assert!(text.contains(&format!(
            "AdditionalSearchTermsTranslationKey=\"Page {} - Property {}\"",
            name_hash("Default Caption"),
            name_hash("AdditionalSearchTerms")
        )));
    }

    #[test]
    fn xliff_includes_page_and_field_captions_excludes_report_at_runtime_14() {
        let src =
            "table 50100 \"T\" { fields { field(1; F; Integer) { Caption = 'Field Cap'; } } }\n\
                   page 50100 \"P\" { Caption = 'Page Cap'; }\n\
                   report 50100 \"R\" { Caption = 'Report Cap'; }\n\
                   enum 50100 \"E\" { value(0; V) { Caption = 'Enum Cap'; } }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let x = xliff_xml(&objects, 14).expect("xliff expected");
        let s = String::from_utf8_lossy(&x);
        assert!(s.contains("original=\"TextDataApp\""), "literal original");
        assert!(s.contains(">Page Cap<"), "page object caption included");
        assert!(s.contains(">Field Cap<"), "table field caption included");
        assert!(
            !s.contains("Report Cap"),
            "report caption excluded at runtime 14"
        );
        assert!(!s.contains("Enum Cap"), "enum value caption never included");
    }

    #[test]
    fn xliff_table_extension_field_folds_to_base_with_object_target() {
        let src = "table 50100 \"Base T\" { fields { field(1; A; Integer) { } } }\n\
                   tableextension 50101 \"Ext T\" extends \"Base T\" { \
                   fields { field(50100; B; Integer) { Caption = 'B Cap'; } } }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let x = xliff_xml(&objects, 14).expect("xliff expected");
        let s = String::from_utf8_lossy(&x);
        // id-root folds onto the base table; al-object-target names the base.
        assert!(
            s.contains("al-object-target=\"Table "),
            "extension field carries al-object-target"
        );
        // The developer note keeps the declaring TableExtension.
        assert!(s.contains("TableExtension Ext T - Field B - Property Caption"));
    }

    #[test]
    fn xliff_includes_page_control_tooltips_and_action_captions() {
        let src = r#"page 50100 "Customer Card"
{
    layout
    {
        area(Content)
        {
            group(General)
            {
                Caption = 'General details';
                field(Name; Rec.Name)
                {
                    ToolTip = 'Specifies the customer name.';
                }
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(RefreshCustomer)
            {
                Caption = 'Refresh customer';
                ToolTip = 'Refreshes the current customer.';
            }
        }
    }
}"#;
        let objects = super::super::symbol_extract::extract_objects(src, "src/Page.al");
        let xml = String::from_utf8(xliff_xml(&objects, 14).expect("page XLIFF")).unwrap();

        for value in [
            "General details",
            "Specifies the customer name.",
            "Refresh customer",
            "Refreshes the current customer.",
        ] {
            assert!(
                xml.contains(value),
                "missing page-control XLIFF value {value}:\n{xml}"
            );
        }
        assert!(xml.contains("Control"));
        assert!(xml.contains("Property ToolTip"));
        assert!(xml.contains("Property Caption"));
    }

    #[test]
    fn xliff_page_extension_includes_added_controls_and_modified_tooltip_only() {
        let src = r#"page 50100 "Customer Card"
{
    layout { area(Content) { group(General) { field(Name; Rec.Name) { } } } }
}
pageextension 50101 "Customer Card Ext" extends "Customer Card"
{
    layout
    {
        modify(Name)
        {
            Caption = 'Changed caption';
            ToolTip = 'Changed tooltip';
        }
        addlast(General)
        {
            field(Note; Rec.Note)
            {
                Caption = 'Added caption';
                ToolTip = 'Added tooltip';
            }
        }
    }
}"#;
        let objects = super::super::symbol_extract::extract_objects(src, "src/Page.al");
        let xml = String::from_utf8(xliff_xml(&objects, 14).expect("page XLIFF")).unwrap();

        for value in ["Changed tooltip", "Added caption", "Added tooltip"] {
            assert!(
                xml.contains(value),
                "missing page-extension XLIFF value {value}:\n{xml}"
            );
        }
        assert!(
            !xml.contains("Changed caption"),
            "alc does not extract modify-control captions"
        );
    }

    #[test]
    fn xliff_includes_request_page_but_not_rendering_layout_captions_at_runtime_15() {
        let src = r#"report 50100 "Customer List"
{
    requestpage
    {
        layout
        {
            area(Content)
            {
                field(ShowBlocked; ShowBlocked)
                {
                    Caption = 'Include blocked customers';
                    ToolTip = 'Includes blocked customers in the report.';
                }
            }
        }
    }
    rendering
    {
        layout(CustomerLayout)
        {
            Type = RDLC;
            LayoutFile = 'layout/Customer.rdl';
            Caption = 'Customer printable layout';
        }
    }
}"#;
        let objects = super::super::symbol_extract::extract_objects(src, "src/Report.al");
        let xml = String::from_utf8(xliff_xml(&objects, 15).expect("report XLIFF")).unwrap();
        for value in [
            "Include blocked customers",
            "Includes blocked customers in the report.",
        ] {
            assert!(
                xml.contains(value),
                "missing report XLIFF value {value}:\n{xml}"
            );
        }
        assert!(
            !xml.contains("Customer printable layout"),
            "alc does not extract report-layout captions into XLIFF"
        );
    }

    #[test]
    fn control_addin_bundle_manifest_and_docket() {
        let src = "controladdin \"My Addin\" { RequestedHeight = 100; MinimumHeight = 50; \
                   VerticalStretch = true; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let bundle = control_addin_bundle(&objects, "Types App", None).expect("bundle");
        assert_eq!(bundle.len(), 2);
        assert_eq!(bundle[0].0, "addin/My_Addin.zip");
        assert_eq!(bundle[1].0, "addin/controladdins.dock");
        let dock = String::from_utf8_lossy(&bundle[1].1);
        // Token = SHA256("Types App")[..8] (the app name), verified against alc.
        assert!(dock.contains(
            "<ControlAddIn Name=\"My_Addin\" Version=\"\" Token=\"bbb326a7d514dda0\" \
             FileName=\"My_Addin.zip\" Type=\"JavaScriptControlAddIn\" />"
        ));
        // Manifest element order: dimensions then stretch/shrink (true only) then Version.
        let res = resolve_addin_resources(&objects[0].entry.properties, None).unwrap();
        let manifest = control_addin_manifest_xml(&objects[0].entry.properties, &res);
        assert_eq!(
            manifest,
            "<Manifest>\n  <Resources />\n  <ScriptUrls />\n  <StyleSheetUrls />\n  \
             <RequestedHeight>100</RequestedHeight>\n  <MinimumHeight>50</MinimumHeight>\n  \
             <VerticalStretch>True</VerticalStretch>\n  <Version>2</Version>\n</Manifest>"
        );
    }

    #[test]
    fn control_addin_local_resources_embed_and_split_urls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.js"), "init();\n").unwrap();
        let src = "controladdin \"Loc\" { Scripts = 'src/main.js', 'https://cdn/x.js'; \
                   StartupScript = 'src/main.js'; RequestedHeight = 100; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");
        let res = resolve_addin_resources(&objects[0].entry.properties, Some(dir.path())).unwrap();
        // Local file vs external URL split.
        assert_eq!(res.local_scripts, vec!["src/main.js"]);
        assert_eq!(res.script_urls, vec!["https://cdn/x.js"]);
        // StartupScript file content embedded.
        assert_eq!(res.startup_script.as_deref(), Some("init();\n"));
        let m = control_addin_manifest_xml(&objects[0].entry.properties, &res);
        assert!(m.contains("<Resources>\n    <Script>src/main.js</Script>\n  </Resources>"));
        assert!(m.contains("<Script><![CDATA[init();\n]]></Script>"));
        assert!(m.contains("<ScriptUrl>https://cdn/x.js</ScriptUrl>"));

        let bundle = control_addin_bundle(&objects, "App", Some(dir.path())).unwrap();
        assert!(bundle
            .iter()
            .any(|(path, bytes)| { path == "addin/src/src/main.js" && bytes == b"init();\n" }));
    }

    #[test]
    fn control_addin_missing_local_resource_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let src = "controladdin Missing { Scripts = 'src/missing.js'; }";
        let objects = super::super::symbol_extract::extract_objects(src, "src/Lib.al");

        let error = control_addin_bundle(&objects, "App", Some(dir.path()))
            .expect_err("missing local resource must fail");
        assert!(matches!(error, EmitError::Io(_)));
    }

    #[test]
    fn content_types_lists_distinct_extensions_in_order() {
        let entries = vec![
            ("NavxManifest.xml".to_string(), vec![]),
            ("src/x.al".to_string(), vec![]),
            ("S.json".to_string(), vec![]),
            ("TextData/a.xliff".to_string(), vec![]),
        ];
        let exts = distinct_extensions(&entries);
        assert_eq!(exts, vec!["xml", "al", "json", "xliff"]);
        let ct = String::from_utf8_lossy(&content_types_xml(&exts)).into_owned();
        for e in ["xml", "al", "json", "xliff"] {
            assert!(ct.contains(&format!("<Default Extension=\"{e}\" ContentType=\"\" />")));
        }
    }

    #[test]
    fn embedded_manifest_parses() {
        let app = assemble_app(&manifest(), &[], &[], b"{}", [1u8; 16], None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        al_symbols::app_inspect::extract_app(&app, dir.path()).unwrap();
        let xml = std::fs::read(dir.path().join("NavxManifest.xml")).unwrap();
        let m = parse_manifest(&xml).unwrap();
        assert_eq!(m.name, "Min App");
        assert_eq!(m.app_id, "aaaaaaaa-1111-2222-3333-444444444444");
    }
}
