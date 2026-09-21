//! Permission set generation from workspace objects.
//!
//! Scans the workspace FileIndex for AL object declarations, maps each object
//! type to its appropriate permission level, and renders the result as either
//! an AL `permissionset` object or an XML permission set file.

use std::fmt::Write;
use std::path::PathBuf;

use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionEntry {
    /// The permission object type (e.g. "tabledata", "page", "codeunit").
    pub object_type: String,
    pub object_name: String,
    /// The AL object ID (None for unnumbered objects).
    pub object_id: Option<i64>,
    /// The permission string (e.g. "RIMD" for tabledata, "X" for executables).
    pub permissions: String,
}

/// A workspace file that contributed no permission entry, and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: String,
}

/// The permission entries, plus the files that could not contribute one.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PermissionCollection {
    pub entries: Vec<PermissionEntry>,
    pub skipped: Vec<SkippedFile>,
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionCollectionError {
    /// The workspace could not be snapshotted coherently: an indexed path has
    /// no cached parse, or the workspace changed mid-collection. A partial
    /// permission set is indistinguishable from a complete one, so this still
    /// fails the whole query.
    #[error("cannot generate permissions: {0}")]
    Workspace(String),
}

/// Collect a permission entry for every workspace object that has one.
///
/// Extension objects (tableextension, pageextension, …) contribute nothing —
/// they extend objects that carry their own permissions.
///
/// A file with a syntax error, no object declaration or a missing object ID is
/// reported in `skipped` rather than failing the call. Real projects contain
/// scratch and work-in-progress sources, and one of those used to make
/// permission generation unavailable for the entire project. This is the same
/// skip-and-report policy `workspace_sources::snapshot_with_skipped` applies to
/// the other whole-project queries, and it runs through that function so the
/// two cannot drift.
pub fn collect_permissions(
    workspace: &Workspace,
) -> Result<PermissionCollection, PermissionCollectionError> {
    let (sources, skipped) = crate::workspace_sources::snapshot_with_skipped(workspace)
        .map_err(|error| PermissionCollectionError::Workspace(error.to_string()))?;

    let mut entries = Vec::new();
    for source in sources {
        let info = source.object.info;
        if let Some((perm_type, perm_value)) = permission_for_kind(&info.kind) {
            entries.push(PermissionEntry {
                object_type: perm_type.to_string(),
                object_name: info.name,
                object_id: info.id,
                permissions: perm_value.to_string(),
            });
        }
    }

    // Sort by type then name for deterministic output
    entries.sort_by(|a, b| {
        a.object_type
            .cmp(&b.object_type)
            .then(a.object_name.cmp(&b.object_name))
    });

    let mut skipped: Vec<SkippedFile> = skipped
        .into_iter()
        .map(|(path, reason)| SkippedFile { path, reason })
        .collect();
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(PermissionCollection { entries, skipped })
}

pub fn render_al(entries: &[PermissionEntry], name: &str, id: i64) -> String {
    let mut out = String::new();
    writeln!(out, "permissionset {id} \"{}\"", al_escape_name(name)).unwrap();
    writeln!(out, "{{").unwrap();
    writeln!(out, "    Assignable = true;").unwrap();

    if !entries.is_empty() {
        writeln!(out, "    Permissions =").unwrap();
        for (i, e) in entries.iter().enumerate() {
            let sep = if i + 1 < entries.len() { "," } else { ";" };
            writeln!(
                out,
                "        {type} \"{name}\" = {perm}{sep}",
                type = e.object_type,
                name = al_escape_name(&e.object_name),
                perm = e.permissions,
            )
            .unwrap();
        }
    }

    writeln!(out, "}}").unwrap();
    out
}

/// Escape a name for use inside AL double-quoted identifiers.
///
/// AL uses `""` to represent a literal double-quote inside a quoted identifier.
/// Made `pub(crate)` so generators / scaffolders in sibling modules can share
/// the same convention — duplicating it would risk one site forgetting to
/// escape and emitting unparseable AL.
pub(crate) fn al_escape_name(name: &str) -> String {
    name.replace('"', "\"\"")
}

