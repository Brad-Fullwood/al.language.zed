//! Audit queries: DataClassification and Permission Set coverage.
//!
//! T1702: DataClassification audit — find table fields with missing/incorrect classification.
//! T1706: Permission Set audit — compare defined permission sets against actual object usage.

use serde::Serialize;

use crate::workspace::Workspace;

/// The GDPR risk level of a DataClassification value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GdprRisk {
    /// No classification set — unknown risk.
    Unclassified,
    /// Contains personal data (EndUserId, ToBeClassified, CustomerContent, EUII, etc.).
    Personal,
    /// Non-personal organizational data.
    OrganizationIdentifiableInformation,
    SystemMetadata,
    /// Explicitly set to no personal data.
    None,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataClassificationEntry {
    pub table: String,
    pub field: String,
    /// DataClassification value found (or "(none)" if missing).
    pub classification: String,
    pub risk: GdprRisk,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number (1-based).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[must_use]
pub fn data_classification_audit(workspace: &Workspace) -> Vec<DataClassificationEntry> {
    let mut results = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let file_path = path.to_string_lossy().to_string();
        let Some((file_text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };

        let Some(obj_info) = crate::syntax::find_object_declaration(&parsed_tree, &file_text)
        else {
            continue;
        };

        if !matches!(
            obj_info.kind.to_lowercase().as_str(),
            "table" | "tableextension"
        ) {
            continue;
        }

        scan_table_fields(&file_path, &file_text, &obj_info.name, &mut results);
    }

    results
}

fn scan_table_fields(
    file_path: &str,
    text: &str,
    table_name: &str,
    results: &mut Vec<DataClassificationEntry>,
) {
    // Text-based scan: the AL grammar has no dedicated field_declaration node.
    // We track `field(id; Name; Type) { ... }` blocks and their DataClassification property.
    struct FieldCtx {
        name: String,
        line: u32,
        classification: Option<String>,
        brace_depth: i32,
    }

    let mut stack: Vec<FieldCtx> = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        let open = line.chars().filter(|&c| c == '{').count() as i32;
        let close = line.chars().filter(|&c| c == '}').count() as i32;

        if lower.starts_with("field(") || lower.starts_with("field (") {
            let field_name = extract_field_name_from_line(trimmed);
            stack.push(FieldCtx {
                name: field_name,
                line: line_idx as u32 + 1,
                classification: None,
                brace_depth: open - close,
            });
            continue;
        }

        if let Some(ctx) = stack.last_mut() {
            ctx.brace_depth += open - close;

            if lower.contains("dataclassification") {
                let classification =
                    extract_property_value_from_line(trimmed, "DataClassification");
                if let Some(c) = classification {
                    ctx.classification = Some(c);
                }
            }

            if ctx.brace_depth <= 0 {
                let Some(ctx) = stack.pop() else {
                    continue;
                };
                let classification = ctx
                    .classification
                    .clone()
                    .unwrap_or_else(|| "(none)".to_string());
                let risk = classify_gdpr_risk(&classification);

                results.push(DataClassificationEntry {
                    table: table_name.to_string(),
                    field: ctx.name,
                    classification,
                    risk,
                    file: Some(file_path.to_string()),
                    line: Some(ctx.line),
                });
            }
        }
    }
}

fn extract_field_name_from_line(line: &str) -> String {
    // field(id; "Name"; ...) or field(id; Name; ...)
    if let Some(rest) = line
        .strip_prefix("field(")
        .or_else(|| line.strip_prefix("field ("))
    {
        if let Some(after_semi) = rest.find(';').map(|i| rest[i + 1..].trim()) {
            if let Some(stripped) = after_semi.strip_prefix('"') {
                if let Some(end) = stripped.find('"') {
                    return stripped[..end].to_string();
                }
            }
            let end = after_semi.find([';', ')']).unwrap_or(after_semi.len());
            return after_semi[..end].trim().to_string();
        }
    }
    String::new()
}

