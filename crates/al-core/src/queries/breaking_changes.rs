//! Cross-extension breaking change analysis.
//!
//! T1703: Compare two symbol sets (baseline vs current) to identify breaking changes.
//! Breaking changes are API surface removals or signature changes.

use serde::Serialize;
use std::collections::HashMap;

use al_symbols::{MethodSymbol, SymbolEntry};

/// Kind of breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BreakingChangeKind {
    /// Object was removed entirely.
    ObjectRemoved,
    /// Procedure was removed from an object.
    ProcedureRemoved,
    /// Procedure signature changed (parameter added/removed/reordered).
    SignatureChanged,
    /// Return type changed.
    ReturnTypeChanged,
    /// Field was removed from a table/page.
    FieldRemoved,
    /// Enum value was removed.
    EnumValueRemoved,
}

/// A single breaking change between baseline and current symbol sets.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakingChange {
    /// What kind of change this is.
    pub kind: BreakingChangeKind,
    /// Object affected.
    pub object: String,
    /// Member affected (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Human-readable description.
    pub description: String,
    /// Whether this is definitely breaking (vs potentially non-breaking).
    pub is_breaking: bool,
}

/// Compare two symbol entry lists and return breaking changes.
///
/// `baseline` is the old version; `current` is the new version.
pub fn analyze_breaking_changes(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
) -> Vec<BreakingChange> {
    let mut changes = Vec::new();

    // Build lookup maps by (kind, name) for quick access
    let baseline_map = build_map(baseline);
    let current_map = build_map(current);

    // Find removed objects
    for (key, old_entry) in &baseline_map {
        if !current_map.contains_key(key) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::ObjectRemoved,
                object: old_entry.name.clone(),
                member: None,
                description: format!(
                    "Object '{}' ({}) was removed",
                    old_entry.name, old_entry.kind
                ),
                is_breaking: true,
            });
            continue;
        }

        // Object still exists — check internal changes
        let new_entry = &current_map[key];
        diff_object(old_entry, new_entry, &mut changes);
    }

    changes
}

fn build_map(entries: &[SymbolEntry]) -> HashMap<(String, String), &SymbolEntry> {
    entries
        .iter()
        .map(|e| ((e.kind.to_string(), e.name.to_lowercase()), e))
        .collect()
}

fn diff_object(
    old: &SymbolEntry,
    new: &SymbolEntry,
    changes: &mut Vec<BreakingChange>,
) {
    let old_methods: HashMap<String, &MethodSymbol> = old
        .methods
        .iter()
        .filter(|m| !m.is_local)
        .map(|m| (m.name.to_lowercase(), m))
        .collect();

    let new_methods: HashMap<String, &MethodSymbol> = new
        .methods
        .iter()
        .filter(|m| !m.is_local)
        .map(|m| (m.name.to_lowercase(), m))
        .collect();

    // Check for removed or changed public methods
    for (name_lower, old_method) in &old_methods {
        match new_methods.get(name_lower) {
            None => {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::ProcedureRemoved,
                    object: old.name.clone(),
                    member: Some(old_method.name.clone()),
                    description: format!(
                        "Public procedure '{}' was removed from '{}'",
                        old_method.name, old.name
                    ),
                    is_breaking: true,
                });
            }
            Some(new_method) => {
                // Check signature compatibility
                check_signature_change(&old.name, old_method, new_method, changes);
            }
        }
    }

    // Check removed fields
    let old_fields: HashMap<String, _> = old.fields.iter().map(|f| (f.name.to_lowercase(), f)).collect();
    let new_fields: HashMap<String, _> = new.fields.iter().map(|f| (f.name.to_lowercase(), f)).collect();

    for (name_lower, old_field) in &old_fields {
        if !new_fields.contains_key(name_lower) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::FieldRemoved,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!(
                    "Field '{}' was removed from '{}'",
                    old_field.name, old.name
                ),
                is_breaking: true,
            });
        }
    }

    // Check removed enum values
    let new_enums: std::collections::HashSet<String> =
        new.enum_values.iter().map(|v| v.name.to_lowercase()).collect();

    for old_val in &old.enum_values {
        if !new_enums.contains(&old_val.name.to_lowercase()) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::EnumValueRemoved,
                object: old.name.clone(),
                member: Some(old_val.name.clone()),
                description: format!(
                    "Enum value '{}' was removed from '{}'",
                    old_val.name, old.name
                ),
                is_breaking: true,
            });
        }
    }
}

