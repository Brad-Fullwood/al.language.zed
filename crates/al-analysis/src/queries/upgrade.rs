//! Upgrade impact analysis with migration hints.

use serde::Serialize;

use super::breaking_changes::{
    analyze_breaking_changes, analyze_breaking_changes_checked, BreakingChange, BreakingChangeKind,
};
use al_symbols::SymbolEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpgradeIssueKind {
    BreakingChange,
    DataMigration,
    ObsoleteSymbol,
    NewPermission,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeIssue {
    pub kind: UpgradeIssueKind,
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub description: String,
    pub migration_hint: String,
    /// Severity: "error", "warning", "info".
    pub severity: String,
}

pub fn upgrade_report(baseline: &[SymbolEntry], current: &[SymbolEntry]) -> Vec<UpgradeIssue> {
    build_upgrade_report(
        analyze_breaking_changes(baseline, current),
        baseline,
        current,
    )
}

pub fn upgrade_report_checked(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
) -> Result<Vec<UpgradeIssue>, String> {
    let breaking = analyze_breaking_changes_checked(baseline, current)?;
    Ok(build_upgrade_report(breaking, baseline, current))
}

fn build_upgrade_report(
    breaking: Vec<BreakingChange>,
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
) -> Vec<UpgradeIssue> {
    let mut issues = Vec::new();
    for change in breaking {
        let issue = breaking_change_to_upgrade_issue(change);
        issues.push(issue);
    }

    let baseline_tables: std::collections::HashMap<_, _> = baseline
        .iter()
        .filter(|entry| !entry.synthetic && matches!(entry.kind, al_symbols::ObjectKind::Table))
        .map(|entry| (surface_key(entry), entry))
        .collect();

    let current_tables: std::collections::HashMap<_, _> = current
        .iter()
        .filter(|entry| !entry.synthetic && matches!(entry.kind, al_symbols::ObjectKind::Table))
        .map(|entry| (surface_key(entry), entry))
        .collect();

    for (identity, old_table) in &baseline_tables {
        if let Some(new_table) = current_tables.get(identity) {
            check_data_migration_needs(old_table, new_table, &mut issues);
        }
    }

    detect_obsolete_transitions(baseline, current, &mut issues);
    detect_new_permissions(baseline, current, &mut issues);
    issues.sort_by(|left, right| {
        left.object
            .to_ascii_lowercase()
            .cmp(&right.object.to_ascii_lowercase())
            .then_with(|| left.member.cmp(&right.member))
            .then_with(|| left.description.cmp(&right.description))
    });
    issues
}

fn breaking_change_to_upgrade_issue(change: BreakingChange) -> UpgradeIssue {
    let (migration_hint, severity) = match &change.kind {
        BreakingChangeKind::ObjectRemoved => (
            format!("Remove all references to '{}' and its dependent code.", change.object),
            "error".to_string(),
        ),
        BreakingChangeKind::ProcedureRemoved => (
            format!(
                "Replace calls to '{}' with an alternative or remove the calling code.",
                change.member.as_deref().unwrap_or("(unknown)")
            ),
            "error".to_string(),
        ),
        BreakingChangeKind::SignatureChanged => (
            format!(
                "Update all callers of '{}' to match the new signature.",
                change.member.as_deref().unwrap_or("(unknown)")
            ),
            if change.is_breaking { "error".to_string() } else { "warning".to_string() },
        ),
        BreakingChangeKind::ReturnTypeChanged => (
            "Update code that uses the return value to handle the new type.".to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::FieldRemoved => (
            format!(
                "Run upgrade codeunit to migrate data from '{}' and update all code that references it.",
                change.member.as_deref().unwrap_or("(unknown)")
            ),
            "error".to_string(),
        ),
        BreakingChangeKind::EnumValueRemoved => (
            format!(
                "Replace usage of enum value '{}' with a valid alternative.",
                change.member.as_deref().unwrap_or("(unknown)")
            ),
            "error".to_string(),
        ),
        BreakingChangeKind::ObjectIdChanged => (
            "Restore the published object ID or provide a replacement object and migrate references."
                .to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::NamespaceChanged => (
            "Preserve the published namespace or update every dependent app and publish a coordinated major upgrade."
                .to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::BaseObjectChanged
        | BreakingChangeKind::InterfaceRemoved
        | BreakingChangeKind::AccessReduced => (
            "Restore the previous public contract or update and recompile all dependent apps."
                .to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::FieldRenamed
        | BreakingChangeKind::FieldIdChanged
        | BreakingChangeKind::FieldTypeChanged => (
            "Add upgrade code that preserves existing table data and update all field references."
                .to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::EnumValueOrdinalChanged => (
            "Restore the published enum ordinal; persisted enum values use ordinals, not display names."
                .to_string(),
            "error".to_string(),
        ),
        BreakingChangeKind::PermissionReduced => (
            "Review dependent runtime flows and restore required permission flags or provide an explicit replacement permission set."
                .to_string(),
            "error".to_string(),
        ),
    };

    UpgradeIssue {
        kind: UpgradeIssueKind::BreakingChange,
        object: change.object,
        member: change.member,
        description: change.description,
        migration_hint,
        severity,
    }
}

fn surface_key(entry: &SymbolEntry) -> (String, String, String) {
    (
        entry.kind.to_string().to_ascii_lowercase(),
        entry.namespace.to_ascii_lowercase(),
        entry.name.to_ascii_lowercase(),
    )
}

fn property<'a>(properties: &'a [al_symbols::PropertyValue], name: &str) -> Option<&'a str> {
    properties
        .iter()
        .find(|property| property.name.eq_ignore_ascii_case(name))
        .map(|property| property.value.trim().trim_matches(['\'', '"']))
}

fn obsolete_rank(properties: &[al_symbols::PropertyValue]) -> u8 {
    match property(properties, "ObsoleteState")
        .unwrap_or("No")
        .to_ascii_lowercase()
        .as_str()
    {
        "pending" => 1,
        "removed" => 2,
        _ => 0,
    }
}

fn obsolete_label(properties: &[al_symbols::PropertyValue]) -> &'static str {
    match obsolete_rank(properties) {
        1 => "Pending",
        2 => "Removed",
        _ => "No",
    }
}

fn detect_obsolete_transitions(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
    issues: &mut Vec<UpgradeIssue>,
) {
    let baseline_map: std::collections::HashMap<_, _> = baseline
        .iter()
        .filter(|entry| !entry.synthetic)
        .map(|entry| (surface_key(entry), entry))
        .collect();
    for current_entry in current.iter().filter(|entry| !entry.synthetic) {
        let Some(old_entry) = baseline_map.get(&surface_key(current_entry)) else {
            continue;
        };
        if obsolete_rank(&current_entry.properties) > obsolete_rank(&old_entry.properties) {
            issues.push(UpgradeIssue {
                kind: UpgradeIssueKind::ObsoleteSymbol,
                object: current_entry.name.clone(),
                member: None,
                description: format!(
                    "Object '{}' ObsoleteState advanced from {} to {}",
                    current_entry.name,
                    obsolete_label(&old_entry.properties),
                    obsolete_label(&current_entry.properties)
                ),
                migration_hint: property(&current_entry.properties, "ObsoleteReason")
                    .map(|reason| format!("Migrate callers before removal: {reason}"))
                    .unwrap_or_else(|| {
                        "Migrate callers to the documented replacement before removal.".to_string()
                    }),
                severity: if obsolete_rank(&current_entry.properties) >= 2 {
                    "error".to_string()
                } else {
                    "warning".to_string()
                },
            });
        }

        for field in &current_entry.fields {
            let Some(old_field) = old_entry
                .fields
                .iter()
                .find(|old_field| old_field.id == field.id)
                .or_else(|| {
                    old_entry
                        .fields
                        .iter()
                        .find(|old_field| old_field.name.eq_ignore_ascii_case(&field.name))
                })
            else {
                continue;
            };
            if obsolete_rank(&field.properties) > obsolete_rank(&old_field.properties) {
                issues.push(UpgradeIssue {
                    kind: UpgradeIssueKind::ObsoleteSymbol,
                    object: current_entry.name.clone(),
                    member: Some(field.name.clone()),
                    description: format!(
                        "Field '{}.{}' ObsoleteState advanced from {} to {}",
                        current_entry.name,
                        field.name,
                        obsolete_label(&old_field.properties),
                        obsolete_label(&field.properties)
                    ),
                    migration_hint: property(&field.properties, "ObsoleteReason")
                        .map(|reason| format!("Migrate stored data and callers: {reason}"))
                        .unwrap_or_else(|| {
                            "Migrate stored data and callers before field removal.".to_string()
                        }),
                    severity: if obsolete_rank(&field.properties) >= 2 {
                        "error".to_string()
                    } else {
                        "warning".to_string()
                    },
                });
            }
        }

        for method in current_entry
            .methods
            .iter()
            .filter(|method| !method.is_local)
        {
            let newly_obsolete = method
                .attributes
                .iter()
                .any(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
                && !old_entry.methods.iter().any(|old_method| {
                    same_method_contract(old_method, method)
                        && old_method
                            .attributes
                            .iter()
                            .any(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
                });
            if newly_obsolete {
                issues.push(UpgradeIssue {
                    kind: UpgradeIssueKind::ObsoleteSymbol,
                    object: current_entry.name.clone(),
                    member: Some(method.name.clone()),
                    description: format!(
                        "Procedure '{}.{}' became obsolete",
                        current_entry.name, method.name
                    ),
                    migration_hint: method
                        .attributes
                        .iter()
                        .find(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
                        .and_then(|attribute| attribute.arguments.first())
                        .map(|reason| {
                            format!(
                                "Migrate callers before removal: {}",
                                reason.trim_matches('\'')
                            )
                        })
                        .unwrap_or_else(|| {
                            "Migrate callers to the documented replacement before removal."
                                .to_string()
                        }),
                    severity: "warning".to_string(),
                });
            }
        }
    }
}

fn detect_new_permissions(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
    issues: &mut Vec<UpgradeIssue>,
) {
    let baseline_map: std::collections::HashMap<_, _> = baseline
        .iter()
        .filter(|entry| !entry.synthetic)
        .map(|entry| (surface_key(entry), entry))
        .collect();
    for current_entry in current.iter().filter(|entry| {
        !entry.synthetic
            && matches!(
                entry.kind,
                al_symbols::ObjectKind::PermissionSet
                    | al_symbols::ObjectKind::PermissionSetExtension
            )
    }) {
        let old_permissions = baseline_map
            .get(&surface_key(current_entry))
            .map(|entry| entry.permissions.as_slice())
            .unwrap_or(&[]);
        for permission in &current_entry.permissions {
            let old_value = old_permissions
                .iter()
                .find(|old| {
                    old.permission_object == permission.permission_object
                        && old.object_id == permission.object_id
                })
                .map_or(0, |old| old.value);
            let added = permission.value & !old_value;
            if added == 0 {
                continue;
            }
            issues.push(UpgradeIssue {
                kind: UpgradeIssueKind::NewPermission,
                object: current_entry.name.clone(),
                member: Some(format!(
                    "{}:{}",
                    permission.permission_object, permission.object_id
                )),
                description: format!(
                    "Permission set '{}' adds mask {} for object kind {} ID {}",
                    current_entry.name, added, permission.permission_object, permission.object_id
                ),
                migration_hint:
                    "Review the added privilege against least-privilege and AppSource policy."
                        .to_string(),
                severity: "warning".to_string(),
            });
        }
    }
}

fn same_method_contract(
    baseline: &al_symbols::MethodSymbol,
    current: &al_symbols::MethodSymbol,
) -> bool {
    baseline.name.eq_ignore_ascii_case(&current.name)
        && baseline.parameters.len() == current.parameters.len()
        && baseline
            .parameters
            .iter()
            .zip(&current.parameters)
            .all(|(baseline, current)| {
                baseline
                    .type_name
                    .trim()
                    .trim_matches('"')
                    .eq_ignore_ascii_case(current.type_name.trim().trim_matches('"'))
                    && baseline.is_var == current.is_var
            })
}

fn check_data_migration_needs(
    old_table: &SymbolEntry,
    new_table: &SymbolEntry,
    issues: &mut Vec<UpgradeIssue>,
) {
    let old_fields: std::collections::HashMap<String, &al_symbols::FieldSymbol> = old_table
        .fields
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();

    let new_fields: std::collections::HashMap<String, &al_symbols::FieldSymbol> = new_table
        .fields
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();

    for (name_lower, old_field) in &old_fields {
        if let Some(new_field) = new_fields.get(name_lower) {
            if old_field.type_name.to_lowercase() != new_field.type_name.to_lowercase() {
                issues.push(UpgradeIssue {
                    kind: UpgradeIssueKind::DataMigration,
                    object: old_table.name.clone(),
                    member: Some(old_field.name.clone()),
                    description: format!(
                        "Field '{}' type changed from '{}' to '{}' — data migration required",
                        old_field.name, old_field.type_name, new_field.type_name
                    ),
                    migration_hint: format!(
                        "Create an upgrade codeunit that converts data in '{}' from {} to {}. Use OnUpgradePerDatabase or OnUpgradePerCompany trigger.",
                        old_field.name, old_field.type_name, new_field.type_name
                    ),
                    severity: "error".to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{
        AttributeSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, PermissionSymbol,
        PropertyValue, SymbolEntry,
    };

    fn make_codeunit(name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Test".to_string(),
            methods,
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn property(name: &str, value: &str) -> PropertyValue {
        PropertyValue {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn method(name: &str, type_name: &str, obsolete: bool) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: vec![ParameterSymbol {
                name: "Value".to_string(),
                type_name: type_name.to_string(),
                is_var: false,
            }],
            return_type: None,
            attributes: obsolete
                .then(|| AttributeSymbol {
                    name: "Obsolete".to_string(),
                    arguments: vec!["'Use replacement'".to_string(), "'27.0'".to_string()],
                })
                .into_iter()
                .collect(),
            is_local: false,
        }
    }

    #[test]
    fn upgrade_report_on_removed_object() {
        let baseline = vec![make_codeunit("Legacy CU", vec![])];
        let current: Vec<SymbolEntry> = vec![];

        let issues = upgrade_report(&baseline, &current);
        assert!(
            issues
                .iter()
                .any(|i| i.object == "Legacy CU" && i.kind == UpgradeIssueKind::BreakingChange),
            "Should detect removed object as upgrade issue: {:?}",
            issues
        );
    }

    #[test]
    fn upgrade_report_empty_on_identical() {
        let cu = make_codeunit("My CU", vec![]);
        let baseline = std::slice::from_ref(&cu);
        let issues = upgrade_report(baseline, baseline);
        assert!(issues.is_empty(), "No issues for identical versions");
    }

    #[test]
    fn field_type_change_triggers_data_migration() {
        let old_table = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "Amount".to_string(),
                type_name: "Integer".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        };

        let new_table = SymbolEntry {
            fields: vec![al_symbols::FieldSymbol {
                id: 1,
                name: "Amount".to_string(),
                type_name: "Decimal".to_string(),
                properties: vec![],
            }],
            ..old_table.clone()
        };

        let issues = upgrade_report(&[old_table], &[new_table]);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == UpgradeIssueKind::DataMigration),
            "Field type change should trigger data migration: {:?}",
            issues
        );
    }

    #[test]
    fn reports_object_field_and_correct_overload_obsolete_transitions() {
        let baseline = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Published Table".to_string(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "Legacy Field".to_string(),
                type_name: "Text".to_string(),
                properties: Vec::new(),
            }],
            methods: vec![
                method("Process", "Integer", false),
                method("Process", "Text", true),
            ],
            ..Default::default()
        };
        let current = SymbolEntry {
            properties: vec![
                property("ObsoleteState", "Pending"),
                property("ObsoleteReason", "Use Current Table"),
            ],
            fields: vec![FieldSymbol {
                id: 1,
                name: "Legacy Field".to_string(),
                type_name: "Text".to_string(),
                properties: vec![
                    property("ObsoleteState", "Pending"),
                    property("ObsoleteReason", "Use Current Field"),
                ],
            }],
            methods: vec![
                method("Process", "Integer", true),
                method("Process", "Text", true),
            ],
            ..baseline.clone()
        };

        let issues = upgrade_report_checked(&[baseline], &[current]).expect("valid surface");
        let obsolete: Vec<_> = issues
            .iter()
            .filter(|issue| issue.kind == UpgradeIssueKind::ObsoleteSymbol)
            .collect();
        assert_eq!(
            obsolete.len(),
            3,
            "object, field, and one overload: {issues:?}"
        );
        assert_eq!(
            obsolete
                .iter()
                .filter(|issue| issue.member.as_deref() == Some("Process"))
                .count(),
            1,
            "only the Integer overload became obsolete: {issues:?}"
        );
    }

    #[test]
    fn reports_only_new_permission_bits() {
        let baseline = SymbolEntry {
            kind: ObjectKind::PermissionSet,
            id: 50100,
            name: "Runtime Access".to_string(),
            permissions: vec![PermissionSymbol {
                permission_object: 5,
                object_id: 80,
                value: 1,
            }],
            ..Default::default()
        };
        let current = SymbolEntry {
            permissions: vec![PermissionSymbol {
                permission_object: 5,
                object_id: 80,
                value: 17,
            }],
            ..baseline.clone()
        };

        let issues = upgrade_report_checked(&[baseline], &[current]).expect("valid surface");
        let permission = issues
            .iter()
            .find(|issue| issue.kind == UpgradeIssueKind::NewPermission)
            .expect("execute bit addition must be reported");
        assert!(permission.description.contains("mask 16"), "{permission:?}");
    }

    #[test]
    fn namespace_move_does_not_compare_unrelated_table_storage() {
        let baseline = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Shared Name".to_string(),
            namespace: "Contoso.Legacy".to_string(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "Amount".to_string(),
                type_name: "Integer".to_string(),
                properties: Vec::new(),
            }],
            ..Default::default()
        };
        let current = SymbolEntry {
            namespace: "Contoso.Current".to_string(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "Amount".to_string(),
                type_name: "Decimal".to_string(),
                properties: Vec::new(),
            }],
            ..baseline.clone()
        };

        let issues = upgrade_report_checked(&[baseline], &[current]).expect("valid surface");
        assert!(
            issues
                .iter()
                .any(|issue| issue.description.contains("moved from namespace")),
            "namespace move must remain breaking: {issues:?}"
        );
        assert!(
            !issues
                .iter()
                .any(|issue| issue.kind == UpgradeIssueKind::DataMigration),
            "different namespace identities must not be compared as one table: {issues:?}"
        );
    }

    #[test]
    fn checked_upgrade_rejects_duplicate_surface_identity() {
        let duplicate = make_codeunit("Duplicate", Vec::new());
        let error = upgrade_report_checked(&[duplicate.clone(), duplicate], &[])
            .expect_err("ambiguous baseline must fail");
        assert!(error.contains("duplicate public identity"), "{error}");
    }
}
