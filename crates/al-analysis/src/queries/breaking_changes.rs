//! Cross-extension breaking change analysis.
//!
//! T1703: Compare two symbol sets (baseline vs current) to identify breaking changes.
//! Breaking changes are API surface removals or signature changes.

use serde::Serialize;
use std::collections::BTreeMap;

use al_symbols::{MethodSymbol, SymbolEntry};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BreakingChangeKind {
    ObjectRemoved,
    ProcedureRemoved,
    /// Procedure signature changed (parameter added/removed/reordered).
    SignatureChanged,
    ReturnTypeChanged,
    /// Field was removed from a table/page.
    FieldRemoved,
    EnumValueRemoved,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakingChange {
    pub kind: BreakingChangeKind,
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub description: String,
    /// Whether this is definitely breaking (vs potentially non-breaking).
    pub is_breaking: bool,
}

pub fn analyze_breaking_changes(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
) -> Vec<BreakingChange> {
    let mut changes = Vec::new();

    let baseline_map = build_map(baseline);
    let current_map = build_map(current);

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

        let new_entry = &current_map[key];
        diff_object(old_entry, new_entry, &mut changes);
    }

    changes
}

fn build_map(entries: &[SymbolEntry]) -> BTreeMap<(String, String), &SymbolEntry> {
    entries
        .iter()
        .map(|e| ((e.kind.to_string(), e.name.to_lowercase()), e))
        .collect()
}