fn check_signature_change(
    object_name: &str,
    old: &MethodSymbol,
    new: &MethodSymbol,
    changes: &mut Vec<BreakingChange>,
) {
    // Return type change
    if old.return_type != new.return_type {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::ReturnTypeChanged,
            object: object_name.to_string(),
            member: Some(old.name.clone()),
            description: format!(
                "Return type of '{}' changed from '{:?}' to '{:?}'",
                old.name, old.return_type, new.return_type
            ),
            is_breaking: true,
        });
    }

    // Required parameter count change (removing required parameters is breaking,
    // adding required parameters is breaking, adding optional is non-breaking)
    let old_count = old.parameters.len();
    let new_count = new.parameters.len();

    if old_count != new_count {
        // Adding parameters to end is potentially non-breaking (caller can still compile)
        // but removing is always breaking
        let is_breaking = new_count < old_count;
        changes.push(BreakingChange {
            kind: BreakingChangeKind::SignatureChanged,
            object: object_name.to_string(),
            member: Some(old.name.clone()),
            description: format!(
                "Procedure '{}' parameter count changed from {} to {}",
                old.name, old_count, new_count
            ),
            is_breaking,
        });
    } else {
        // Check for type changes in existing parameters
        for (i, (op, np)) in old.parameters.iter().zip(new.parameters.iter()).enumerate() {
            if op.type_name.to_lowercase() != np.type_name.to_lowercase() {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::SignatureChanged,
                    object: object_name.to_string(),
                    member: Some(old.name.clone()),
                    description: format!(
                        "Parameter {} type changed from '{}' to '{}' in '{}'",
                        i + 1, op.type_name, np.type_name, old.name
                    ),
                    is_breaking: true,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, SymbolEntry};

    fn make_codeunit(name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
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

    fn make_method(name: &str, params: Vec<ParameterSymbol>, ret: Option<String>) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: params,
            return_type: ret,
            attributes: Vec::new(),
            is_local: false,
        }
    }

    fn make_param(name: &str, ty: &str) -> ParameterSymbol {
        ParameterSymbol {
            name: name.to_string(),
            type_name: ty.to_string(),
            is_var: false,
        }
    }

    #[test]
    fn detects_removed_object() {
        let baseline = vec![make_codeunit("Sales Post", vec![])];
        let current: Vec<SymbolEntry> = vec![];

        let changes = analyze_breaking_changes(&baseline, &current);
        assert!(
            changes.iter().any(|c| c.kind == BreakingChangeKind::ObjectRemoved && c.object == "Sales Post"),
            "Should detect removed object: {:?}", changes
        );
    }

    #[test]
    fn detects_removed_procedure() {
        let old_cu = make_codeunit("My CU", vec![
            make_method("PublicProc", vec![], None),
        ]);
        let new_cu = make_codeunit("My CU", vec![]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes.iter().any(|c| c.kind == BreakingChangeKind::ProcedureRemoved && c.member.as_deref() == Some("PublicProc")),
            "Should detect removed procedure: {:?}", changes
        );
    }

    #[test]
    fn detects_signature_change() {
        let old_cu = make_codeunit("My CU", vec![
            make_method("Process", vec![make_param("Amount", "Decimal")], None),
        ]);
        let new_cu = make_codeunit("My CU", vec![
            make_method("Process", vec![make_param("Amount", "Integer")], None),
        ]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes.iter().any(|c| c.kind == BreakingChangeKind::SignatureChanged),
            "Should detect parameter type change: {:?}", changes
        );
    }

    #[test]
    fn no_changes_for_identical_symbols() {
        let cu = make_codeunit("My CU", vec![
            make_method("Process", vec![make_param("Amount", "Decimal")], None),
        ]);

        let changes = analyze_breaking_changes(&[cu.clone()], &[cu]);
        assert!(changes.is_empty(), "No changes for identical symbols");
    }

    #[test]
    fn detects_removed_field() {
        let old_table = SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            package: "Base".to_string(),
            namespace: String::new(),
            methods: Vec::new(),
            fields: vec![
                FieldSymbol { id: 1, name: "No.".to_string(), type_name: "Code".to_string(), properties: vec![] },
                FieldSymbol { id: 2, name: "Old Field".to_string(), type_name: "Text".to_string(), properties: vec![] },
            ],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };

        let new_table = SymbolEntry {
            fields: vec![
                FieldSymbol { id: 1, name: "No.".to_string(), type_name: "Code".to_string(), properties: vec![] },
            ],
            ..old_table.clone()
        };

        let changes = analyze_breaking_changes(&[old_table], &[new_table]);

        assert!(
            changes.iter().any(|c| c.kind == BreakingChangeKind::FieldRemoved && c.member.as_deref() == Some("Old Field")),
            "Should detect removed field: {:?}", changes
        );
    }

    #[test]
    fn local_procedures_not_breaking() {
        let old_cu = make_codeunit("My CU", vec![
            MethodSymbol {
                name: "LocalHelper".to_string(),
                parameters: vec![],
                return_type: None,
                attributes: vec![],
                is_local: true,
            },
        ]);
        let new_cu = make_codeunit("My CU", vec![]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            !changes.iter().any(|c| c.member.as_deref() == Some("LocalHelper")),
            "Local procedures should not be flagged as breaking"
        );
    }

    #[test]
    fn adding_procedure_is_not_breaking() {
        let old_cu = make_codeunit("My CU", vec![]);
        let new_cu = make_codeunit("My CU", vec![
            make_method("NewProc", vec![], None),
        ]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(changes.is_empty(), "Adding a procedure is not a breaking change");
    }
}
