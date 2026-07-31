//! Cross-extension breaking change analysis.
//!
//! Compare two symbol sets (baseline vs current) to identify breaking changes.
//! Breaking changes are API surface removals or signature changes.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use al_symbols::{MethodSymbol, SymbolEntry};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BreakingChangeKind {
    ObjectRemoved,
    ObjectIdChanged,
    NamespaceChanged,
    BaseObjectChanged,
    InterfaceRemoved,
    AccessReduced,
    ProcedureRemoved,
    /// Procedure signature changed (parameter added/removed/reordered).
    SignatureChanged,
    ReturnTypeChanged,
    /// Field was removed from a table/page.
    FieldRemoved,
    FieldRenamed,
    FieldIdChanged,
    FieldTypeChanged,
    EnumValueRemoved,
    EnumValueOrdinalChanged,
    PermissionReduced,
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
        let Some(new_entry) = current_map.get(key) else {
            let same_name: Vec<_> = current
                .iter()
                .filter(|candidate| {
                    !candidate.synthetic
                        && candidate.kind == old_entry.kind
                        && candidate.name.eq_ignore_ascii_case(&old_entry.name)
                })
                .collect();
            if same_name.len() == 1
                && !same_name[0]
                    .namespace
                    .eq_ignore_ascii_case(&old_entry.namespace)
            {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::NamespaceChanged,
                    object: old_entry.name.clone(),
                    member: None,
                    description: format!(
                        "Object '{}' moved from namespace '{}' to '{}'",
                        old_entry.name,
                        namespace_label(&old_entry.namespace),
                        namespace_label(&same_name[0].namespace)
                    ),
                    is_breaking: true,
                });
                continue;
            }
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
        };
        diff_object(old_entry, new_entry, &mut changes);
    }

    changes.sort_by(|left, right| {
        left.object
            .to_ascii_lowercase()
            .cmp(&right.object.to_ascii_lowercase())
            .then_with(|| left.member.cmp(&right.member))
            .then_with(|| left.description.cmp(&right.description))
    });
    changes
}

/// Checked entry point for user-provided/package-derived surfaces. Duplicate
/// public identities would otherwise be overwritten by a map and could hide a
/// removal, so callers at trust boundaries use this variant.
pub fn analyze_breaking_changes_checked(
    baseline: &[SymbolEntry],
    current: &[SymbolEntry],
) -> Result<Vec<BreakingChange>, String> {
    validate_unique_surface("baseline", baseline)?;
    validate_unique_surface("current", current)?;
    Ok(analyze_breaking_changes(baseline, current))
}

fn validate_unique_surface(label: &str, entries: &[SymbolEntry]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for entry in entries.iter().filter(|entry| !entry.synthetic) {
        let key = (
            entry.kind.to_string().to_ascii_lowercase(),
            entry.namespace.to_ascii_lowercase(),
            entry.name.to_ascii_lowercase(),
        );
        if !seen.insert(key) {
            return Err(format!(
                "{label} contains duplicate public identity {} {}.{}",
                entry.kind,
                namespace_label(&entry.namespace),
                entry.name
            ));
        }
        validate_entry_surface(label, entry)?;
    }
    Ok(())
}

fn validate_entry_surface(label: &str, entry: &SymbolEntry) -> Result<(), String> {
    let object = format!(
        "{} {}.{}",
        entry.kind,
        namespace_label(&entry.namespace),
        entry.name
    );

    let mut field_names = BTreeSet::new();
    let mut field_ids = BTreeSet::new();
    for field in &entry.fields {
        if !field_names.insert(field.name.to_ascii_lowercase()) {
            return Err(format!(
                "{label} {object} contains duplicate field name '{}'",
                field.name
            ));
        }
        if !field_ids.insert(field.id) {
            return Err(format!(
                "{label} {object} contains duplicate field ID {}",
                field.id
            ));
        }
    }

    let mut methods = BTreeSet::new();
    for method in entry.methods.iter().filter(|method| !method.is_local) {
        let signature = (
            method.name.to_ascii_lowercase(),
            method
                .parameters
                .iter()
                .map(|parameter| (normalize_name(&parameter.type_name), parameter.is_var))
                .collect::<Vec<_>>(),
        );
        if !methods.insert(signature) {
            return Err(format!(
                "{label} {object} contains duplicate public procedure contract '{}'",
                method_label(method)
            ));
        }
    }

    let mut enum_names = BTreeSet::new();
    let mut enum_ordinals = BTreeSet::new();
    for value in &entry.enum_values {
        if !enum_names.insert(value.name.to_ascii_lowercase()) {
            return Err(format!(
                "{label} {object} contains duplicate enum value name '{}'",
                value.name
            ));
        }
        if !enum_ordinals.insert(value.ordinal) {
            return Err(format!(
                "{label} {object} contains duplicate enum ordinal {}",
                value.ordinal
            ));
        }
    }

    let mut permissions = BTreeSet::new();
    for permission in &entry.permissions {
        if !matches!(permission.permission_object, 0 | 1 | 3 | 5 | 6 | 8 | 9 | 10) {
            return Err(format!(
                "{label} {object} contains unknown permission object code {}",
                permission.permission_object
            ));
        }
        if !(0..=31).contains(&permission.value) {
            return Err(format!(
                "{label} {object} contains invalid permission mask {}",
                permission.value
            ));
        }
        if !permissions.insert((permission.permission_object, permission.object_id)) {
            return Err(format!(
                "{label} {object} contains duplicate permission target {}:{}",
                permission.permission_object, permission.object_id
            ));
        }
    }

    Ok(())
}

