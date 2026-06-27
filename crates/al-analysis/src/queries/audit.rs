//! Audit queries: DataClassification and Permission Set coverage.
//!
//! T1702: DataClassification audit — find table fields with missing/incorrect classification.
//! T1706: Permission Set audit — compare defined permission sets against actual object usage.
//!
//! B13: usage-vs-grant comparison — flag granted permissions that exceed what the
//! workspace actually uses. Detection is **object-level** only: a grant is reported
//! when its target object is never referenced by any workspace object. Per-right
//! (RIMDX) over-grant analysis (e.g. a table granted Modify/Insert/Delete but only
//! ever read) is **not** performed — see `OverBroadGrantEntry`.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use al_workspace::Workspace;

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

        let Some(obj_info) = al_syntax::find_object_declaration(&parsed_tree, &file_text) else {
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
    if let Some(rest) = crate::queries::strip_field_prefix(line) {
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

/// A granted permission that exceeds what the workspace actually uses.
///
/// **Precision: object-level only.** An entry is produced when the granted
/// object is never referenced by any workspace object (i.e. an entirely unused
/// grant). The `rights` field reports the RIMDX letters as written in the
/// permission set, but they are **not** verified against actual access patterns:
/// a table granted `RIMD` that is only ever read (so `IMD` is over-broad) is
/// *not* flagged as long as the table is referenced somewhere. Right-level
/// (RIMDX) over-grant detection needs per-table record-access analysis that the
/// workspace does not yet expose (B13).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverBroadGrantEntry {
    /// Name of the permission set granting this permission.
    pub permission_set: String,
    /// Object type as written in the grant (`TableData`, `Table`, `Page`, `Codeunit`, `Report`).
    pub object_type: String,
    /// Granted object name.
    pub object: String,
    /// RIMDX rights as written in the grant (may be empty if none were specified).
    pub rights: String,
    /// Human-readable explanation of why the grant was flagged.
    pub reason: String,
}

/// Full result of the permission-set audit: per-object coverage plus over-broad
/// (unused) grants. B13 added the `over_broad` section; `coverage` is unchanged.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAuditReport {
    /// Which access objects are / are not covered by a permission set.
    pub coverage: Vec<PermissionCoverageEntry>,
    /// Grants whose object is never used by the workspace (object-level check).
    pub over_broad: Vec<OverBroadGrantEntry>,
}

/// A single permission clause parsed from a permission set body.
#[derive(Debug, Clone)]
struct PermissionGrant {
    /// Canonical object type keyword (`TableData`, `Table`, `Page`, ...).
    object_type: String,
    /// Object name (quotes stripped).
    object: String,
    /// RIMDX rights as written (uppercased; may be empty).
    rights: String,
}

pub fn permission_set_audit(workspace: &Workspace) -> PermissionAuditReport {
    let mut perm_sets: Vec<(String, Vec<PermissionGrant>)> = Vec::new();
    // Files that *define* a permission set — excluded from the usage scan so a
    // grant clause is never counted as "usage" of the object it grants.
    let mut perm_set_paths: HashSet<PathBuf> = HashSet::new();
    // Names of objects declared in the workspace (non-permissionset). Used as a
    // reference baseline: an object's own declaration counts as one reference.
    let mut declared_names: HashSet<String> = HashSet::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };
        let Some(obj_info) = al_syntax::find_object_declaration(&parsed_tree, &text) else {
            continue;
        };

        if obj_info.kind.to_lowercase() == "permissionset" {
            perm_sets.push((obj_info.name.clone(), extract_permission_grants(&text)));
            perm_set_paths.insert(path.clone());
        } else {
            declared_names.insert(obj_info.name.to_lowercase());
        }
    }

    let coverage = compute_coverage(workspace, &perm_sets);
    let over_broad = compute_over_broad(workspace, &perm_sets, &perm_set_paths, &declared_names);

    PermissionAuditReport {
        coverage,
        over_broad,
    }
}

fn compute_coverage(
    workspace: &Workspace,
    perm_sets: &[(String, Vec<PermissionGrant>)],
) -> Vec<PermissionCoverageEntry> {
    let mut results = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };
        let Some(obj_info) = al_syntax::find_object_declaration(&parsed_tree, &text) else {
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
            .filter(|(_, grants)| grants.iter().any(|g| g.object.to_lowercase() == name_lower))
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

/// Object-level over-broad / unused grant detection.
///
/// For each grant, count identifier references to the granted object across all
/// workspace files *except* permission-set definitions. The object's own
/// declaration (if it lives in the workspace) contributes exactly one reference,
/// so the baseline for "used elsewhere" is 1 for declared objects and 0 for
/// base-app objects the workspace merely references. A grant with no references
/// above that baseline is flagged as unused.
fn compute_over_broad(
    workspace: &Workspace,
    perm_sets: &[(String, Vec<PermissionGrant>)],
    perm_set_paths: &HashSet<PathBuf>,
    declared_names: &HashSet<String>,
) -> Vec<OverBroadGrantEntry> {
    // Snapshot non-permissionset parsed files once; the reference scan reuses
    // them for every grant rather than re-reading the index per grant.
    let scan_files: Vec<(String, tree_sitter::Tree)> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .filter(|path| !perm_set_paths.contains(path))
        .filter_map(|path| workspace.file_index.get_cached_parse(&path))
        .collect();

    let mut out = Vec::new();
    for (set_name, grants) in perm_sets {
        // Dedupe repeated grants of the same object within one set.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for grant in grants {
            let key = (grant.object_type.to_lowercase(), grant.object.to_lowercase());
            if !seen.insert(key) {
                continue;
            }

            let total_refs: usize = scan_files
                .iter()
                .map(|(text, tree)| {
                    al_syntax::find_variable_references(tree, text, &grant.object).len()
                })
                .sum();

            let baseline = usize::from(declared_names.contains(&grant.object.to_lowercase()));
            if total_refs <= baseline {
                out.push(OverBroadGrantEntry {
                    permission_set: set_name.clone(),
                    object_type: grant.object_type.clone(),
                    object: grant.object.clone(),
                    rights: grant.rights.clone(),
                    reason:
                        "granted object is not referenced by any workspace object (object-level \
                         check; RIMDX rights not verified)"
                            .to_string(),
                });
            }
        }
    }

    out
}