pub fn render_xml(entries: &[PermissionEntry], role_id: &str, role_name: &str) -> String {
    let mut out = String::new();
    writeln!(out, r#"<?xml version="1.0" encoding="utf-8"?>"#).unwrap();
    writeln!(out, "<PermissionSets>").unwrap();
    writeln!(
        out,
        r#"  <PermissionSet RoleID="{}" RoleName="{}">"#,
        xml_escape_attr(role_id),
        xml_escape_attr(role_name),
    )
    .unwrap();

    for e in entries {
        writeln!(out, "    <Permission>").unwrap();
        writeln!(
            out,
            "      <ObjectType>{}</ObjectType>",
            xml_object_type(&e.object_type)
        )
        .unwrap();
        if let Some(id) = e.object_id {
            writeln!(out, "      <ObjectID>{id}</ObjectID>").unwrap();
        } else {
            writeln!(out, "      <ObjectID>0</ObjectID>").unwrap();
        }

        let (r, i, m, d, x) = xml_permission_flags(&e.permissions);
        writeln!(out, "      <ReadPermission>{r}</ReadPermission>").unwrap();
        writeln!(out, "      <InsertPermission>{i}</InsertPermission>").unwrap();
        writeln!(out, "      <ModifyPermission>{m}</ModifyPermission>").unwrap();
        writeln!(out, "      <DeletePermission>{d}</DeletePermission>").unwrap();
        writeln!(out, "      <ExecutePermission>{x}</ExecutePermission>").unwrap();
        writeln!(out, "    </Permission>").unwrap();
    }

    writeln!(out, "  </PermissionSet>").unwrap();
    writeln!(out, "</PermissionSets>").unwrap();
    out
}

fn permission_for_kind(kind: &str) -> Option<(&'static str, &'static str)> {
    let ot = al_syntax::language_data::object_type_by_keyword(kind)?;
    let perm_type = ot.permission_type.as_deref()?;
    let perm_value = ot.permission_value.as_deref()?;
    Some((perm_type, perm_value))
}

/// Escape a string for safe embedding in an XML attribute value (double-quoted).
///
/// Escapes the five XML special characters: `&`, `<`, `>`, `"`, `'`.
fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Convert an internal permission type (e.g. `"tabledata"`, `"page"`) to the BC
/// permission-set XML `ObjectType` schema name (e.g. `"TableData"`, `"Page"`).
/// The XML names are fixed BC schema constants — not AL keywords — but the *set*
/// of permission types that must be covered is an AL language fact, verified
/// against `language_data` by the `xml_object_type_covers_*` test so a new
/// permission type can't silently fall through to a schema-invalid lowercase name.
fn xml_object_type(perm_type: &str) -> &str {
    match perm_type {
        "tabledata" => "TableData",
        "page" => "Page",
        "codeunit" => "Codeunit",
        "report" => "Report",
        "xmlport" => "XMLport",
        "query" => "Query",
        other => other,
    }
}

fn xml_permission_flags(perms: &str) -> (u8, u8, u8, u8, u8) {
    let r = if perms.contains('R') { 1 } else { 0 };
    let i = if perms.contains('I') { 1 } else { 0 };
    let m = if perms.contains('M') { 1 } else { 0 };
    let d = if perms.contains('D') { 1 } else { 0 };
    let x = if perms.contains('X') { 1 } else { 0 };
    (r, i, m, d, x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn xml_object_type_covers_every_permission_type_in_language_data() {
        // Data-driven completeness guard: every permission_type language_data
        // declares must map to a CamelCase BC schema name. A new permission_type
        // added to the data without a mapping here fails this test instead of
        // emitting a lowercase, schema-invalid <Object Type="..."> in the XML.
        for ot in al_syntax::language_data::object_types() {
            let Some(pt) = ot.permission_type.as_deref() else {
                continue;
            };
            let xml = xml_object_type(pt);
            assert_ne!(
                xml, pt,
                "permission_type '{pt}' falls through xml_object_type unmapped"
            );
            assert!(
                xml.chars().next().is_some_and(|c| c.is_ascii_uppercase()),
                "xml_object_type('{pt}') = '{xml}' must be a CamelCase BC schema name"
            );
        }
    }

    #[test]
    fn permission_for_table_is_rimd() {
        let result = permission_for_kind("table");
        assert_eq!(result, Some(("tabledata", "RIMD")));
    }

    #[test]
    fn permission_for_page_is_execute() {
        let result = permission_for_kind("page");
        assert_eq!(result, Some(("page", "X")));
    }

    #[test]
    fn permission_for_codeunit_is_execute() {
        let result = permission_for_kind("codeunit");
        assert_eq!(result, Some(("codeunit", "X")));
    }

    #[test]
    fn permission_for_report_is_execute() {
        let result = permission_for_kind("report");
        assert_eq!(result, Some(("report", "X")));
    }

    #[test]
    fn permission_for_xmlport_is_execute() {
        let result = permission_for_kind("xmlport");
        assert_eq!(result, Some(("xmlport", "X")));
    }

    #[test]
    fn permission_for_query_is_execute() {
        let result = permission_for_kind("query");
        assert_eq!(result, Some(("query", "X")));
    }

    #[test]
    fn extension_objects_return_none() {
        assert_eq!(permission_for_kind("tableextension"), None);
        assert_eq!(permission_for_kind("pageextension"), None);
        assert_eq!(permission_for_kind("reportextension"), None);
        assert_eq!(permission_for_kind("enumextension"), None);
    }

    #[test]
    fn non_permissioned_objects_return_none() {
        assert_eq!(permission_for_kind("enum"), None);
        assert_eq!(permission_for_kind("interface"), None);
        assert_eq!(permission_for_kind("profile"), None);
        assert_eq!(permission_for_kind("pagecustomization"), None);
        assert_eq!(permission_for_kind("controladdin"), None);
        assert_eq!(permission_for_kind("permissionset"), None);
        assert_eq!(permission_for_kind("permissionsetextension"), None);
        assert_eq!(permission_for_kind("entitlement"), None);
    }

    #[test]
    fn collect_permissions_from_workspace() {
        let workspace = Workspace::new();

        workspace.file_index.add_file(
            PathBuf::from("/project/MyTable.al"),
            r#"table 50100 "My Table" { fields { field(1; Code; Code[20]) { } } }"#.to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/project/MyPage.al"),
            r#"page 50100 "My Page" { SourceTable = "My Table"; }"#.to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/project/MyCU.al"),
            r#"codeunit 50100 "My Codeunit" { procedure Run() begin end; }"#.to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/project/MyReport.al"),
            r#"report 50100 "My Report" { }"#.to_string(),
        );
        // Extension object — should be skipped
        workspace.file_index.add_file(
            PathBuf::from("/project/MyTableExt.al"),
            r#"tableextension 50100 "My Table Ext" extends "Customer" { }"#.to_string(),
        );
        // Enum — should be skipped
        workspace.file_index.add_file(
            PathBuf::from("/project/MyEnum.al"),
            r#"enum 50100 "My Enum" { value(0; None) { } }"#.to_string(),
        );

        let mut entries = collect_permissions(&workspace).unwrap().entries;
        entries.sort_by(|a, b| {
            a.object_type
                .cmp(&b.object_type)
                .then(a.object_name.cmp(&b.object_name))
        });

        assert_eq!(
            entries.len(),
            4,
            "Should have 4 entries (table, page, codeunit, report)"
        );

        let cu = entries
            .iter()
            .find(|e| e.object_type == "codeunit")
            .unwrap();
        assert_eq!(cu.object_name, "My Codeunit");
        assert_eq!(cu.object_id, Some(50100));
        assert_eq!(cu.permissions, "X");

        let pg = entries.iter().find(|e| e.object_type == "page").unwrap();
        assert_eq!(pg.object_name, "My Page");
        assert_eq!(pg.permissions, "X");

        let rp = entries.iter().find(|e| e.object_type == "report").unwrap();
        assert_eq!(rp.object_name, "My Report");
        assert_eq!(rp.permissions, "X");

        let td = entries
            .iter()
            .find(|e| e.object_type == "tabledata")
            .unwrap();
        assert_eq!(td.object_name, "My Table");
        assert_eq!(td.object_id, Some(50100));
        assert_eq!(td.permissions, "RIMD");
    }

    #[test]
    fn render_al_generates_valid_permissionset() {
        let entries = vec![
            PermissionEntry {
                object_type: "tabledata".into(),
                object_name: "My Table".into(),
                object_id: Some(50100),
                permissions: "RIMD".into(),
            },
            PermissionEntry {
                object_type: "codeunit".into(),
                object_name: "My Codeunit".into(),
                object_id: Some(50101),
                permissions: "X".into(),
            },
            PermissionEntry {
                object_type: "page".into(),
                object_name: "My Page".into(),
                object_id: Some(50102),
                permissions: "X".into(),
            },
        ];

        let output = render_al(&entries, "My Extension Permissions", 50100);

        assert!(output.contains(r#"permissionset 50100 "My Extension Permissions""#));
        assert!(output.contains("Assignable = true;"));
        assert!(output.contains(r#"tabledata "My Table" = RIMD"#));
        assert!(output.contains(r#"codeunit "My Codeunit" = X"#));
        assert!(output.contains(r#"page "My Page" = X"#));
    }

    #[test]
    fn render_al_empty_entries() {
        let output = render_al(&[], "Empty Perms", 50100);
        assert!(output.contains(r#"permissionset 50100 "Empty Perms""#));
        assert!(output.contains("Assignable = true;"));
    }

    #[test]
    fn render_xml_generates_valid_xml() {
        let entries = vec![
            PermissionEntry {
                object_type: "tabledata".into(),
                object_name: "My Table".into(),
                object_id: Some(50100),
                permissions: "RIMD".into(),
            },
            PermissionEntry {
                object_type: "codeunit".into(),
                object_name: "My Codeunit".into(),
                object_id: Some(50101),
                permissions: "X".into(),
            },
        ];

        let output = render_xml(&entries, "MY EXT", "My Extension Permissions");

        assert!(output.contains(r#"<?xml version="1.0" encoding="utf-8"?>"#));
        assert!(output.contains(r#"RoleID="MY EXT""#));
        assert!(output.contains(r#"RoleName="My Extension Permissions""#));
        assert!(output.contains("<ObjectType>TableData</ObjectType>"));
        assert!(output.contains("<ObjectID>50100</ObjectID>"));
        assert!(output.contains("<ReadPermission>1</ReadPermission>"));
        assert!(output.contains("<InsertPermission>1</InsertPermission>"));
        assert!(output.contains("<ModifyPermission>1</ModifyPermission>"));
        assert!(output.contains("<DeletePermission>1</DeletePermission>"));
        assert!(output.contains("<ObjectType>Codeunit</ObjectType>"));
        assert!(output.contains("<ObjectID>50101</ObjectID>"));
        assert!(output.contains("<ExecutePermission>1</ExecutePermission>"));
    }

    #[test]
    fn render_xml_empty_entries() {
        let output = render_xml(&[], "EMPTY", "Empty");
        assert!(output.contains(r#"<?xml version="1.0" encoding="utf-8"?>"#));
        assert!(output.contains(r#"RoleID="EMPTY""#));
    }

    #[test]
    fn xml_escape_attr_escapes_special_chars() {
        assert_eq!(xml_escape_attr("A&B"), "A&amp;B");
        assert_eq!(xml_escape_attr("A<B"), "A&lt;B");
        assert_eq!(xml_escape_attr("A>B"), "A&gt;B");
        assert_eq!(xml_escape_attr("A\"B"), "A&quot;B");
        assert_eq!(xml_escape_attr("A'B"), "A&apos;B");
        assert_eq!(xml_escape_attr("plain"), "plain");
        assert_eq!(xml_escape_attr(""), "");
    }

    #[test]
    fn render_xml_escapes_role_id_and_name() {
        let output = render_xml(&[], "A&B", "Name \"Quoted\" <Here>");
        assert!(output.contains(r#"RoleID="A&amp;B""#));
        assert!(output.contains(r#"RoleName="Name &quot;Quoted&quot; &lt;Here&gt;""#));
    }

    #[test]
    fn al_escape_name_doubles_quotes() {
        assert_eq!(al_escape_name("My Table"), "My Table");
        assert_eq!(al_escape_name(r#"Say "Hello""#), r#"Say ""Hello"""#);
        assert_eq!(al_escape_name(""), "");
    }

    #[test]
    fn render_al_escapes_object_name_with_quotes() {
        let entries = vec![PermissionEntry {
            object_type: "codeunit".into(),
            object_name: r#"Say "Hello""#.into(),
            object_id: Some(50200),
            permissions: "X".into(),
        }];
        let output = render_al(&entries, "My \"Perms\"", 50100);
        assert!(output.contains(r#"permissionset 50100 "My ""Perms"""#));
        assert!(output.contains(r#"codeunit "Say ""Hello"""#));
    }

    #[test]
    fn collect_permissions_empty_workspace() {
        let workspace = Workspace::new();
        let entries = collect_permissions(&workspace).unwrap().entries;
        assert!(entries.is_empty());
    }

    /// One scratch or work-in-progress file must not take permission
    /// generation down for the whole project.
    #[test]
    fn collect_permissions_skips_an_unparseable_file_and_reports_it() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/bad.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/good.al"),
            r#"codeunit 50101 "Good Codeunit" { procedure Run() begin end; }"#.to_string(),
        );

        let collection = collect_permissions(&workspace).unwrap();
        assert_eq!(
            collection
                .entries
                .iter()
                .map(|entry| entry.object_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Good Codeunit"],
            "the parseable object must still get a permission entry"
        );
        assert_eq!(
            collection
                .skipped
                .iter()
                .map(|skip| skip.path.clone())
                .collect::<Vec<_>>(),
            vec![std::path::PathBuf::from("/project/bad.al")]
        );
        assert!(
            collection.skipped[0].reason.contains("syntax error"),
            "the report must say why: {}",
            collection.skipped[0].reason
        );
    }

    #[test]
    fn collect_permissions_handles_object_with_no_id() {
        let workspace = Workspace::new();
        // Interface objects have no numeric ID
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/IMyInterface.al"),
            r#"interface "IMyInterface" { procedure Run(); }"#.to_string(),
        );
        let entries = collect_permissions(&workspace).unwrap().entries;
        assert!(entries.is_empty());
    }
}
