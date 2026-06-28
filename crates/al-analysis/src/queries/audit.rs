//! Audit queries: DataClassification and Permission Set coverage.
//!
//! T1702: DataClassification audit — find table fields with missing/incorrect classification.
//! T1706: Permission Set audit — compare defined permission sets against actual object usage.
//!
//! B13: usage-vs-grant comparison — flag granted permissions that exceed what the
//! workspace actually uses. Two complementary checks:
//!
//! 1. **Object-level** (`compute_over_broad` / `OverBroadGrantEntry`): a grant is
//!    reported when its target object is never referenced by any workspace object.
//! 2. **Right-level / RIMDX** (`compute_over_granted_rights` /
//!    `OverGrantedRightsEntry`): for a `tabledata` grant whose table *is*
//!    referenced (read), each Insert/Modify/Delete right that is granted but for
//!    which no corresponding write site (`Insert`/`Modify`/`ModifyAll`/`Delete`/
//!    `DeleteAll`/`Rename`) exists anywhere in the workspace is reported as
//!    over-granted. Read (`R`) is never flagged — a read cannot be disproven by
//!    static scanning. The write-site scan reuses `al_insight::calls`
//!    (record-variable subtype map + record-op call sites) rather than re-walking
//!    the AST. This is an over-approximation in the **safe** direction: a right is
//!    only flagged when *no* write site is found, so writes via `RecordRef`,
//!    dynamic dispatch, or other apps are conservatively missed (false negatives,
//!    never false "you may keep it" advice removed for a right that is used).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use al_insight::calls::{extract_call_sites, extract_procedure_var_types, CallSite, RecordOp};

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

/// A `tabledata` grant whose table is referenced (read) but whose granted
/// Insert/Modify/Delete rights exceed the write access the workspace actually
/// exercises (B13 right-level / RIMDX check).
///
/// **Precision: write-site over-approximation in the safe direction.** A right
/// (I/M/D) is reported as over-granted only when **no** matching write site is
/// found anywhere in the workspace — `Insert` for `I`; `Modify`/`ModifyAll`/
/// `Rename` for `M`; `Delete`/`DeleteAll` for `D`. Writes via `RecordRef`,
/// dynamically-dispatched code, base-app/other-extension code, or
/// repeated-named triggers the per-procedure scan does not revisit are **not**
/// detected, so the over-grant set is a lower bound (false negatives possible,
/// false positives avoided). Read (`R`) is never flagged: a read cannot be
/// disproven statically, and the table is referenced by construction.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverGrantedRightsEntry {
    /// Name of the permission set granting this permission.
    pub permission_set: String,
    /// Always `TableData` — only table-data grants carry RIMD rights.
    pub object_type: String,
    /// Granted table name.
    pub object: String,
    /// RIMDX rights as written in the grant.
    pub granted_rights: String,
    /// Subset of `IMD` that is granted but never observed as a write site
    /// (the rights that could be dropped).
    pub over_granted: String,
    /// Rights for which evidence was observed, in `RIMD` order. `R` is always
    /// present (assumed — the table is referenced and reads aren't disproven).
    pub observed_rights: String,
    /// Human-readable explanation of why the rights were flagged.
    pub reason: String,
}