fn build_map(entries: &[SymbolEntry]) -> BTreeMap<(String, String, String), &SymbolEntry> {
    entries
        .iter()
        .filter(|entry| !entry.synthetic)
        .map(|entry| {
            (
                (
                    entry.kind.to_string().to_ascii_lowercase(),
                    entry.namespace.to_ascii_lowercase(),
                    entry.name.to_ascii_lowercase(),
                ),
                entry,
            )
        })
        .collect()
}

fn namespace_label(namespace: &str) -> &str {
    if namespace.is_empty() {
        "(global)"
    } else {
        namespace
    }
}

fn diff_object(old: &SymbolEntry, new: &SymbolEntry, changes: &mut Vec<BreakingChange>) {
    if old.id != new.id {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::ObjectIdChanged,
            object: old.name.clone(),
            member: None,
            description: format!(
                "Object '{}' ID changed from {} to {}",
                old.name, old.id, new.id
            ),
            is_breaking: true,
        });
    }
    if normalize_optional_name(old.extends.as_deref())
        != normalize_optional_name(new.extends.as_deref())
    {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::BaseObjectChanged,
            object: old.name.clone(),
            member: None,
            description: format!(
                "Base object of '{}' changed from '{}' to '{}'",
                old.name,
                old.extends.as_deref().unwrap_or("(none)"),
                new.extends.as_deref().unwrap_or("(none)")
            ),
            is_breaking: true,
        });
    }
    let new_interfaces: BTreeSet<String> = new
        .implements
        .iter()
        .map(|name| normalize_name(name))
        .collect();
    for interface in &old.implements {
        if !new_interfaces.contains(&normalize_name(interface)) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::InterfaceRemoved,
                object: old.name.clone(),
                member: Some(interface.clone()),
                description: format!(
                    "Object '{}' no longer implements interface '{}'",
                    old.name, interface
                ),
                is_breaking: true,
            });
        }
    }
    if access_level(&old.properties) != "internal" && access_level(&new.properties) == "internal" {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::AccessReduced,
            object: old.name.clone(),
            member: None,
            description: format!("Object '{}' access changed to Internal", old.name),
            is_breaking: true,
        });
    }

    let current_public: Vec<&MethodSymbol> = new
        .methods
        .iter()
        .filter(|method| !method.is_local)
        .collect();
    for old_method in old.methods.iter().filter(|method| !method.is_local) {
        let same_name: Vec<_> = current_public
            .iter()
            .copied()
            .filter(|method| method.name.eq_ignore_ascii_case(&old_method.name))
            .collect();
        let exact = same_name
            .iter()
            .copied()
            .find(|method| parameter_contract_matches(old_method, method));
        if let Some(new_method) = exact {
            check_matching_signature(&old.name, old_method, new_method, changes);
        } else if same_name.is_empty() {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::ProcedureRemoved,
                object: old.name.clone(),
                member: Some(old_method.name.clone()),
                description: format!(
                    "Public procedure '{}' was removed from '{}'",
                    method_label(old_method),
                    old.name
                ),
                is_breaking: true,
            });
        } else if same_name.len() == 1 {
            check_incompatible_signature(&old.name, old_method, same_name[0], changes);
        } else {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::SignatureChanged,
                object: old.name.clone(),
                member: Some(old_method.name.clone()),
                description: format!(
                    "No current overload of '{}' preserves baseline signature {}",
                    old_method.name,
                    method_label(old_method)
                ),
                is_breaking: true,
            });
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
        let Some(new_field) = new_fields.get(name_lower) else {
            if let Some(renamed) = new.fields.iter().find(|field| field.id == old_field.id) {
                changes.push(BreakingChange {
                    kind: BreakingChangeKind::FieldRenamed,
                    object: old.name.clone(),
                    member: Some(old_field.name.clone()),
                    description: format!(
                        "Field '{}' (ID {}) in '{}' was renamed to '{}'",
                        old_field.name, old_field.id, old.name, renamed.name
                    ),
                    is_breaking: true,
                });
                continue;
            }
            changes.push(BreakingChange {
                kind: BreakingChangeKind::FieldRemoved,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!("Field '{}' was removed from '{}'", old_field.name, old.name),
                is_breaking: true,
            });
            continue;
        };
        if old_field.id != new_field.id {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::FieldIdChanged,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!(
                    "Field '{}' in '{}' changed ID from {} to {}",
                    old_field.name, old.name, old_field.id, new_field.id
                ),
                is_breaking: true,
            });
        }
        if normalize_name(&old_field.type_name) != normalize_name(&new_field.type_name) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::FieldTypeChanged,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!(
                    "Field '{}' in '{}' changed type from '{}' to '{}'",
                    old_field.name, old.name, old_field.type_name, new_field.type_name
                ),
                is_breaking: true,
            });
        }
        if access_level(&old_field.properties) != "internal"
            && access_level(&new_field.properties) == "internal"
        {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::AccessReduced,
                object: old.name.clone(),
                member: Some(old_field.name.clone()),
                description: format!(
                    "Field '{}' in '{}' access changed to Internal",
                    old_field.name, old.name
                ),
                is_breaking: true,
            });
        }
    }

    for old_val in &old.enum_values {
        let current_value = new
            .enum_values
            .iter()
            .find(|value| value.name.eq_ignore_ascii_case(&old_val.name));
        let Some(current_value) = current_value else {
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
            continue;
        };
        if old_val.ordinal != current_value.ordinal {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::EnumValueOrdinalChanged,
                object: old.name.clone(),
                member: Some(old_val.name.clone()),
                description: format!(
                    "Enum value '{}' in '{}' changed ordinal from {} to {}",
                    old_val.name, old.name, old_val.ordinal, current_value.ordinal
                ),
                is_breaking: true,
            });
        }
    }

    for old_permission in &old.permissions {
        let current = new.permissions.iter().find(|permission| {
            permission.permission_object == old_permission.permission_object
                && permission.object_id == old_permission.object_id
        });
        if current.is_none_or(|permission| {
            permission.value & old_permission.value != old_permission.value
        }) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::PermissionReduced,
                object: old.name.clone(),
                member: Some(format!(
                    "{}:{}",
                    old_permission.permission_object, old_permission.object_id
                )),
                description: format!(
                    "Permission grant {}:{} in '{}' was removed or reduced from mask {}",
                    old_permission.permission_object,
                    old_permission.object_id,
                    old.name,
                    old_permission.value
                ),
                is_breaking: true,
            });
        }
    }
}