fn extract_property_value_from_line(line: &str, prop: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let prop_lower = prop.to_lowercase();
    let pos = lower.find(&prop_lower)?;
    let after = line[pos + prop_lower.len()..].trim_start_matches([' ', '=', ':']);
    let after = after.trim_start_matches(['"', '\'']);
    let end = after
        .find(['"', '\'', ';', '\n', ' '])
        .unwrap_or(after.len().min(100));
    let val = after[..end].trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

fn classify_gdpr_risk(classification: &str) -> GdprRisk {
    let lower = classification.to_lowercase();
    match lower.as_str() {
        "(none)" | "" => GdprRisk::Unclassified,
        "enduserpseudonymousidentifiers" | "eupi" => GdprRisk::Personal,
        "enduseridentifiableinformation" | "euii" => GdprRisk::Personal,
        "customercontent" => GdprRisk::Personal,
        "tobeclassified" => GdprRisk::Unclassified,
        "organizationidentifiableinformation" => GdprRisk::OrganizationIdentifiableInformation,
        "systemmetadata" => GdprRisk::SystemMetadata,
        "none" | "accountdata" => GdprRisk::None,
        _ if lower.contains("personal") || lower.contains("customer") => GdprRisk::Personal,
        _ => GdprRisk::Unclassified,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionCoverageEntry {
    pub kind: String,
    pub id: u32,
    pub name: String,
    pub covered: bool,
    pub covered_by: Vec<String>,
}

pub fn permission_set_audit(workspace: &Workspace) -> Vec<PermissionCoverageEntry> {
    let mut results = Vec::new();

    let mut perm_sets: Vec<(String, Vec<String>)> = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };
        let Some(obj_info) = crate::syntax::find_object_declaration(&parsed_tree, &text) else {
            continue;
        };

        if obj_info.kind.to_lowercase() == "permissionset" {
            let covered = extract_permission_objects(&text);
            perm_sets.push((obj_info.name.clone(), covered));
        }
    }

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };
        let Some(obj_info) = crate::syntax::find_object_declaration(&parsed_tree, &text) else {
            continue;
        };

        let kind = obj_info.kind.to_lowercase();
        // Only audit tables, pages, codeunits, reports (primary access objects)
        if !matches!(kind.as_str(), "table" | "page" | "codeunit" | "report") {
            continue;
        }

        let name_lower = obj_info.name.to_lowercase();
        let covered_by: Vec<String> = perm_sets
            .iter()
            .filter(|(_, objects)| objects.iter().any(|o| o.to_lowercase() == name_lower))
            .map(|(n, _)| n.clone())
            .collect();

        let id = obj_info.id.unwrap_or(0) as u32;
        results.push(PermissionCoverageEntry {
            kind: obj_info.kind,
            id,
            name: obj_info.name,
            covered: !covered_by.is_empty(),
            covered_by,
        });
    }

    results
}

fn extract_permission_objects(text: &str) -> Vec<String> {
    // Look for patterns like: TableData "Sales Header" = RIMD
    // or: Table "Sales Header" = R
    let mut objects = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }

        let lower = trimmed.to_lowercase();
        for prefix in &["tabledata ", "table ", "page ", "codeunit ", "report "] {
            if lower.starts_with(prefix) {
                let rest = &trimmed[prefix.len()..];
                let name = if let Some(stripped) = rest.strip_prefix('"') {
                    stripped.find('"').map(|i| stripped[..i].to_string())
                } else {
                    rest.find(['=', ' ']).map(|i| rest[..i].trim().to_string())
                };
                if let Some(n) = name {
                    if !n.is_empty() {
                        objects.push(n);
                    }
                }
            }
        }
    }
    objects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn data_classification_finds_unclassified_field() {
        let ws = workspace_with(vec![(
            "/src/Customer.al",
            r#"table 18 Customer
{
    fields
    {
        field(1; "No."; Code[20])
        {
        }
        field(2; "Name"; Text[100])
        {
            DataClassification = CustomerContent;
        }
    }
}"#,
        )]);

        let entries = data_classification_audit(&ws);
        let no_field = entries.iter().find(|e| e.field == "No.");
        assert!(no_field.is_some(), "Should find 'No.' field");
        assert_eq!(
            no_field.unwrap().risk,
            GdprRisk::Unclassified,
            "No. should be unclassified"
        );

        let name_field = entries.iter().find(|e| e.field == "Name");
        assert!(name_field.is_some(), "Should find 'Name' field");
        assert_eq!(
            name_field.unwrap().risk,
            GdprRisk::Personal,
            "CustomerContent is personal"
        );
    }

    #[test]
    fn data_classification_skips_non_tables() {
        let ws = workspace_with(vec![(
            "/src/MyCu.al",
            r#"codeunit 50100 "My CU"
{
    procedure DoWork()
    begin
    end;
}"#,
        )]);

        let entries = data_classification_audit(&ws);
        assert!(entries.is_empty(), "Should not audit codeunit fields");
    }

    #[test]
    fn permission_audit_finds_uncovered_object() {
        let ws = workspace_with(vec![(
            "/src/MyTable.al",
            r#"table 50100 "My Table"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
        )]);

        let entries = permission_set_audit(&ws);
        let my_table = entries.iter().find(|e| e.name == "My Table");
        assert!(my_table.is_some(), "Should find My Table");
        assert!(!my_table.unwrap().covered, "My Table has no permission set");
    }

    #[test]
    fn permission_audit_covered_object() {
        let ws = workspace_with(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/MyPermSet.al",
                r#"permissionset 50100 "My Perms"
{
    Permissions =
        TableData "My Table" = RIMD;
}"#,
            ),
        ]);

        let entries = permission_set_audit(&ws);
        let my_table = entries.iter().find(|e| e.name == "My Table");
        assert!(my_table.is_some(), "Should find My Table");
        assert!(my_table.unwrap().covered, "My Table should be covered");
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        assert!(data_classification_audit(&ws).is_empty());
        assert!(permission_set_audit(&ws).is_empty());
    }
}