fn diff_object(old: &SymbolEntry, new: &SymbolEntry, changes: &mut Vec<BreakingChange>) {
    let old_methods: BTreeMap<String, &MethodSymbol> = old
        .methods
        .iter()
        .filter(|m| !m.is_local)
        .map(|m| (m.name.to_lowercase(), m))
        .collect();

    let new_methods: BTreeMap<String, &MethodSymbol> = new
        .methods
        .iter()
        .filter(|m| !m.is_local)
        .map(|m| (m.name.to_lowercase(), m))
        .collect();

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
                check_signature_change(&old.name, old_method, new_method, changes);
            }
        }
    }

    let old_fields: BTreeMap<String, _> = old
        .fields
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();
    let new_fields: BTreeMap<String, _> = new
        .fields
        .iter()
        .map(|f| (f.name.to_lowercase(), f))
        .collect();

    for (name_lower, old_field) in &old_fields {
        if !new_fields.contains_key(name_lower) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::FieldRemoved,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!("Field '{}' was removed from '{}'", old_field.name, old.name),
                is_breaking: true,
            });
        }
    }

    for old_val in &old.enum_values {
        let old_lower = old_val.name.to_lowercase();
        if !new
            .enum_values
            .iter()
            .any(|n| n.name.to_lowercase() == old_lower)
        {
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
    // Return type change. AL type names are case-insensitive, so compare
    // normalized — matching the parameter-type comparison below.
    let old_ret = old.return_type.as_deref();
    let new_ret = new.return_type.as_deref();
    if old_ret.map(str::to_lowercase) != new_ret.map(str::to_lowercase) {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::ReturnTypeChanged,
            object: object_name.to_string(),
            member: Some(old.name.clone()),
            description: format!(
                "Return type of '{}' changed from '{}' to '{}'",
                old.name,
                old_ret.unwrap_or("(none)"),
                new_ret.unwrap_or("(none)")
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
        for (i, (op, np)) in old.parameters.iter().zip(new.parameters.iter()).enumerate() {
            if op.type_name.to_lowercase() != np.type_name.to_lowercase() {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::SignatureChanged,
                    object: object_name.to_string(),
                    member: Some(old.name.clone()),
                    description: format!(
                        "Parameter {} type changed from '{}' to '{}' in '{}'",
                        i + 1,
                        op.type_name,
                        np.type_name,
                        old.name
                    ),
                    is_breaking: true,
                });
            }

            // A `var` (pass-by-reference) modifier change is breaking: callers
            // passing a constant break if a parameter becomes `var`, and callers
            // relying on reference semantics break if `var` is removed.
            if op.is_var != np.is_var {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::SignatureChanged,
                    object: object_name.to_string(),
                    member: Some(old.name.clone()),
                    description: format!(
                        "Parameter {} '{}' modifier changed from {} to {} in '{}'",
                        i + 1,
                        op.name,
                        if op.is_var { "var" } else { "non-var" },
                        if np.is_var { "var" } else { "non-var" },
                        old.name
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

    fn make_var_param(name: &str, ty: &str, is_var: bool) -> ParameterSymbol {
        ParameterSymbol {
            name: name.to_string(),
            type_name: ty.to_string(),
            is_var,
        }
    }

    #[test]
    fn detects_removed_object() {
        let baseline = vec![make_codeunit("Sales Post", vec![])];
        let current: Vec<SymbolEntry> = vec![];

        let changes = analyze_breaking_changes(&baseline, &current);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::ObjectRemoved && c.object == "Sales Post"),
            "Should detect removed object: {:?}",
            changes
        );
    }

    #[test]
    fn detects_removed_procedure() {
        let old_cu = make_codeunit("My CU", vec![make_method("PublicProc", vec![], None)]);
        let new_cu = make_codeunit("My CU", vec![]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::ProcedureRemoved
                    && c.member.as_deref() == Some("PublicProc")),
            "Should detect removed procedure: {:?}",
            changes
        );
    }

    #[test]
    fn detects_signature_change() {
        let old_cu = make_codeunit(
            "My CU",
            vec![make_method(
                "Process",
                vec![make_param("Amount", "Decimal")],
                None,
            )],
        );
        let new_cu = make_codeunit(
            "My CU",
            vec![make_method(
                "Process",
                vec![make_param("Amount", "Integer")],
                None,
            )],
        );

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::SignatureChanged),
            "Should detect parameter type change: {:?}",
            changes
        );
    }

    #[test]
    fn no_changes_for_identical_symbols() {
        let cu = make_codeunit(
            "My CU",
            vec![make_method(
                "Process",
                vec![make_param("Amount", "Decimal")],
                None,
            )],
        );

        let baseline = std::slice::from_ref(&cu);
        let changes = analyze_breaking_changes(baseline, baseline);
        assert!(changes.is_empty(), "No changes for identical symbols");
    }

    #[test]
    fn detects_removed_field() {
        let old_table = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            package: "Base".to_string(),
            namespace: String::new(),
            methods: Vec::new(),
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code".to_string(),
                    properties: vec![],
                },
                FieldSymbol {
                    id: 2,
                    name: "Old Field".to_string(),
                    type_name: "Text".to_string(),
                    properties: vec![],
                },
            ],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };

        let new_table = SymbolEntry {
            fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            ..old_table.clone()
        };

        let changes = analyze_breaking_changes(&[old_table], &[new_table]);

        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::FieldRemoved
                    && c.member.as_deref() == Some("Old Field")),
            "Should detect removed field: {:?}",
            changes
        );
    }

    #[test]
    fn local_procedures_not_breaking() {
        let old_cu = make_codeunit(
            "My CU",
            vec![MethodSymbol {
                name: "LocalHelper".to_string(),
                parameters: vec![],
                return_type: None,
                attributes: vec![],
                is_local: true,
            }],
        );
        let new_cu = make_codeunit("My CU", vec![]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            !changes
                .iter()
                .any(|c| c.member.as_deref() == Some("LocalHelper")),
            "Local procedures should not be flagged as breaking"
        );
    }

    #[test]
    fn detects_return_type_change() {
        // T019: previously untested ReturnTypeChanged variant.
        let old_cu = make_codeunit(
            "Calc",
            vec![make_method("Total", vec![], Some("Decimal".to_string()))],
        );
        let new_cu = make_codeunit(
            "Calc",
            vec![make_method("Total", vec![], Some("Integer".to_string()))],
        );
        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::ReturnTypeChanged
                    && c.member.as_deref() == Some("Total")),
            "ReturnTypeChanged must be reported: {changes:?}"
        );
    }

    #[test]
    fn detects_enum_value_removed() {
        // T019: previously untested EnumValueRemoved variant.
        use al_symbols::EnumValueSymbol;
        let make_enum = |values: Vec<&str>| SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Enum,
            id: 50100,
            name: "Status".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Test".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: values
                .into_iter()
                .enumerate()
                .map(|(i, n)| EnumValueSymbol {
                    ordinal: i as i32,
                    name: n.to_string(),
                })
                .collect(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };
        let baseline = vec![make_enum(vec!["Open", "Pending", "Closed"])];
        let current = vec![make_enum(vec!["Open", "Closed"])];
        let changes = analyze_breaking_changes(&baseline, &current);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::EnumValueRemoved
                    && c.member.as_deref() == Some("Pending")),
            "EnumValueRemoved must be reported when a value disappears: {changes:?}"
        );
    }

    #[test]
    fn detects_signature_change_parameter_count() {
        // T019: signature-change tests previously only covered TYPE changes;
        // adding/removing a parameter is also a SignatureChanged report.
        let old_cu = make_codeunit(
            "API",
            vec![make_method("Send", vec![make_param("Body", "Text")], None)],
        );
        let new_cu = make_codeunit(
            "API",
            vec![make_method(
                "Send",
                vec![make_param("Body", "Text"), make_param("Timeout", "Integer")],
                None,
            )],
        );
        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::SignatureChanged
                    && c.member.as_deref() == Some("Send")),
            "SignatureChanged must be reported on parameter-count delta: {changes:?}"
        );
    }

    #[test]
    fn detects_parameter_var_modifier_change() {
        // A parameter flipping between value and reference passing is breaking.
        let old_cu = make_codeunit(
            "API",
            vec![make_method(
                "Process",
                vec![make_var_param("Rec", "Record Customer", false)],
                None,
            )],
        );
        let new_cu = make_codeunit(
            "API",
            vec![make_method(
                "Process",
                vec![make_var_param("Rec", "Record Customer", true)],
                None,
            )],
        );
        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::SignatureChanged
                    && c.member.as_deref() == Some("Process")
                    && c.description.contains("modifier changed")),
            "var modifier change must be reported as SignatureChanged: {changes:?}"
        );
    }

    #[test]
    fn return_type_change_is_case_insensitive() {
        // AL type names are case-insensitive; a pure case difference in the
        // return type must NOT be reported as a breaking change.
        let old_cu = make_codeunit(
            "Calc",
            vec![make_method("Total", vec![], Some("Decimal".to_string()))],
        );
        let new_cu = make_codeunit(
            "Calc",
            vec![make_method("Total", vec![], Some("decimal".to_string()))],
        );
        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            !changes
                .iter()
                .any(|c| c.kind == BreakingChangeKind::ReturnTypeChanged),
            "case-only return type difference must not be breaking: {changes:?}"
        );
    }

    #[test]
    fn return_type_change_description_is_human_readable() {
        // Description must read 'Decimal'/'(none)', never Debug 'Some(...)'.
        let old_cu = make_codeunit(
            "Calc",
            vec![make_method("Total", vec![], Some("Decimal".to_string()))],
        );
        let new_cu = make_codeunit("Calc", vec![make_method("Total", vec![], None)]);
        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        let change = changes
            .iter()
            .find(|c| c.kind == BreakingChangeKind::ReturnTypeChanged)
            .expect("return type change reported");
        assert!(
            !change.description.contains("Some("),
            "description must not contain Debug formatting: {}",
            change.description
        );
        assert!(
            change.description.contains("'Decimal'") && change.description.contains("'(none)'"),
            "description should use clean values: {}",
            change.description
        );
    }

    #[test]
    fn adding_procedure_is_not_breaking() {
        let old_cu = make_codeunit("My CU", vec![]);
        let new_cu = make_codeunit("My CU", vec![make_method("NewProc", vec![], None)]);

        let changes = analyze_breaking_changes(&[old_cu], &[new_cu]);
        assert!(
            changes.is_empty(),
            "Adding a procedure is not a breaking change"
        );
    }
}
