//! Audit queries: DataClassification and Permission Set coverage.
//!
//! Usage-vs-grant comparison flags permissions that exceed what the
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
//!    dynamic dispatch, or other apps are conservatively missed. False negatives
//!    are possible; observed writes are never reported as removable rights.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use al_insight::calls::{extract_call_sites, extract_procedure_var_types, CallSite, RecordOp};

use serde::Serialize;

use crate::workspace_sources::{self, WorkspaceSource};
use al_workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit refused an incomplete workspace snapshot: {reason}")]
    IncompleteWorkspace { reason: String },
    #[error("audit could not inspect '{}': {reason}", path.display())]
    InvalidSource { path: PathBuf, reason: String },
}

fn workspace_snapshot(workspace: &Workspace) -> Result<Vec<WorkspaceSource>, AuditError> {
    workspace_sources::snapshot(workspace).map_err(|error| AuditError::IncompleteWorkspace {
        reason: error.to_string(),
    })
}

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
    pub file: String,
    /// Line number (1-based).
    pub line: u32,
}

pub fn data_classification_audit(
    workspace: &Workspace,
) -> Result<Vec<DataClassificationEntry>, AuditError> {
    let sources = workspace_snapshot(workspace)?;
    let mut results = Vec::new();

    for source in sources {
        if !matches!(
            source.object.kind,
            al_symbols::ObjectKind::Table | al_symbols::ObjectKind::TableExtension
        ) {
            continue;
        }

        scan_table_fields(&source, &mut results)?;
    }

    Ok(results)
}

fn scan_table_fields(
    source: &WorkspaceSource,
    results: &mut Vec<DataClassificationEntry>,
) -> Result<(), AuditError> {
    let sections = super::bulk_fix::collect_ast_sections(&source.tree, &source.text, &["field"])
        .map_err(|reason| AuditError::InvalidSource {
            path: source.path.clone(),
            reason,
        })?;
    for section in sections {
        let field = section
            .header_segments
            .get(1)
            .and_then(|value| super::bulk_fix::parse_identifier(value))
            .ok_or_else(|| AuditError::InvalidSource {
                path: source.path.clone(),
                reason: format!(
                    "table field on line {} has no unambiguous AST field name",
                    section.line
                ),
            })?;
        if section.properties.get("fieldclass").is_some_and(|value| {
            matches!(
                value
                    .trim()
                    .trim_matches(['\'', '"'])
                    .to_ascii_lowercase()
                    .as_str(),
                "flowfield" | "flowfilter"
            )
        }) {
            continue;
        }
        let classification = match section.properties.get("dataclassification") {
            Some(value) if !value.trim().is_empty() => {
                value.trim().trim_matches(['\'', '"']).trim().to_string()
            }
            Some(_) => {
                return Err(AuditError::InvalidSource {
                    path: source.path.clone(),
                    reason: format!("DataClassification on field '{field}' has no value"),
                });
            }
            None => "(none)".to_string(),
        };
        let risk = classify_gdpr_risk(&classification);
        results.push(DataClassificationEntry {
            table: source.object.info.name.clone(),
            field,
            classification,
            risk,
            file: source.path.display().to_string(),
            line: section.line,
        });
    }
    Ok(())
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
    pub id: i32,
    pub name: String,
    pub covered: bool,
    pub covered_by: Vec<String>,
}

/// A granted permission that exceeds what the workspace actually uses.
///
/// **Precision: object-level entry.** An entry is produced when the granted
/// object is never referenced by any workspace object (i.e. an entirely unused
/// grant). Right-level table-data results are represented separately by
/// [`OverGrantedRightsEntry`], using the workspace record-access scan below; this
/// object-level shape remains useful for non-table grants and entirely unused
/// objects.
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
/// exercises (right-level / RIMDX check).
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
/// (unused) grants. added the `over_broad` (object-level) and
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