fn check_incompatible_signature(
    object_name: &str,
    old: &MethodSymbol,
    new: &MethodSymbol,
    changes: &mut Vec<BreakingChange>,
) {
    if old.parameters.len() != new.parameters.len() {
        changes.push(BreakingChange {
            kind: BreakingChangeKind::SignatureChanged,
            object: object_name.to_string(),
            member: Some(old.name.clone()),
            description: format!(
                "Procedure '{}' parameter count changed from {} to {}",
                old.name,
                old.parameters.len(),
                new.parameters.len()
            ),
            is_breaking: true,
        });
        return;
    }
    for (index, (old_parameter, new_parameter)) in
        old.parameters.iter().zip(&new.parameters).enumerate()
    {
        if normalize_name(&old_parameter.type_name) != normalize_name(&new_parameter.type_name) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::SignatureChanged,
                object: object_name.to_string(),
                member: Some(old.name.clone()),
                description: format!(
                    "Parameter {} type changed from '{}' to '{}' in '{}'",
                    index + 1,
                    old_parameter.type_name,
                    new_parameter.type_name,
                    old.name
                ),
                is_breaking: true,
            });
        }
        if old_parameter.is_var != new_parameter.is_var {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::SignatureChanged,
                object: object_name.to_string(),
                member: Some(old.name.clone()),
                description: format!(
                    "Parameter {} '{}' modifier changed from {} to {} in '{}'",
                    index + 1,
                    old_parameter.name,
                    if old_parameter.is_var {
                        "var"
                    } else {
                        "non-var"
                    },
                    if new_parameter.is_var {
                        "var"
                    } else {
                        "non-var"
                    },
                    old.name
                ),
                is_breaking: true,
            });
        }
    }
}

