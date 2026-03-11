//! Object composition: merging base objects with their extensions.
//!
//! In AL, extensions add fields, methods, controls, and enum values to
//! base objects. This module merges a base object with all applicable
//! extensions to produce a `ComposedObject`.

use std::sync::Arc;

use crate::index::SymbolIndex;
use crate::model::{ComposedObject, ObjectKind, SymbolEntry};

/// Get a composed view of an object by merging the base with all extensions.
///
/// Returns `None` if no base object with the given kind and name is found.
pub fn get_composed(
    index: &SymbolIndex,
    kind: ObjectKind,
    name: &str,
) -> Option<ComposedObject> {
    // Don't compose extension objects themselves
    if kind.is_extension() {
        return None;
    }

    // Find the base object
    let candidates = index.get_by_name(name);
    let base = candidates
        .iter()
        .find(|e| e.kind == kind)?;

    // Find all extensions
    let extensions = index.get_extensions_of(name);
    let relevant_extensions: Vec<Arc<SymbolEntry>> = extensions
        .into_iter()
        .filter(|ext| {
            // Only include extensions of the matching kind
            ext.kind.base_kind() == Some(kind)
        })
        .collect();

    Some(compose(base, &relevant_extensions))
}

/// Compose a base object with a set of extensions.
fn compose(base: &SymbolEntry, extensions: &[Arc<SymbolEntry>]) -> ComposedObject {
    let mut all_fields = base.fields.clone();
    let mut all_methods = base.methods.clone();
    let mut all_controls = base.controls.clone();
    let mut all_enum_values = base.enum_values.clone();

    let mut ext_entries = Vec::new();

    for ext in extensions {
        all_fields.extend(ext.fields.iter().cloned());
        all_methods.extend(ext.methods.iter().cloned());
        all_controls.extend(ext.controls.iter().cloned());
        all_enum_values.extend(ext.enum_values.iter().cloned());
        ext_entries.push(ext.as_ref().clone());
    }

    // Sort fields by ID for consistent output
    all_fields.sort_by_key(|f| f.id);
    // Sort enum values by ordinal
    all_enum_values.sort_by_key(|v| v.ordinal);

    ComposedObject {
        base: base.clone(),
        extensions: ext_entries,
        all_fields,
        all_methods,
        all_controls,
        all_enum_values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn make_table(id: i32, name: &str, fields: Vec<FieldSymbol>, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Table,
            id,
            name: name.to_string(),
            extends: None,
            package: "Base".to_string(),
            methods,
            fields,
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_table_ext(id: i32, name: &str, extends: &str, fields: Vec<FieldSymbol>, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::TableExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            package: "Extension".to_string(),
            methods,
            fields,
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_enum(id: i32, name: &str, values: Vec<EnumValueSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Enum,
            id,
            name: name.to_string(),
            extends: None,
            package: "Base".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: values,
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_enum_ext(id: i32, name: &str, extends: &str, values: Vec<EnumValueSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::EnumExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            package: "Extension".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: values,
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn compose_table_with_extensions() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_table(18, "Customer", vec![
                FieldSymbol { id: 1, name: "No.".into(), type_name: "Code".into(), properties: vec![] },
                FieldSymbol { id: 2, name: "Name".into(), type_name: "Text".into(), properties: vec![] },
            ], vec![
                MethodSymbol {
                    name: "GetFullName".into(),
                    parameters: Vec::new(),
                    return_type: Some("Text".into()),
                    attributes: Vec::new(),
                    is_local: false,
                },
            ]),
            make_table_ext(50100, "Cust Ext 1", "Customer", vec![
                FieldSymbol { id: 50100, name: "Custom Field".into(), type_name: "Boolean".into(), properties: vec![] },
            ], vec![
                MethodSymbol {
                    name: "GetCustomValue".into(),
                    parameters: Vec::new(),
                    return_type: Some("Boolean".into()),
                    attributes: Vec::new(),
                    is_local: false,
                },
            ]),
            make_table_ext(50101, "Cust Ext 2", "Customer", vec![
                FieldSymbol { id: 50101, name: "Another Field".into(), type_name: "Integer".into(), properties: vec![] },
            ], Vec::new()),
        ]);

        let composed = get_composed(&index, ObjectKind::Table, "Customer").unwrap();
        assert_eq!(composed.base.name, "Customer");
        assert_eq!(composed.extensions.len(), 2);
        // 2 base fields + 2 extension fields
        assert_eq!(composed.all_fields.len(), 4);
        // Fields are sorted by ID
        assert_eq!(composed.all_fields[0].id, 1);
        assert_eq!(composed.all_fields[3].id, 50101);
        // 1 base method + 1 extension method
        assert_eq!(composed.all_methods.len(), 2);
    }

    #[test]
    fn compose_enum_with_extension() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_enum(50100, "Status", vec![
                EnumValueSymbol { ordinal: 0, name: "Open".into() },
                EnumValueSymbol { ordinal: 1, name: "Released".into() },
            ]),
            make_enum_ext(50100, "Status Ext", "Status", vec![
                EnumValueSymbol { ordinal: 10, name: "Custom".into() },
            ]),
        ]);

        let composed = get_composed(&index, ObjectKind::Enum, "Status").unwrap();
        assert_eq!(composed.all_enum_values.len(), 3);
        assert_eq!(composed.all_enum_values[0].ordinal, 0);
        assert_eq!(composed.all_enum_values[2].ordinal, 10);
    }

    #[test]
    fn compose_nonexistent_returns_none() {
        let index = SymbolIndex::new();
        assert!(get_composed(&index, ObjectKind::Table, "Nonexistent").is_none());
    }

    #[test]
    fn compose_extension_kind_returns_none() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_table_ext(50100, "Ext", "Customer", Vec::new(), Vec::new()),
        ]);
        assert!(get_composed(&index, ObjectKind::TableExtension, "Ext").is_none());
    }

    #[test]
    fn compose_no_extensions() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_table(50100, "Standalone", vec![
                FieldSymbol { id: 1, name: "F1".into(), type_name: "Text".into(), properties: vec![] },
            ], Vec::new()),
        ]);

        let composed = get_composed(&index, ObjectKind::Table, "Standalone").unwrap();
        assert!(composed.extensions.is_empty());
        assert_eq!(composed.all_fields.len(), 1);
    }
}