fn extract_permission_grants(text: &str) -> Vec<PermissionGrant> {
    // Look for patterns like: TableData "Sales Header" = RIMD
    // or: Table "Sales Header" = R; or: Codeunit "My CU" = X
    const PREFIXES: &[(&str, &str)] = &[
        ("tabledata ", "TableData"),
        ("table ", "Table"),
        ("page ", "Page"),
        ("codeunit ", "Codeunit"),
        ("report ", "Report"),
    ];

    let mut grants = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }

        let lower = trimmed.to_lowercase();
        for (prefix, canon) in PREFIXES {
            if !lower.starts_with(prefix) {
                continue;
            }
            let rest = &trimmed[prefix.len()..];
            let (name, after_name) = if let Some(stripped) = rest.strip_prefix('"') {
                match stripped.find('"') {
                    Some(i) => (stripped[..i].to_string(), &stripped[i + 1..]),
                    None => (String::new(), ""),
                }
            } else {
                let end = rest.find(['=', ' ']).unwrap_or(rest.len());
                (rest[..end].trim().to_string(), &rest[end..])
            };
            if name.is_empty() {
                break;
            }
            // Rights are whatever follows `=`, restricted to RIMDX letters.
            let rights = after_name
                .split('=')
                .nth(1)
                .map(|r| {
                    r.chars()
                        .filter(|c| "rimdxRIMDX".contains(*c))
                        .collect::<String>()
                        .to_uppercase()
                })
                .unwrap_or_default();
            grants.push(PermissionGrant {
                object_type: (*canon).to_string(),
                object: name,
                rights,
            });
            break;
        }
    }
    grants
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
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

        let report = permission_set_audit(&ws);
        let my_table = report.coverage.iter().find(|e| e.name == "My Table");
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

        let report = permission_set_audit(&ws);
        let my_table = report.coverage.iter().find(|e| e.name == "My Table");
        assert!(my_table.is_some(), "Should find My Table");
        assert!(my_table.unwrap().covered, "My Table should be covered");
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        assert!(data_classification_audit(&ws).is_empty());
        let report = permission_set_audit(&ws);
        assert!(report.coverage.is_empty());
        assert!(report.over_broad.is_empty());
    }

    /// A permission set grants an object that no workspace object ever uses →
    /// flagged as an unused / over-broad grant.
    #[test]
    fn over_broad_flags_unused_grant() {
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
        TableData "My Table" = RIMD,
        TableData "Unused Table" = RIMD;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws);

        // "My Table" is declared but never referenced elsewhere → unused.
        let my_table = report
            .over_broad
            .iter()
            .find(|e| e.object == "My Table");
        assert!(
            my_table.is_some(),
            "My Table is declared but never used → should be flagged. Got: {:?}",
            report.over_broad
        );

        // "Unused Table" is neither declared nor referenced → unused.
        let unused = report.over_broad.iter().find(|e| e.object == "Unused Table");
        assert!(unused.is_some(), "Unused Table should be flagged as unused");
        assert_eq!(unused.unwrap().rights, "RIMD");
        assert_eq!(unused.unwrap().object_type, "TableData");
        assert_eq!(unused.unwrap().permission_set, "My Perms");
    }

    /// A grant whose object is actually referenced by another workspace object
    /// is NOT flagged as over-broad.
    #[test]
    fn over_broad_ignores_used_grant() {
        let ws = workspace_with(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/Consumer.al",
                r#"codeunit 50101 "Consumer"
{
    procedure Use()
    var
        Rec: Record "My Table";
    begin
        Rec.Insert();
    end;
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

        let report = permission_set_audit(&ws);
        assert!(
            report.over_broad.iter().all(|e| e.object != "My Table"),
            "My Table is used by Consumer → must NOT be flagged. Got: {:?}",
            report.over_broad
        );
        // And coverage still reports it as covered.
        let cov = report.coverage.iter().find(|e| e.name == "My Table");
        assert!(cov.is_some_and(|e| e.covered), "My Table should be covered");
    }

    /// Grant clauses inside the permission set itself must not be counted as
    /// "usage" of the granted object.
    #[test]
    fn over_broad_does_not_count_grant_as_usage() {
        let ws = workspace_with(vec![(
            "/src/MyPermSet.al",
            r#"permissionset 50100 "My Perms"
{
    Permissions =
        Page "Some Page" = X;
}"#,
        )]);

        let report = permission_set_audit(&ws);
        let some_page = report.over_broad.iter().find(|e| e.object == "Some Page");
        assert!(
            some_page.is_some(),
            "Grant referencing only itself must still be flagged as unused. Got: {:?}",
            report.over_broad
        );
        assert_eq!(some_page.unwrap().object_type, "Page");
        assert_eq!(some_page.unwrap().rights, "X");
    }
}
