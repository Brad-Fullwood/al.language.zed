//! Table impact analysis for the AL insight engine.
//!
//! Given a table name, finds all objects in the workspace that interact with
//! it via: Record variable declarations, Record-typed parameters, TableRelation
//! properties, and Extends relationships.
//!
//! Used by `al impact <TableName>` queries.

use std::collections::HashMap;

use serde::Serialize;

use al_symbols::{ObjectKind, SymbolIndex};

/// How an object interacts with a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableOperationKind {
    /// Object declares a Record variable of this table type.
    /// This enables Read (Get, FindSet, FindFirst, FindLast, CalcFields) and
    /// Write (Insert, Modify, Delete) operations.
    RecordVariable,
    RecordParameter,
    /// This object has a field with a TableRelation property pointing to the table.
    Relation,
    /// This object is a TableExtension that extends the target table.
    Extends,
}

impl std::fmt::Display for TableOperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableOperationKind::RecordVariable => write!(f, "record_variable"),
            TableOperationKind::RecordParameter => write!(f, "record_parameter"),
            TableOperationKind::Relation => write!(f, "relation"),
            TableOperationKind::Extends => write!(f, "extends"),
        }
    }
}

/// A single impact site: where an object touches the target table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableImpact {
    pub operation: TableOperationKind,
    /// Optional: the name of the variable, parameter, or field that references the table.
    pub location_hint: Option<String>,
}

/// All impacts for one AL object.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectImpact {
    pub object_kind: String,
    pub object_name: String,
    pub package: String,
    pub impacts: Vec<TableImpact>,
}

/// Full result of a table impact query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableImpactResult {
    /// The table name that was queried (normalized to original casing from index).
    pub table_name: String,
    /// All objects that reference this table, grouped by object.
    pub objects: Vec<ObjectImpact>,
    /// Total number of impact sites across all objects.
    pub total_impacts: usize,
}

/// Analyse all objects in the symbol index that interact with the named table.
///
/// Detects:
/// - `Extends`: TableExtension objects whose `extends` field matches the table name.
/// - `Relation`: Fields on any table/table-extension with a `TableRelation` property
///   pointing to the target table.
/// - `RecordVariable`: Global variables whose type is `Record "<TableName>"`.
/// - `RecordParameter`: Procedure parameters typed as `Record "<TableName>"`.
///
/// Results are grouped by object and sorted by object name for stable output.
pub fn table_impact(symbols: &SymbolIndex, table_name: &str) -> TableImpactResult {
    // All comparisons use `eq_ignore_ascii_case` so we can pass `table_name`
    // directly without allocating a lowercased copy. AL identifiers are ASCII.
    let canonical_name = symbols
        .get_by_name(table_name)
        .into_iter()
        .find(|e| e.kind == ObjectKind::Table)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| table_name.to_string());

    let all_entries = symbols.all_entries();

    let mut by_object: HashMap<(String, String), ObjectImpact> = HashMap::new();

    for entry in &all_entries {
        let mut impacts: Vec<TableImpact> = Vec::new();

        if entry.kind == ObjectKind::TableExtension {
            if let Some(ref ext_target) = entry.extends {
                if ext_target.eq_ignore_ascii_case(table_name) {
                    impacts.push(TableImpact {
                        operation: TableOperationKind::Extends,
                        location_hint: Some(format!("extends {}", ext_target)),
                    });
                }
            }
        }

        if matches!(entry.kind, ObjectKind::Table | ObjectKind::TableExtension) {
            for field in &entry.fields {
                for prop in &field.properties {
                    if prop.name.eq_ignore_ascii_case("TableRelation") {
                        if let Some(table_part) = extract_table_relation_table(&prop.value) {
                            if table_part.eq_ignore_ascii_case(table_name) {
                                impacts.push(TableImpact {
                                    operation: TableOperationKind::Relation,
                                    location_hint: Some(format!("field {}", field.name)),
                                });
                            }
                        }
                    }
                }
            }
        }

        for var in &entry.variables {
            if is_record_of(&var.type_name, table_name) {
                impacts.push(TableImpact {
                    operation: TableOperationKind::RecordVariable,
                    location_hint: Some(format!("var {}", var.name)),
                });
            }
        }

        for method in &entry.methods {
            for param in &method.parameters {
                if is_record_of(&param.type_name, table_name) {
                    impacts.push(TableImpact {
                        operation: TableOperationKind::RecordParameter,
                        location_hint: Some(format!(
                            "{}.{}({})",
                            entry.name, method.name, param.name
                        )),
                    });
                }
            }
        }

        if impacts.is_empty() {
            continue;
        }

        let kind_str = entry.kind.to_string();
        let key = (kind_str.clone(), entry.name.clone());
        let obj_entry = by_object.entry(key).or_insert_with(|| ObjectImpact {
            object_kind: kind_str,
            object_name: entry.name.clone(),
            package: entry.package.clone(),
            impacts: Vec::new(),
        });
        obj_entry.impacts.extend(impacts);
    }

    let mut objects: Vec<ObjectImpact> = by_object.into_values().collect();
    objects.sort_by(|a, b| {
        a.object_name
            .as_bytes()
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.object_name.as_bytes().iter().map(u8::to_ascii_lowercase))
    });

    let total_impacts = objects.iter().map(|o| o.impacts.len()).sum();

    TableImpactResult {
        table_name: canonical_name,
        objects,
        total_impacts,
    }
}