/// Full result of the permission-set audit: per-object coverage plus over-broad
/// (unused) grants. B13 added the `over_broad` (object-level) and
/// `over_granted_rights` (right-level / RIMDX) sections; `coverage` is unchanged.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionAuditReport {
    /// Which access objects are / are not covered by a permission set.
    pub coverage: Vec<PermissionCoverageEntry>,
    /// Grants whose object is never used by the workspace (object-level check).
    pub over_broad: Vec<OverBroadGrantEntry>,
    /// `tabledata` grants whose Insert/Modify/Delete rights exceed observed
    /// write access, for tables that are referenced (right-level / RIMDX check).
    pub over_granted_rights: Vec<OverGrantedRightsEntry>,
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

    // Snapshot non-permissionset parsed files once; both the object-level scan
    // and the right-level write-site scan reuse it.
    let scan_files: Vec<(String, tree_sitter::Tree)> = workspace
        .file_index
        .files
        .iter()
        .map(|e| e.key().clone())
        .filter(|path| !perm_set_paths.contains(path))
        .filter_map(|path| workspace.file_index.get_cached_parse(&path))
        .collect();

    let over_broad = compute_over_broad(&scan_files, &perm_sets, &declared_names);

    let observed_writes = collect_observed_writes(&scan_files);
    let over_granted_rights =
        compute_over_granted_rights(&scan_files, &perm_sets, &declared_names, &observed_writes);

    PermissionAuditReport {
        coverage,
        over_broad,
        over_granted_rights,
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
    scan_files: &[(String, tree_sitter::Tree)],
    perm_sets: &[(String, Vec<PermissionGrant>)],
    declared_names: &HashSet<String>,
) -> Vec<OverBroadGrantEntry> {
    let mut out = Vec::new();
    for (set_name, grants) in perm_sets {
        // Dedupe repeated grants of the same object within one set.
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for grant in grants {
            let key = (grant.object_type.to_lowercase(), grant.object.to_lowercase());
            if !seen.insert(key) {
                continue;
            }

            let total_refs = count_object_refs(scan_files, &grant.object);

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

/// Total identifier references to `object` across the (non-permissionset)
/// snapshot. Shared by the object-level and right-level checks.
fn count_object_refs(scan_files: &[(String, tree_sitter::Tree)], object: &str) -> usize {
    scan_files
        .iter()
        .map(|(text, tree)| al_syntax::find_variable_references(tree, text, object).len())
        .sum()
}

/// Right-level / RIMDX over-grant detection (B13 follow-up).
///
/// For each `tabledata` grant whose table **is** referenced in the workspace
/// (so it is not already an object-level over-broad finding), compare the
/// granted Insert/Modify/Delete rights against the write sites observed for
/// that table (`observed_writes`). Any granted I/M/D right with no matching
/// write site is reported as over-granted. Read is never flagged.
///
/// Tables that are *not* referenced are skipped here — they are surfaced by
/// `compute_over_broad` instead, so the two checks never double-report a grant.
fn compute_over_granted_rights(
    scan_files: &[(String, tree_sitter::Tree)],
    perm_sets: &[(String, Vec<PermissionGrant>)],
    declared_names: &HashSet<String>,
    observed_writes: &HashMap<String, BTreeSet<char>>,
) -> Vec<OverGrantedRightsEntry> {
    let mut out = Vec::new();
    for (set_name, grants) in perm_sets {
        let mut seen: HashSet<String> = HashSet::new();
        for grant in grants {
            // Only table-data grants carry RIMD rights.
            if !grant.object_type.eq_ignore_ascii_case("tabledata") {
                continue;
            }
            let object_lower = grant.object.to_lowercase();
            if !seen.insert(object_lower.clone()) {
                continue;
            }

            // Which of I/M/D were granted (R/X are out of scope here).
            let granted_imd: BTreeSet<char> = ['I', 'M', 'D']
                .into_iter()
                .filter(|c| grant.rights.contains(*c))
                .collect();
            if granted_imd.is_empty() {
                continue;
            }

            // Skip tables that aren't referenced at all — object-level handles
            // those, and "only R observed" presumes the table is read.
            let total_refs = count_object_refs(scan_files, &grant.object);
            let baseline = usize::from(declared_names.contains(&object_lower));
            if total_refs <= baseline {
                continue;
            }

            let observed = observed_writes.get(&object_lower).cloned().unwrap_or_default();
            let over: BTreeSet<char> = granted_imd.difference(&observed).copied().collect();
            if over.is_empty() {
                continue;
            }

            // Render in canonical RIMD order.
            let over_granted: String = ['I', 'M', 'D']
                .into_iter()
                .filter(|c| over.contains(c))
                .collect();
            let observed_rights: String = ['R', 'I', 'M', 'D']
                .into_iter()
                .filter(|c| *c == 'R' || observed.contains(c))
                .collect();

            out.push(OverGrantedRightsEntry {
                permission_set: set_name.clone(),
                object_type: grant.object_type.clone(),
                object: grant.object.clone(),
                granted_rights: grant.rights.clone(),
                over_granted: over_granted.clone(),
                observed_rights: observed_rights.clone(),
                reason: format!(
                    "granted {granted} but only {observed} observed — no write site for {over} \
                     found in workspace (over-approximation: RecordRef / dynamic / cross-app \
                     writes are not detected; R is never flagged)",
                    granted = grant.rights,
                    observed = observed_rights,
                    over = over_granted,
                ),
            });
        }
    }
    out
}

/// Build a map of `lowercase_table_name -> {observed write rights}` by scanning
/// every workspace procedure/trigger for record write operations.
///
/// Reuses `al_insight::calls` for the heavy lifting: `extract_procedure_var_types`
/// resolves each `Record "T"` variable's subtype, and `extract_call_sites`
/// yields the record-op / member-call sites. We then map each write method to
/// the RIMD right it exercises:
///
/// | Method                       | Right |
/// |------------------------------|-------|
/// | `Insert`                     | `I`   |
/// | `Modify`, `ModifyAll`, `Rename` | `M`   |
/// | `Delete`, `DeleteAll`        | `D`   |
///
/// `Insert`/`Modify`/`Delete` arrive as `CallSite::RecordOp`; the `*All` /
/// `Rename` variants (not in `al_insight`'s `RecordOp`) arrive as
/// `CallSite::MemberCall` and are classified here. `Validate` and read ops
/// (`Get`/`Find*`) are not persistence writes and are ignored.
fn collect_observed_writes(
    scan_files: &[(String, tree_sitter::Tree)],
) -> HashMap<String, BTreeSet<char>> {
    let mut writes: HashMap<String, BTreeSet<char>> = HashMap::new();

    for (text, tree) in scan_files {
        for proc_name in collect_procedure_names(tree, text) {
            let var_types = extract_procedure_var_types(tree, text, &proc_name);
            if var_types.is_empty() {
                continue;
            }
            for site in extract_call_sites(tree, text, &proc_name) {
                let (variable, right) = match &site {
                    CallSite::RecordOp { variable, op, .. } => match op {
                        RecordOp::Insert => (variable, 'I'),
                        RecordOp::Modify => (variable, 'M'),
                        RecordOp::Delete => (variable, 'D'),
                        // Validate sets a field + runs OnValidate; it is not a
                        // persistence write on its own.
                        RecordOp::Validate => continue,
                    },
                    CallSite::MemberCall { object, method } => {
                        match write_right_for_method(method) {
                            Some(r) => (object, r),
                            None => continue,
                        }
                    }
                    _ => continue,
                };

                if let Some(table) = var_types.get(&variable.to_lowercase()) {
                    writes
                        .entry(table.to_lowercase())
                        .or_default()
                        .insert(right);
                }
            }
        }
    }

    writes
}

/// Map a record member-call method name to the RIMD write right it exercises.
///
/// Covers the `*All` / `Rename` variants that `al_insight::calls::RecordOp`
/// does not model (those surface as plain member calls). Plain `Insert`/
/// `Modify`/`Delete` are handled via `RecordOp` and are not matched here.
fn write_right_for_method(method: &str) -> Option<char> {
    match method.to_ascii_lowercase().as_str() {
        "modifyall" | "rename" => Some('M'),
        "deleteall" => Some('D'),
        _ => None,
    }
}

/// Collect the names of all procedure/trigger declarations in a parse tree.
///
/// Names feed the per-procedure `al_insight::calls` extractors. Duplicate names
/// (e.g. repeated `OnValidate` / `OnAction` triggers) are de-duplicated; the
/// name-keyed extractors only revisit the first occurrence, which is the
/// documented precision limit of the right-level check.
fn collect_procedure_names(tree: &tree_sitter::Tree, text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(t) = name_node.utf8_text(bytes) {
                        let clean = t.trim_matches('"').trim().to_string();
                        if !clean.is_empty() && seen.insert(clean.to_lowercase()) {
                            names.push(clean);
                        }
                    }
                }
                // Do not descend into the body — no nested procedures in AL.
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
    }
    names
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
        assert!(report.over_granted_rights.is_empty());
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

    // ---- B13 follow-up: right-level (RIMDX) over-grant ----------------------

    /// A table granted `RIMD` that the workspace only *reads* (via `Get`) must
    /// have its Insert/Modify/Delete rights flagged as over-granted — and `R`
    /// must never appear in the over-granted set.
    #[test]
    fn over_granted_rights_flags_read_only_table() {
        let ws = workspace_with(vec![
            (
                "/src/SalesDoc.al",
                r#"table 50100 "Sales Doc"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/Reader.al",
                r#"codeunit 50101 "Doc Reader"
{
    procedure ReadIt()
    var
        Rec: Record "Sales Doc";
    begin
        if Rec.Get('X') then
            Message(Rec."No.");
    end;
}"#,
            ),
            (
                "/src/Perms.al",
                r#"permissionset 50100 "Doc Perms"
{
    Permissions =
        TableData "Sales Doc" = RIMD;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws);

        // Read-only table is referenced, so NOT object-level over-broad.
        assert!(
            report.over_broad.iter().all(|e| e.object != "Sales Doc"),
            "Sales Doc is referenced → must not be object-level over-broad. Got: {:?}",
            report.over_broad
        );

        let entry = report
            .over_granted_rights
            .iter()
            .find(|e| e.object == "Sales Doc")
            .expect("read-only table granted RIMD should be flagged at right level");

        assert_eq!(entry.over_granted, "IMD", "all of I/M/D are unused");
        assert_eq!(entry.granted_rights, "RIMD");
        assert_eq!(entry.observed_rights, "R", "only R is observed/assumed");
        assert_eq!(entry.object_type, "TableData");
        assert_eq!(entry.permission_set, "Doc Perms");
        assert!(
            !entry.over_granted.contains('R'),
            "R must never be flagged as over-granted"
        );
    }

    /// A table the workspace actually inserts into and modifies must NOT have
    /// the granted `I`/`M` rights flagged.
    #[test]
    fn over_granted_rights_ignores_written_table() {
        let ws = workspace_with(vec![
            (
                "/src/Buffer.al",
                r#"table 50100 "Item Buffer"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/Writer.al",
                r#"codeunit 50101 "Buf Writer"
{
    procedure WriteIt()
    var
        Buf: Record "Item Buffer";
    begin
        Buf.Insert();
        Buf.Modify();
    end;
}"#,
            ),
            (
                "/src/Perms.al",
                r#"permissionset 50100 "Buf Perms"
{
    Permissions =
        TableData "Item Buffer" = RIM;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws);
        assert!(
            report
                .over_granted_rights
                .iter()
                .all(|e| e.object != "Item Buffer"),
            "Item Buffer is inserted+modified → I/M must not be flagged. Got: {:?}",
            report.over_granted_rights
        );
    }

    /// `ModifyAll` / `DeleteAll` count as M / D writes (they are member calls,
    /// not `al_insight` `RecordOp`s). A table granted `RIMD` that only uses
    /// `ModifyAll`/`DeleteAll` should flag exactly `I` as over-granted.
    #[test]
    fn over_granted_rights_recognizes_modifyall_deleteall() {
        let ws = workspace_with(vec![
            (
                "/src/Ledger.al",
                r#"table 50100 "Ledger Buf"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/Cleaner.al",
                r#"codeunit 50101 "Ledger Writer"
{
    procedure Clean()
    var
        L: Record "Ledger Buf";
    begin
        L.ModifyAll("No.", '');
        L.DeleteAll();
    end;
}"#,
            ),
            (
                "/src/Perms.al",
                r#"permissionset 50100 "Ledger Perms"
{
    Permissions =
        TableData "Ledger Buf" = RIMD;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws);
        let entry = report
            .over_granted_rights
            .iter()
            .find(|e| e.object == "Ledger Buf")
            .expect("Ledger Buf should be flagged (Insert never used)");

        assert_eq!(entry.over_granted, "I", "only Insert is unused");
        assert_eq!(
            entry.observed_rights, "RMD",
            "R assumed; ModifyAll→M, DeleteAll→D observed"
        );
    }

    /// An entirely unused table (object-level over-broad) must NOT also appear
    /// in the right-level section — the two checks never double-report.
    #[test]
    fn over_granted_rights_skips_unreferenced_table() {
        let ws = workspace_with(vec![(
            "/src/Perms.al",
            r#"permissionset 50100 "Lonely Perms"
{
    Permissions =
        TableData "Ghost Table" = RIMD;
}"#,
        )]);

        let report = permission_set_audit(&ws);
        assert!(
            report.over_broad.iter().any(|e| e.object == "Ghost Table"),
            "unreferenced table is object-level over-broad"
        );
        assert!(
            report
                .over_granted_rights
                .iter()
                .all(|e| e.object != "Ghost Table"),
            "unreferenced table must not also be a right-level finding. Got: {:?}",
            report.over_granted_rights
        );
    }
}
