//! Upgrade impact analysis with migration hints.
//!
//! T1707: Generate an upgrade report from two .app versions using breaking change analysis.
//! Depends on T1703 (breaking_changes module).

use serde::Serialize;

use super::breaking_changes::{analyze_breaking_changes, BreakingChange, BreakingChangeKind};
use crate::symbols::SymbolEntry;

/// Category of upgrade issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpgradeIssueKind {
    /// Breaking API change that requires code updates.
    BreakingChange,
    /// Table structure change that may require data migration.
    DataMigration,
    /// Obsoleted symbol that should be replaced.
    ObsoleteSymbol,
    /// New required permission.
    NewPermission,
}

/// A single upgrade issue with migration hints.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeIssue {
    /// Category.
    pub kind: UpgradeIssueKind,
    /// Affected object.
    pub object: String,
    /// Affected member (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Description of the issue.
    pub description: String,
    /// Suggested migration action.
    pub migration_hint: String,
    /// Severity: "error", "warning", "info".
    pub severity: String,
}

/// Generate an upgrade report comparing two symbol sets.
pub fn upgrade_report(baseline: &[SymbolEntry], current: &[SymbolEntry]) -> Vec<UpgradeIssue> {
    let mut issues = Vec::new();

    // Get breaking changes from T1703
    let breaking = analyze_breaking_changes(baseline, current);
    for change in breaking {
        let issue = breaking_change_to_upgrade_issue(change);
        issues.push(issue);
    }

    // Find data migration needs (field type changes in tables)
    let baseline_tables: std::collections::HashMap<String, &SymbolEntry> = baseline
        .iter()
        .filter(|e| matches!(e.kind, crate::symbols::ObjectKind::Table))
        .map(|e| (e.name.to_lowercase(), e))
        .collect();

    let current_tables: std::collections::HashMap<String, &SymbolEntry> = current
        .iter()
        .filter(|e| matches!(e.kind, crate::symbols::ObjectKind::Table))
        .map(|e| (e.name.to_lowercase(), e))
        .collect();

    for (name_lower, old_table) in &baseline_tables {
        if let Some(new_table) = current_tables.get(name_lower) {
            check_data_migration_needs(old_table, new_table, &mut issues);
        }
    }

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

fn check_data_migration_needs(
    old_table: &SymbolEntry,
    new_table: &SymbolEntry,
    issues: &mut Vec<UpgradeIssue>,
) {
    let old_fields: std::collections::HashMap<String, &crate::symbols::FieldSymbol> = old_table
        .fields
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();

    let new_fields: std::collections::HashMap<String, &crate::symbols::FieldSymbol> = new_table
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
    use crate::symbols::{MethodSymbol, ObjectKind, SymbolEntry};

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
            variables: Vec::new(),
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
        use crate::symbols::FieldSymbol;

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
            variables: Vec::new(),
        };

        let new_table = SymbolEntry {
            fields: vec![crate::symbols::FieldSymbol {
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
}