pub fn permission_set_audit(workspace: &Workspace) -> Result<PermissionAuditReport, AuditError> {
    let sources = workspace_snapshot(workspace)?;
    let mut perm_sets: Vec<(String, Vec<PermissionGrant>)> = Vec::new();
    // Files that *define* a permission set — excluded from the usage scan so a
    // grant clause is never counted as "usage" of the object it grants.
    let mut perm_set_paths: HashSet<PathBuf> = HashSet::new();
    // Names of objects declared in the workspace (non-permissionset). Used as a
    // reference baseline: an object's own declaration counts as one reference.
    let mut declared_names: HashSet<String> = HashSet::new();

    for source in &sources {
        if matches!(
            source.object.kind,
            al_symbols::ObjectKind::PermissionSet | al_symbols::ObjectKind::PermissionSetExtension
        ) {
            perm_sets.push((
                source.object.info.name.clone(),
                extract_permission_grants(source)?,
            ));
            perm_set_paths.insert(source.path.clone());
        } else {
            declared_names.insert(source.object.info.name.to_lowercase());
        }
    }

    let coverage = compute_coverage(&sources, &perm_sets);

    // Snapshot non-permissionset parsed files once; both the object-level scan
    // and the right-level write-site scan reuse it.
    let scan_files: Vec<(String, tree_sitter::Tree)> = sources
        .iter()
        .filter(|source| !perm_set_paths.contains(&source.path))
        .map(|source| (source.text.clone(), source.tree.clone()))
        .collect();

    let over_broad = compute_over_broad(&scan_files, &perm_sets, &declared_names);

    let observed_writes = collect_observed_writes(&scan_files);
    let over_granted_rights =
        compute_over_granted_rights(&scan_files, &perm_sets, &declared_names, &observed_writes);

    Ok(PermissionAuditReport {
        coverage,
        over_broad,
        over_granted_rights,
    })
}

fn compute_coverage(
    sources: &[WorkspaceSource],
    perm_sets: &[(String, Vec<PermissionGrant>)],
) -> Vec<PermissionCoverageEntry> {
    let mut results = Vec::new();

    for source in sources {
        let kind = source.object.info.kind.to_lowercase();
        // Only audit tables, pages, codeunits, reports (primary access objects)
        if !matches!(kind.as_str(), "table" | "page" | "codeunit" | "report") {
            continue;
        }

        let covered_by: Vec<String> = perm_sets
            .iter()
            .filter(|(_, grants)| {
                grants.iter().any(|grant| {
                    grant_covers_object(
                        grant,
                        &kind,
                        source.object.normalized_id,
                        &source.object.info.name,
                    )
                })
            })
            .map(|(n, _)| n.clone())
            .collect();

        results.push(PermissionCoverageEntry {
            kind: source.object.info.kind.clone(),
            id: source.object.normalized_id,
            name: source.object.info.name.clone(),
            covered: !covered_by.is_empty(),
            covered_by,
        });
    }

    results
}