fn check_matching_signature(
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

    for (index, (old_parameter, new_parameter)) in
        old.parameters.iter().zip(&new.parameters).enumerate()
    {
        if !old_parameter.name.eq_ignore_ascii_case(&new_parameter.name) {
            changes.push(BreakingChange {
                kind: BreakingChangeKind::SignatureChanged,
                object: object_name.to_string(),
                member: Some(old.name.clone()),
                description: format!(
                    "Parameter {} in '{}' was renamed from '{}' to '{}'",
                    index + 1,
                    old.name,
                    old_parameter.name,
                    new_parameter.name
                ),
                is_breaking: true,
            });
        }
    }
}

fn parameter_contract_matches(old: &MethodSymbol, new: &MethodSymbol) -> bool {
    old.parameters.len() == new.parameters.len()
        && old
            .parameters
            .iter()
            .zip(&new.parameters)
            .all(|(old, new)| {
                normalize_name(&old.type_name) == normalize_name(&new.type_name)
                    && old.is_var == new.is_var
            })
}

fn method_label(method: &MethodSymbol) -> String {
    let parameters = method
        .parameters
        .iter()
        .map(|parameter| {
            format!(
                "{}{}",
                if parameter.is_var { "var " } else { "" },
                parameter.type_name
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}({parameters})", method.name)
}

fn normalize_name(value: &str) -> String {
    value.trim().trim_matches('"').to_ascii_lowercase()
}

fn normalize_optional_name(value: Option<&str>) -> Option<String> {
    value.map(normalize_name)
}

fn access_level(properties: &[al_symbols::PropertyValue]) -> String {
    properties
        .iter()
        .find(|property| property.name.eq_ignore_ascii_case("Access"))
        .map(|property| normalize_name(&property.value))
        .unwrap_or_else(|| "public".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{
        EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, PermissionSymbol,
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

    fn property(name: &str, value: &str) -> PropertyValue {
        PropertyValue {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn has_kind(changes: &[BreakingChange], kind: BreakingChangeKind) -> bool {
        changes.iter().any(|change| change.kind == kind)
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
            permissions: Vec::new(),
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
            permissions: Vec::new(),
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

    #[test]
    fn detects_object_identity_base_interface_and_access_contract_changes() {
        let mut baseline = make_codeunit("Published API", Vec::new());
        baseline.extends = Some("Old Base".to_string());
        baseline.implements = vec!["IKeep".to_string(), "IRemoved".to_string()];
        baseline.properties = vec![property("Access", "Public")];

        let mut current = baseline.clone();
        current.id = 50101;
        current.extends = Some("New Base".to_string());
        current.implements = vec!["IKeep".to_string()];
        current.properties = vec![property("Access", "Internal")];

        let changes = analyze_breaking_changes(&[baseline], &[current]);
        for kind in [
            BreakingChangeKind::ObjectIdChanged,
            BreakingChangeKind::BaseObjectChanged,
            BreakingChangeKind::InterfaceRemoved,
            BreakingChangeKind::AccessReduced,
        ] {
            assert!(
                has_kind(&changes, kind.clone()),
                "missing {kind:?}: {changes:?}"
            );
        }
    }

    #[test]
    fn detects_namespace_move_without_reporting_removal() {
        let mut baseline = make_codeunit("Published API", Vec::new());
        baseline.namespace = "Contoso.Legacy".to_string();
        let mut current = baseline.clone();
        current.namespace = "Contoso.Current".to_string();

        let changes = analyze_breaking_changes(&[baseline], &[current]);
        assert!(has_kind(&changes, BreakingChangeKind::NamespaceChanged));
        assert!(!has_kind(&changes, BreakingChangeKind::ObjectRemoved));
    }

    #[test]
    fn detects_field_rename_id_type_and_access_changes() {
        let baseline = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Published Table".to_string(),
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "Legacy Name".to_string(),
                    type_name: "Text[100]".to_string(),
                    properties: Vec::new(),
                },
                FieldSymbol {
                    id: 2,
                    name: "Stable Name".to_string(),
                    type_name: "Integer".to_string(),
                    properties: vec![property("Access", "Public")],
                },
            ],
            ..Default::default()
        };
        let current = SymbolEntry {
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "Current Name".to_string(),
                    type_name: "Text[100]".to_string(),
                    properties: Vec::new(),
                },
                FieldSymbol {
                    id: 20,
                    name: "Stable Name".to_string(),
                    type_name: "Decimal".to_string(),
                    properties: vec![property("Access", "Internal")],
                },
            ],
            ..baseline.clone()
        };

        let changes = analyze_breaking_changes(&[baseline], &[current]);
        for kind in [
            BreakingChangeKind::FieldRenamed,
            BreakingChangeKind::FieldIdChanged,
            BreakingChangeKind::FieldTypeChanged,
            BreakingChangeKind::AccessReduced,
        ] {
            assert!(
                has_kind(&changes, kind.clone()),
                "missing {kind:?}: {changes:?}"
            );
        }
    }

    #[test]
    fn detects_enum_ordinal_and_permission_reductions() {
        let baseline_enum = SymbolEntry {
            kind: ObjectKind::Enum,
            id: 50100,
            name: "Published Status".to_string(),
            enum_values: vec![EnumValueSymbol {
                ordinal: 1,
                name: "Open".to_string(),
            }],
            ..Default::default()
        };
        let current_enum = SymbolEntry {
            enum_values: vec![EnumValueSymbol {
                ordinal: 5,
                name: "Open".to_string(),
            }],
            ..baseline_enum.clone()
        };

        let baseline_permissions = SymbolEntry {
            kind: ObjectKind::PermissionSet,
            id: 50101,
            name: "Published Permissions".to_string(),
            permissions: vec![PermissionSymbol {
                permission_object: 5,
                object_id: 80,
                value: 17,
            }],
            ..Default::default()
        };
        let current_permissions = SymbolEntry {
            permissions: vec![PermissionSymbol {
                permission_object: 5,
                object_id: 80,
                value: 1,
            }],
            ..baseline_permissions.clone()
        };

        let changes = analyze_breaking_changes(
            &[baseline_enum, baseline_permissions],
            &[current_enum, current_permissions],
        );
        assert!(has_kind(
            &changes,
            BreakingChangeKind::EnumValueOrdinalChanged
        ));
        assert!(has_kind(&changes, BreakingChangeKind::PermissionReduced));
    }

    #[test]
    fn detects_parameter_rename_in_preserved_overload() {
        let baseline = make_codeunit(
            "Published API",
            vec![make_method(
                "Process",
                vec![make_param("OldName", "Text")],
                None,
            )],
        );
        let current = make_codeunit(
            "Published API",
            vec![make_method(
                "Process",
                vec![make_param("NewName", "Text")],
                None,
            )],
        );

        let changes = analyze_breaking_changes(&[baseline], &[current]);
        assert!(changes.iter().any(|change| {
            change.kind == BreakingChangeKind::SignatureChanged
                && change.description.contains("renamed")
        }));
    }

    #[test]
    fn checked_analysis_rejects_ambiguous_or_invalid_surfaces() {
        let duplicate = make_codeunit("Duplicate", Vec::new());
        let error = analyze_breaking_changes_checked(&[duplicate.clone(), duplicate], &[])
            .expect_err("duplicate object identity must fail");
        assert!(error.contains("duplicate public identity"), "{error}");

        let invalid_fields = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Invalid Fields".to_string(),
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "First".to_string(),
                    type_name: "Text".to_string(),
                    properties: Vec::new(),
                },
                FieldSymbol {
                    id: 1,
                    name: "Second".to_string(),
                    type_name: "Text".to_string(),
                    properties: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let error = analyze_breaking_changes_checked(&[invalid_fields], &[])
            .expect_err("duplicate field IDs must fail");
        assert!(error.contains("duplicate field ID"), "{error}");

        let invalid_permissions = SymbolEntry {
            kind: ObjectKind::PermissionSet,
            id: 50101,
            name: "Invalid Permissions".to_string(),
            permissions: vec![PermissionSymbol {
                permission_object: 5,
                object_id: 80,
                value: 32,
            }],
            ..Default::default()
        };
        let error = analyze_breaking_changes_checked(&[invalid_permissions], &[])
            .expect_err("unknown permission bits must fail");
        assert!(error.contains("invalid permission mask"), "{error}");
    }
}