/// Extract the leading table-name component of a `TableRelation` property value.
///
/// AL `TableRelation` values can take several shapes:
/// - `Customer` — bare identifier
/// - `"Customer"` — quoted identifier
/// - `"Sales Header"` — quoted multi-word
/// - `Customer."No."` — table dot field
/// - `"Sales Header"."No."`
/// - `"Item" WHERE("Type" = CONST(Inventory))` — with filter clause
/// - `Customer WHERE(...)`
///
/// Returns `None` if the value is empty after stripping. Pre-allocates no
/// `String` on the happy path; returns a borrowed `&str` of the table-name
/// slice. Used by `table_impact` to detect cross-table relations.
pub fn extract_table_relation_table(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Quoted form: `"Table Name"` — body is everything inside the first
    // matching pair of `"`. Multi-word and embedded-special-char identifiers
    // require quotes in AL.
    if let Some(after_open) = trimmed.strip_prefix('"') {
        let end = after_open.find('"')?;
        let name = &after_open[..end];
        if name.is_empty() {
            return None;
        }
        return Some(name);
    }
    // Bare form: identifier terminates at the first whitespace, dot, or
    // opening paren (start of a `WHERE` / `IF` / `FIELD` clause). Trim any
    // residual single-quote wrapping (AL accepts `'Customer'` rarely).
    let end = trimmed
        .find(|c: char| c.is_whitespace() || c == '.' || c == '(')
        .unwrap_or(trimmed.len());
    let bare = trimmed[..end].trim_matches('\'');
    if bare.is_empty() {
        None
    } else {
        Some(bare)
    }
}