fn grant_covers_object(
    grant: &PermissionGrant,
    object_kind: &str,
    object_id: i32,
    object_name: &str,
) -> bool {
    let expected_grant_type = match object_kind {
        "table" => "TableData",
        "page" => "Page",
        "codeunit" => "Codeunit",
        "report" => "Report",
        _ => return false,
    };
    grant.object_type.eq_ignore_ascii_case(expected_grant_type)
        && (grant.object == "*"
            || grant.object.eq_ignore_ascii_case(object_name)
            || grant.object.parse::<i32>() == Ok(object_id))
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
            let key = (
                grant.object_type.to_lowercase(),
                grant.object.to_lowercase(),
            );
            if !seen.insert(key) {
                continue;
            }

            if grant.object == "*" {
                out.push(OverBroadGrantEntry {
                    permission_set: set_name.clone(),
                    object_type: grant.object_type.clone(),
                    object: grant.object.clone(),
                    rights: grant.rights.clone(),
                    reason: "wildcard grant applies to every object of this type; Microsoft \
                             documents that wildcard permissions require caution and the static \
                             audit cannot prove that complete scope is required"
                        .to_string(),
                });
                continue;
            }
            if grant
                .object
                .chars()
                .all(|character| character.is_ascii_digit())
            {
                out.push(OverBroadGrantEntry {
                    permission_set: set_name.clone(),
                    object_type: grant.object_type.clone(),
                    object: grant.object.clone(),
                    rights: grant.rights.clone(),
                    reason: "numeric grant target cannot be matched to name-based static \
                             references without authoritative package identity resolution; manual \
                             review is required"
                        .to_string(),
                });
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

/// Right-level / RIMDX over-grant detection.
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
            if grant.object == "*"
                || grant
                    .object
                    .chars()
                    .all(|character| character.is_ascii_digit())
            {
                continue;
            }
            let object_lower = grant.object.to_lowercase();
            if !seen.insert(object_lower.clone()) {
                continue;
            }

            // Which of I/M/D were granted (R/X are out of scope here).
            let granted_imd: BTreeSet<char> = ['I', 'M', 'D']
                .into_iter()
                .filter(|right| {
                    grant
                        .rights
                        .chars()
                        .any(|granted| granted.eq_ignore_ascii_case(right))
                })
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

            let observed = observed_writes
                .get(&object_lower)
                .cloned()
                .unwrap_or_default();
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

fn extract_permission_grants(source: &WorkspaceSource) -> Result<Vec<PermissionGrant>, AuditError> {
    let mut permission_properties = Vec::new();
    let mut stack = vec![source.tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "property_assignment" {
            let name = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.text.as_bytes()).ok())
                .map(str::trim);
            if name.is_some_and(|name| name.eq_ignore_ascii_case("Permissions")) {
                permission_properties.push(node);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    let property = match permission_properties.as_slice() {
        [] => return Ok(Vec::new()),
        [property] => *property,
        _ => {
            return Err(AuditError::InvalidSource {
                path: source.path.clone(),
                reason: "permission-set object contains multiple Permissions properties"
                    .to_string(),
            });
        }
    };

    let mut rhs_started = false;
    let mut clauses: Vec<Vec<String>> = vec![Vec::new()];
    for index in 0..property.child_count() {
        let child = property
            .child(index)
            .ok_or_else(|| AuditError::InvalidSource {
                path: source.path.clone(),
                reason: "Permissions AST child disappeared".to_string(),
            })?;
        let token = child
            .utf8_text(source.text.as_bytes())
            .map_err(|error| AuditError::InvalidSource {
                path: source.path.clone(),
                reason: format!("Permissions token is not UTF-8: {error}"),
            })?
            .trim();
        if !rhs_started {
            if child.kind() == "=" {
                rhs_started = true;
            }
            continue;
        }
        if matches!(child.kind(), "comment" | "line_comment" | "block_comment") {
            continue;
        }
        if child.kind() == "semicolon" || token == ";" {
            break;
        }
        if child.kind() == "comma" || token == "," {
            if clauses.last().is_some_and(Vec::is_empty) {
                return Err(AuditError::InvalidSource {
                    path: source.path.clone(),
                    reason: "Permissions property contains an empty grant clause".to_string(),
                });
            }
            clauses.push(Vec::new());
            continue;
        }
        let clause = clauses
            .last_mut()
            .ok_or_else(|| AuditError::InvalidSource {
                path: source.path.clone(),
                reason: "Permissions clause accumulator became empty".to_string(),
            })?;
        clause.push(token.to_string());
    }
    if !rhs_started {
        return Err(AuditError::InvalidSource {
            path: source.path.clone(),
            reason: "Permissions property has no assignment operator".to_string(),
        });
    }
    if clauses.len() == 1 && clauses[0].is_empty() {
        return Ok(Vec::new());
    }
    if clauses.last().is_some_and(Vec::is_empty) {
        return Err(AuditError::InvalidSource {
            path: source.path.clone(),
            reason: "Permissions property ends with an empty grant clause".to_string(),
        });
    }

    clauses
        .into_iter()
        .enumerate()
        .map(|(index, clause)| {
            parse_permission_clause(&clause).map_err(|reason| AuditError::InvalidSource {
                path: source.path.clone(),
                reason: format!("Permissions clause {} is invalid: {reason}", index + 1),
            })
        })
        .collect()
}

fn parse_permission_clause(tokens: &[String]) -> Result<PermissionGrant, String> {
    if tokens.len() != 4 || tokens[2] != "=" {
        return Err(format!(
            "expected ObjectType ObjectIdentifier = Rights, got '{}'",
            tokens.join(" ")
        ));
    }
    let object_type = match tokens[0].to_ascii_lowercase().as_str() {
        "tabledata" => "TableData",
        "table" => "Table",
        "report" => "Report",
        "codeunit" => "Codeunit",
        "xmlport" => "XmlPort",
        "page" => "Page",
        "query" => "Query",
        other => return Err(format!("unsupported permission object type '{other}'")),
    };
    let object = if tokens[1] == "*" {
        "*".to_string()
    } else {
        super::bulk_fix::parse_identifier(&tokens[1])
            .or_else(|| {
                tokens[1]
                    .chars()
                    .all(|character| character.is_ascii_digit())
                    .then(|| tokens[1].clone())
            })
            .ok_or_else(|| format!("invalid object identifier '{}'", tokens[1]))?
    };
    let rights = tokens[3].trim().to_string();
    if rights.is_empty()
        || !rights.chars().all(|right| match object_type {
            "TableData" => "RrIiMmDd".contains(right),
            _ => matches!(right, 'X' | 'x'),
        })
    {
        return Err(format!(
            "rights '{rights}' are invalid for {object_type}; tabledata accepts R/r/I/i/M/m/D/d and executable objects accept X/x"
        ));
    }
    let mut seen = HashSet::new();
    if !rights
        .chars()
        .map(|right| right.to_ascii_uppercase())
        .all(|right| seen.insert(right))
    {
        return Err(format!("rights '{rights}' contain a duplicate permission"));
    }
    Ok(PermissionGrant {
        object_type: object_type.to_string(),
        object,
        rights,
    })
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

        let entries = data_classification_audit(&ws).unwrap();
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

        let entries = data_classification_audit(&ws).unwrap();
        assert!(entries.is_empty(), "Should not audit codeunit fields");
    }

    #[test]
    fn data_classification_skips_flow_fields_and_flow_filters() {
        let ws = workspace_with(vec![(
            "/src/Calculated.al",
            r#"table 50100 Calculated
{
    fields
    {
        field(1; Balance; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Sum("Ledger Entry".Amount);
        }
        field(2; Filter; Text[20])
        {
            FieldClass = FlowFilter;
        }
        field(3; Stored; Text[20])
        {
        }
    }
}"#,
        )]);

        let entries = data_classification_audit(&ws).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].field, "Stored");
    }

    #[test]
    fn data_classification_ignores_mentions_in_comments_and_strings() {
        let ws = workspace_with(vec![(
            "/src/Literal.al",
            r#"table 50100 Literal
{
    fields
    {
        field(1; Stored; Text[100])
        {
            Caption = 'DataClassification = SystemMetadata; { literal }';
            // DataClassification = CustomerContent;
        }
    }
}"#,
        )]);

        let entries = data_classification_audit(&ws).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].classification, "(none)");
        assert_eq!(entries[0].risk, GdprRisk::Unclassified);
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

        let report = permission_set_audit(&ws).unwrap();
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

        let report = permission_set_audit(&ws).unwrap();
        let my_table = report.coverage.iter().find(|e| e.name == "My Table");
        assert!(my_table.is_some(), "Should find My Table");
        assert!(my_table.unwrap().covered, "My Table should be covered");
    }

    #[test]
    fn permission_audit_parses_multiline_comments_and_indirect_rights() {
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
        // The lower-case letters are indirect permissions.
        TableData
            "My Table"
            =
            rimd,
        /* executable object */
        XmlPort "My XmlPort" = x,
        Query "My Query" = X;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws).unwrap();
        let table = report
            .coverage
            .iter()
            .find(|entry| entry.name == "My Table")
            .unwrap();
        assert!(table.covered);
        let table_finding = report
            .over_broad
            .iter()
            .find(|entry| entry.object == "My Table")
            .unwrap();
        assert_eq!(table_finding.rights, "rimd");
        assert!(report
            .over_broad
            .iter()
            .any(|entry| { entry.object_type == "XmlPort" && entry.object == "My XmlPort" }));
        assert!(report
            .over_broad
            .iter()
            .any(|entry| entry.object_type == "Query" && entry.object == "My Query"));
    }

    #[test]
    fn wildcard_and_numeric_grants_cover_matching_workspace_objects() {
        let ws = workspace_with(vec![
            (
                "/src/First.al",
                r#"table 50100 First
{
    fields { field(1; Value; Text[20]) { } }
}"#,
            ),
            (
                "/src/Second.al",
                r#"table 50101 Second
{
    fields { field(1; Value; Text[20]) { } }
}"#,
            ),
            (
                "/src/Perms.al",
                r#"permissionset 50102 Perms
{
    Permissions =
        TableData * = R,
        TableData 50101 = r;
}"#,
            ),
        ]);

        let report = permission_set_audit(&ws).unwrap();
        assert!(
            report.coverage.iter().all(|entry| entry.covered),
            "{:?}",
            report.coverage
        );
        assert!(report
            .over_broad
            .iter()
            .any(|entry| entry.object == "*" && entry.reason.contains("wildcard")));
        assert!(report.over_broad.iter().any(|entry| {
            entry.object == "50101" && entry.reason.contains("numeric grant target")
        }));
    }

    #[test]
    fn malformed_permission_clause_is_an_explicit_audit_error() {
        let ws = workspace_with(vec![(
            "/src/Perms.al",
            r#"permissionset 50100 Perms
{
    Permissions = TableData Customer = RX;
}"#,
        )]);

        let error = permission_set_audit(&ws).unwrap_err();
        assert!(matches!(error, AuditError::InvalidSource { .. }));
        assert!(error.to_string().contains("rights 'RX' are invalid"));
    }

    #[test]
    fn duplicate_permissions_properties_are_rejected() {
        let ws = workspace_with(vec![(
            "/src/Perms.al",
            r#"permissionset 50100 Perms
{
    Permissions = TableData Customer = R;
    Permissions = TableData Vendor = R;
}"#,
        )]);

        let error = permission_set_audit(&ws).unwrap_err();
        assert!(matches!(error, AuditError::InvalidSource { .. }));
        assert!(error
            .to_string()
            .contains("multiple Permissions properties"));
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        assert!(data_classification_audit(&ws).unwrap().is_empty());
        let report = permission_set_audit(&ws).unwrap();
        assert!(report.coverage.is_empty());
        assert!(report.over_broad.is_empty());
        assert!(report.over_granted_rights.is_empty());
    }

    #[test]
    fn whole_workspace_audits_reject_malformed_source() {
        let ws = workspace_with(vec![(
            "/src/Broken.al",
            "codeunit 50100 Broken { procedure Incomplete(",
        )]);

        assert!(matches!(
            data_classification_audit(&ws),
            Err(AuditError::IncompleteWorkspace { .. })
        ));
        assert!(matches!(
            permission_set_audit(&ws),
            Err(AuditError::IncompleteWorkspace { .. })
        ));
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

        let report = permission_set_audit(&ws).unwrap();

        // "My Table" is declared but never referenced elsewhere → unused.
        let my_table = report.over_broad.iter().find(|e| e.object == "My Table");
        assert!(
            my_table.is_some(),
            "My Table is declared but never used → should be flagged. Got: {:?}",
            report.over_broad
        );

        // "Unused Table" is neither declared nor referenced → unused.
        let unused = report
            .over_broad
            .iter()
            .find(|e| e.object == "Unused Table");
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

        let report = permission_set_audit(&ws).unwrap();
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

        let report = permission_set_audit(&ws).unwrap();
        let some_page = report.over_broad.iter().find(|e| e.object == "Some Page");
        assert!(
            some_page.is_some(),
            "Grant referencing only itself must still be flagged as unused. Got: {:?}",
            report.over_broad
        );
        assert_eq!(some_page.unwrap().object_type, "Page");
        assert_eq!(some_page.unwrap().rights, "X");
    }

    // ---- right-level (RIMDX) over-grant ----------------------

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

        let report = permission_set_audit(&ws).unwrap();

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

        let report = permission_set_audit(&ws).unwrap();
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

        let report = permission_set_audit(&ws).unwrap();
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

        let report = permission_set_audit(&ws).unwrap();
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