/// Returns true if `type_name` is a Record reference to `table_lower`.
///
/// AL type strings from SymbolReference.json look like:
/// - `Record "Customer"`
/// - `Record Customer`
/// - `Record "Sales Header"`
pub fn is_record_of(type_name: &str, table_name: &str) -> bool {
    if table_name.is_empty() {
        return false;
    }
    let t = type_name.trim();
    let rest = match t.split_once(|c: char| c.is_whitespace()) {
        Some((prefix, rest)) if prefix.eq_ignore_ascii_case("Record") => rest.trim(),
        _ => return false,
    };
    let name = rest.trim_matches('"').trim_matches('\'');
    if name.is_empty() {
        return false;
    }
    name.eq_ignore_ascii_case(table_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{
        AttributeSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, PropertyValue,
        SymbolEntry, SymbolIndex, VariableSymbol,
    };

    fn base_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn is_record_of_quoted() {
        assert!(is_record_of("Record \"Customer\"", "customer"));
        assert!(!is_record_of("Record \"Customer\"", "vendor"));
    }

    #[test]
    fn is_record_of_unquoted() {
        assert!(is_record_of("Record Customer", "customer"));
    }

    #[test]
    fn is_record_of_multi_word() {
        assert!(is_record_of("Record \"Sales Header\"", "sales header"));
        assert!(!is_record_of("Record \"Sales Header\"", "customer"));
    }

    #[test]
    fn is_record_of_not_record() {
        assert!(!is_record_of("Codeunit \"Sales-Post\"", "sales-post"));
        assert!(!is_record_of("Integer", "integer"));
        assert!(!is_record_of("", ""));
    }

    #[test]
    fn is_record_of_empty_table_name_never_matches() {
        // A query with an empty table name must never return true, even if the
        // type string is "Record " (Record followed by a space, no actual name).
        assert!(!is_record_of("Record ", ""));
        assert!(!is_record_of("Record \"\"", ""));
        assert!(!is_record_of("Record Customer", ""));
    }

    #[test]
    fn detects_table_extension() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");
        let mut cust_ext = base_entry(ObjectKind::TableExtension, 50100, "Cust Ext");
        cust_ext.extends = Some("Customer".to_string());

        index.add_entries(&[customer, cust_ext]);

        let result = table_impact(&index, "Customer");
        assert_eq!(result.table_name, "Customer");

        let ext_obj = result
            .objects
            .iter()
            .find(|o| o.object_name == "Cust Ext")
            .expect("Cust Ext should appear in results");
        assert!(ext_obj
            .impacts
            .iter()
            .any(|i| i.operation == TableOperationKind::Extends));
    }

    #[test]
    fn detects_table_relation() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");

        let mut sales_header = base_entry(ObjectKind::Table, 36, "Sales Header");
        sales_header.fields = vec![FieldSymbol {
            id: 2,
            name: "Sell-to Customer No.".to_string(),
            type_name: "Code".to_string(),
            properties: vec![PropertyValue {
                name: "TableRelation".to_string(),
                value: "Customer".to_string(),
            }],
        }];

        index.add_entries(&[customer, sales_header]);

        let result = table_impact(&index, "Customer");

        let sh = result
            .objects
            .iter()
            .find(|o| o.object_name == "Sales Header")
            .expect("Sales Header should appear");
        assert!(sh
            .impacts
            .iter()
            .any(|i| i.operation == TableOperationKind::Relation));
        assert!(sh.impacts.iter().any(|i| i
            .location_hint
            .as_deref()
            .unwrap_or("")
            .contains("Sell-to Customer No.")));
    }

    #[test]
    fn detects_record_variable() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");

        let mut posting_cu = base_entry(ObjectKind::Codeunit, 80, "Sales-Post");
        posting_cu.variables = vec![VariableSymbol {
            name: "Cust".to_string(),
            type_name: "Record \"Customer\"".to_string(),
            is_protected: false,
        }];

        index.add_entries(&[customer, posting_cu]);

        let result = table_impact(&index, "Customer");

        let cu_impact = result
            .objects
            .iter()
            .find(|o| o.object_name == "Sales-Post")
            .expect("Sales-Post should appear");
        assert!(cu_impact
            .impacts
            .iter()
            .any(|i| i.operation == TableOperationKind::RecordVariable));
        assert!(cu_impact.impacts.iter().any(|i| i
            .location_hint
            .as_deref()
            .unwrap_or("")
            .contains("Cust")));
    }

    #[test]
    fn detects_record_parameter() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");

        let mut utility_cu = base_entry(ObjectKind::Codeunit, 50100, "Cust Util");
        utility_cu.methods = vec![MethodSymbol {
            name: "ProcessCustomer".to_string(),
            parameters: vec![ParameterSymbol {
                name: "Cust".to_string(),
                type_name: "Record \"Customer\"".to_string(),
                is_var: true,
            }],
            return_type: None,
            attributes: vec![],
            is_local: false,
        }];

        index.add_entries(&[customer, utility_cu]);

        let result = table_impact(&index, "Customer");

        let cu_impact = result
            .objects
            .iter()
            .find(|o| o.object_name == "Cust Util")
            .expect("Cust Util should appear");
        assert!(cu_impact
            .impacts
            .iter()
            .any(|i| i.operation == TableOperationKind::RecordParameter));
    }

    #[test]
    fn no_impact_when_unrelated() {
        let index = SymbolIndex::new();
        index.add_entries(&[base_entry(ObjectKind::Table, 18, "Customer")]);

        let result = table_impact(&index, "Customer");
        assert!(result.objects.is_empty());
        assert_eq!(result.total_impacts, 0);
    }

    #[test]
    fn canonical_name_from_index() {
        let index = SymbolIndex::new();
        index.add_entries(&[base_entry(ObjectKind::Table, 18, "Customer")]);

        let result = table_impact(&index, "CUSTOMER");
        assert_eq!(result.table_name, "Customer");
    }

    #[test]
    fn total_impacts_counts_all_sites() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");

        let mut cu = base_entry(ObjectKind::Codeunit, 50100, "Multi");
        cu.variables = vec![
            VariableSymbol {
                name: "C1".to_string(),
                type_name: "Record \"Customer\"".to_string(),
                is_protected: false,
            },
            VariableSymbol {
                name: "C2".to_string(),
                type_name: "Record \"Customer\"".to_string(),
                is_protected: false,
            },
        ];

        let mut cust_ext = base_entry(ObjectKind::TableExtension, 50101, "CE");
        cust_ext.extends = Some("Customer".to_string());

        index.add_entries(&[customer, cu, cust_ext]);

        let result = table_impact(&index, "Customer");
        // 2 record vars from Multi + 1 extends from CE
        assert_eq!(result.total_impacts, 3);
    }

    #[test]
    fn extract_table_relation_bare_identifier() {
        assert_eq!(extract_table_relation_table("Customer"), Some("Customer"));
    }

    #[test]
    fn extract_table_relation_quoted_identifier() {
        assert_eq!(
            extract_table_relation_table(r#""Customer""#),
            Some("Customer")
        );
    }

    #[test]
    fn extract_table_relation_quoted_multi_word() {
        assert_eq!(
            extract_table_relation_table(r#""Sales Header""#),
            Some("Sales Header")
        );
    }

    #[test]
    fn extract_table_relation_table_dot_field() {
        assert_eq!(
            extract_table_relation_table(r#""Customer"."No.""#),
            Some("Customer")
        );
    }

    #[test]
    fn extract_table_relation_bare_with_where_clause() {
        // Real AL: `Customer WHERE("Blocked" = CONST(""))`. Prior code did
        // not split on whitespace, so this fell through and never matched.
        assert_eq!(
            extract_table_relation_table(r#"Customer WHERE("Blocked" = CONST(""))"#),
            Some("Customer")
        );
    }

    #[test]
    fn extract_table_relation_quoted_with_where_clause() {
        assert_eq!(
            extract_table_relation_table(r#""Item" WHERE("Type" = CONST(Inventory))"#),
            Some("Item")
        );
    }

    #[test]
    fn extract_table_relation_quoted_multiword_with_filter() {
        assert_eq!(
            extract_table_relation_table(r#""Sales Header" WHERE("Document Type" = CONST(Order))"#),
            Some("Sales Header")
        );
    }

    #[test]
    fn extract_table_relation_rejects_empty() {
        assert_eq!(extract_table_relation_table(""), None);
        assert_eq!(extract_table_relation_table(r#""""#), None);
        assert_eq!(extract_table_relation_table("   "), None);
    }

    #[test]
    fn table_impact_detects_relation_with_where_clause() {
        // Regression: TableRelation = `"Customer" WHERE("Blocked" = CONST(""))`
        // must be detected as an impact on Customer.
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");
        let mut sales_header = base_entry(ObjectKind::Table, 36, "Sales Header");
        sales_header.fields = vec![FieldSymbol {
            id: 2,
            name: "Sell-to Customer No.".to_string(),
            type_name: "Code".to_string(),
            properties: vec![PropertyValue {
                name: "TableRelation".to_string(),
                value: r#""Customer" WHERE("Blocked" = CONST(""))"#.to_string(),
            }],
        }];
        index.add_entries(&[customer, sales_header]);

        let result = table_impact(&index, "Customer");
        let sh = result
            .objects
            .iter()
            .find(|o| o.object_name == "Sales Header")
            .expect("Sales Header must show up as impacted via WHERE-clause relation");
        assert!(
            sh.impacts
                .iter()
                .any(|i| i.operation == TableOperationKind::Relation),
            "Relation impact must be detected even when filter clause is present"
        );
    }

    #[test]
    fn is_record_of_handles_tab_separator() {
        // Prior code split on a literal space only; tab-separated type strings
        // from external symbol JSON would fall through.
        assert!(is_record_of("Record\t\"Customer\"", "customer"));
    }

    #[test]
    fn method_attributes_not_treated_as_record_refs() {
        let index = SymbolIndex::new();

        let customer = base_entry(ObjectKind::Table, 18, "Customer");

        // A codeunit with an EventSubscriber — shouldn't show up as record reference
        let mut cu = base_entry(ObjectKind::Codeunit, 50100, "MySub");
        cu.methods = vec![MethodSymbol {
            name: "HandlePost".to_string(),
            parameters: vec![],
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "EventSubscriber".to_string(),
                arguments: vec![
                    "ObjectType::Codeunit".to_string(),
                    "Codeunit::\"Sales-Post\"".to_string(),
                    "'OnAfterPost'".to_string(),
                ],
            }],
            is_local: false,
        }];

        index.add_entries(&[customer, cu]);

        let result = table_impact(&index, "Customer");
        assert!(result.objects.is_empty());
    }
}
