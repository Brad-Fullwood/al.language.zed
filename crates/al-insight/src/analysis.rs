//! Table impact analysis for the AL insight engine.
//!
//! Given a table name, finds all objects in the workspace that interact with
//! it via: Record variable declarations, Record-typed parameters, TableRelation
//! properties, and Extends relationships.
//!
//! Used by `al impact <TableName>` queries.

use al_syntax::IdentifierText;
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
    /// A page, report, query or XMLport whose `SourceTable` is this table.
    SourceTable,
}

impl std::fmt::Display for TableOperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TableOperationKind::RecordVariable => write!(f, "record_variable"),
            TableOperationKind::RecordParameter => write!(f, "record_parameter"),
            TableOperationKind::Relation => write!(f, "relation"),
            TableOperationKind::Extends => write!(f, "extends"),
            TableOperationKind::SourceTable => write!(f, "source_table"),
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

        // A page listing the table as its SourceTable is the most common way a
        // workspace object consumes a table, and leaving it out reported
        // `totalImpacts: 0` for tables that were plainly in use.
        if matches!(
            entry.kind,
            ObjectKind::Page
                | ObjectKind::PageExtension
                | ObjectKind::Report
                | ObjectKind::ReportExtension
                | ObjectKind::Query
                | ObjectKind::XmlPort
        ) {
            for prop in &entry.properties {
                if prop.name.eq_ignore_ascii_case("SourceTable")
                    && prop
                        .value
                        .unquote_identifier()
                        .eq_ignore_ascii_case(table_name)
                {
                    impacts.push(TableImpact {
                        operation: TableOperationKind::SourceTable,
                        location_hint: Some(format!("SourceTable = {}", prop.value.trim())),
                    });
                }
            }
        }

        if matches!(entry.kind, ObjectKind::Table | ObjectKind::TableExtension) {
            for field in &entry.fields {
                for prop in &field.properties {
                    if prop.name.eq_ignore_ascii_case("TableRelation") {
                        // The conditional form names one table per branch;
                        // every branch is a relation to that table.
                        if extract_table_relation_tables(&prop.value)
                            .iter()
                            .any(|table_part| table_part.eq_ignore_ascii_case(table_name))
                        {
                            impacts.push(TableImpact {
                                operation: TableOperationKind::Relation,
                                location_hint: Some(format!("field {}", field.name)),
                            });
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

/// Add the workspace's procedure-local `Record <table>` variables to a
/// [`table_impact`] result.
///
/// Symbol entries carry an object's global variables and its procedures'
/// parameters, but not the variables declared inside a procedure, which is
/// where most code holds a record: a test codeunit whose only use of
/// Customer was two `Cust: Record Customer` locals did not appear at all.
pub fn add_workspace_local_record_variables(
    result: &mut TableImpactResult,
    files: &al_source::file_index::FileIndex,
    table_name: &str,
) {
    let mut paths: Vec<std::path::PathBuf> = files
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    paths.sort();
    for path in paths {
        let Some((text, tree)) = files.get_cached_parse(&path) else {
            continue;
        };
        let objects = files
            .object_infos
            .get(&path)
            .map(|infos| infos.value().clone())
            .unwrap_or_default();
        let source = text.as_bytes();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
            if node.kind() != "regular_variable_declaration" {
                continue;
            }
            let Some(procedure) = std::iter::successors(node.parent(), |n| n.parent()).find(|n| {
                matches!(
                    n.kind(),
                    "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
                )
            }) else {
                continue; // an object-level global: already in the entry
            };
            let is_record = node
                .child_by_field_name("type")
                .and_then(|ty| ty.utf8_text(source).ok())
                .is_some_and(|ty| is_record_of(ty, table_name));
            if !is_record {
                continue;
            }
            let Some(object) = objects.iter().find(|object| {
                object.range.start_byte <= node.start_byte()
                    && node.end_byte() <= object.range.end_byte
            }) else {
                continue;
            };
            let procedure_name = procedure
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
                .map(|name| name.unquote_identifier().into_owned())
                .unwrap_or_default();
            let mut cursor = node.walk();
            let names: Vec<String> = node
                .children_by_field_name("name", &mut cursor)
                .filter_map(|name| name.utf8_text(source).ok())
                .map(|name| name.unquote_identifier().into_owned())
                .collect();
            let kind = object
                .kind
                .parse::<ObjectKind>()
                .map(|kind| kind.to_string())
                .unwrap_or_else(|_| object.kind.clone());
            let index = match result.objects.iter().position(|existing| {
                existing.object_kind == kind
                    && existing.object_name.eq_ignore_ascii_case(&object.name)
            }) {
                Some(index) => index,
                None => {
                    result.objects.push(ObjectImpact {
                        object_kind: kind.clone(),
                        object_name: object.name.clone(),
                        package: "workspace".to_string(),
                        impacts: Vec::new(),
                    });
                    result.objects.len() - 1
                }
            };
            for name in names {
                result.objects[index].impacts.push(TableImpact {
                    operation: TableOperationKind::RecordVariable,
                    location_hint: Some(format!("var {name} in {procedure_name}")),
                });
                result.total_impacts += 1;
            }
        }
    }
    result.objects.sort_by(|a, b| {
        a.object_name
            .as_bytes()
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.object_name.as_bytes().iter().map(u8::to_ascii_lowercase))
    });
}

/// Every table referenced by a `TableRelation` value, in declaration order.
///
/// AL's conditional form names one table per branch:
///
/// ```text
/// TableRelation = IF (Type = CONST(Item)) Item."No."
///                 ELSE IF (Type = CONST(Resource)) Resource."No."
///                 ELSE "G/L Account";
/// ```
///
/// The single-table helper used to feed the bare-identifier path the whole
/// value, so it returned the token `if` and *both* branch tables were missed by
/// table-impact and `RelatesTo` edges.
pub fn extract_table_relation_tables(value: &str) -> Vec<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if find_keyword(trimmed, "if") != Some(0) {
        return take_table_name(trimmed).0.into_iter().collect();
    }

    let mut tables = Vec::new();
    let mut rest = trimmed;
    // Each iteration consumes one `[IF (cond)] <Table>` branch. The bound is
    // the number of branches; a malformed value breaks out early.
    loop {
        rest = rest.trim_start();
        if find_keyword(rest, "if") == Some(0) {
            rest = rest["if".len()..].trim_start();
            let Some(close) = matching_paren(rest) else {
                break;
            };
            rest = &rest[close + 1..];
        }
        let (name, remainder) = take_table_name(rest);
        if let Some(name) = name {
            tables.push(name);
        }
        match find_keyword(remainder, "else") {
            Some(position) => rest = &remainder[position + "else".len()..],
            None => break,
        }
    }

    // Declaration order, deduplicated in place: the first branch is the
    // primary relation and callers taking the first element rely on it. Sorting
    // here used to make that the alphabetically first branch instead.
    let mut seen = std::collections::HashSet::new();
    tables.retain(|table| seen.insert(table.to_ascii_lowercase()));
    tables
}

/// Byte offset of the first whole-word, case-insensitive occurrence of `keyword`.
fn find_keyword(text: &str, keyword: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    debug_assert_eq!(lower.len(), text.len());
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find(keyword) {
        let position = from + relative;
        let prev_ok = position == 0
            || !text[..position]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = position + keyword.len();
        let next_ok = !text[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if prev_ok && next_ok {
            return Some(position);
        }
        from = after;
    }
    None
}

/// Byte offset of the `)` matching the `(` that `text` starts with.
fn matching_paren(text: &str) -> Option<usize> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a leading table reference off `text`, returning `(name, remainder)`.
///
/// Quoted form `"Table Name"` takes everything inside the first matching pair
/// of `"`. Bare form terminates at the first whitespace, dot, or opening paren
/// (start of a `WHERE` / `FIELD` clause). Residual single-quote wrapping is
/// trimmed (AL accepts `'Customer'` rarely).
fn take_table_name(text: &str) -> (Option<&str>, &str) {
    let text = text.trim_start();
    if let Some(after_open) = text.strip_prefix('"') {
        let Some(end) = after_open.find('"') else {
            return (None, "");
        };
        let name = &after_open[..end];
        return (
            if name.is_empty() { None } else { Some(name) },
            &after_open[end + 1..],
        );
    }
    let end = text
        .find(|c: char| c.is_whitespace() || c == '.' || c == '(')
        .unwrap_or(text.len());
    let bare = text[..end].trim_matches('\'');
    (
        if bare.is_empty() { None } else { Some(bare) },
        &text[end..],
    )
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

    /// A codeunit whose only use of a table is procedure-local variables.
    #[test]
    fn local_record_variables_count_as_table_uses() {
        let files = al_source::file_index::FileIndex::new();
        files.add_file(
            std::path::PathBuf::from("/ws/LoyaltyTest.Codeunit.al"),
            "codeunit 50103 \"Loyalty Test\"\n{\n    var\n        Global: Record Item;\n\n    procedure TierCanBeCleared()\n    var\n        Cust, Other: Record Customer;\n        Count: Integer;\n    begin\n    end;\n}\n"
                .to_string(),
        );
        let mut result = table_impact(&SymbolIndex::new(), "Customer");

        add_workspace_local_record_variables(&mut result, &files, "Customer");

        assert_eq!(result.objects.len(), 1, "{result:#?}");
        assert_eq!(result.objects[0].object_name, "Loyalty Test");
        let hints: Vec<_> = result.objects[0]
            .impacts
            .iter()
            .filter_map(|impact| impact.location_hint.clone())
            .collect();
        assert_eq!(
            hints,
            vec![
                "var Cust in TierCanBeCleared",
                "var Other in TierCanBeCleared"
            ]
        );
        assert_eq!(result.total_impacts, 2);
    }
    use al_symbols::{
        AttributeSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, PropertyValue,
        SymbolEntry, SymbolIndex, VariableSymbol,
    };

    fn base_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            package: "TestPkg".to_string(),
            ..Default::default()
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
    fn detects_a_page_source_table() {
        let index = SymbolIndex::new();

        let staging = base_entry(ObjectKind::Table, 50130, "Work Order Staging");
        let mut card = base_entry(ObjectKind::Page, 50130, "Work Order Card");
        card.properties = vec![al_symbols::PropertyValue {
            name: "SourceTable".to_string(),
            value: "\"Work Order Staging\"".to_string(),
        }];

        index.add_entries(&[staging, card]);

        let result = table_impact(&index, "Work Order Staging");
        assert_eq!(
            result.total_impacts, 1,
            "a page using the table as SourceTable is an impact: {result:?}"
        );
        let page = result
            .objects
            .iter()
            .find(|o| o.object_name == "Work Order Card")
            .expect("the page must appear in the result");
        assert!(page
            .impacts
            .iter()
            .any(|i| i.operation == TableOperationKind::SourceTable));
    }

    #[test]
    fn a_page_on_another_table_is_not_an_impact() {
        let index = SymbolIndex::new();
        let mut card = base_entry(ObjectKind::Page, 50131, "Item Card");
        card.properties = vec![al_symbols::PropertyValue {
            name: "SourceTable".to_string(),
            value: "Item".to_string(),
        }];
        index.add_entries(&[
            base_entry(ObjectKind::Table, 50130, "Work Order Staging"),
            card,
        ]);

        assert_eq!(table_impact(&index, "Work Order Staging").total_impacts, 0);
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
        assert_eq!(extract_table_relation_tables("Customer"), vec!["Customer"]);
    }

    #[test]
    fn extract_table_relation_quoted_identifier() {
        assert_eq!(
            extract_table_relation_tables(r#""Customer""#),
            vec!["Customer"]
        );
    }

    #[test]
    fn extract_table_relation_quoted_multi_word() {
        assert_eq!(
            extract_table_relation_tables(r#""Sales Header""#),
            vec!["Sales Header"]
        );
    }

    #[test]
    fn extract_table_relation_table_dot_field() {
        assert_eq!(
            extract_table_relation_tables(r#""Customer"."No.""#),
            vec!["Customer"]
        );
    }

    #[test]
    fn extract_table_relation_bare_with_where_clause() {
        // Real AL: `Customer WHERE("Blocked" = CONST(""))`. Prior code did
        // not split on whitespace, so this fell through and never matched.
        assert_eq!(
            extract_table_relation_tables(r#"Customer WHERE("Blocked" = CONST(""))"#),
            vec!["Customer"]
        );
    }

    #[test]
    fn extract_table_relation_quoted_with_where_clause() {
        assert_eq!(
            extract_table_relation_tables(r#""Item" WHERE("Type" = CONST(Inventory))"#),
            vec!["Item"]
        );
    }

    #[test]
    fn extract_table_relation_quoted_multiword_with_filter() {
        assert_eq!(
            extract_table_relation_tables(
                r#""Sales Header" WHERE("Document Type" = CONST(Order))"#
            ),
            vec!["Sales Header"]
        );
    }

    #[test]
    fn extract_table_relation_rejects_empty() {
        assert_eq!(extract_table_relation_tables(""), Vec::<&str>::new());
        assert_eq!(extract_table_relation_tables(r#""""#), Vec::<&str>::new());
        assert_eq!(extract_table_relation_tables("   "), Vec::<&str>::new());
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

    /// AL's conditional form names one table per branch; the bare-identifier
    /// path used to return the token `if` and miss both branch tables.
    #[test]
    fn extract_table_relation_tables_handles_the_conditional_form() {
        let value =
            r#"IF (Type=CONST(Item)) Item."No." ELSE IF (Type=CONST(Resource)) Resource."No.""#;
        assert_eq!(
            extract_table_relation_tables(value),
            vec!["Item", "Resource"]
        );
    }

    #[test]
    fn extract_table_relation_tables_handles_a_trailing_else_branch() {
        let value = r#"IF (Type=CONST(Item)) Item ELSE "G/L Account""#;
        assert_eq!(
            extract_table_relation_tables(value),
            vec!["Item", "G/L Account"]
        );
    }

    #[test]
    fn extract_table_relation_tables_handles_conditional_branches_with_where() {
        let value = r#"IF (Type=CONST(Item)) Item WHERE("Blocked"=CONST(false)) ELSE Resource"#;
        assert_eq!(
            extract_table_relation_tables(value),
            vec!["Item", "Resource"]
        );
    }

    /// The doc promises declaration order, so the first element is the primary
    /// relation. Sorting made that wrong: `Apple` came back first for a relation
    /// whose first branch is `Zebra`.
    #[test]
    fn extract_table_relation_tables_keeps_declaration_order() {
        let value = r#"IF (Type=CONST(Zebra)) Zebra."No." ELSE IF (Type=CONST(Apple)) Apple."No.""#;
        assert_eq!(
            extract_table_relation_tables(value),
            vec!["Zebra", "Apple"],
            "branches must come back in the order they are declared"
        );
    }

    #[test]
    fn extract_table_relation_tables_drops_a_repeated_branch_table() {
        let value = r#"IF (Type=CONST(A)) Item ELSE IF (Type=CONST(B)) Item ELSE Resource"#;
        assert_eq!(
            extract_table_relation_tables(value),
            vec!["Item", "Resource"]
        );
    }

    #[test]
    fn extract_table_relation_tables_keeps_the_simple_form_intact() {
        assert_eq!(extract_table_relation_tables("Customer"), vec!["Customer"]);
        assert_eq!(
            extract_table_relation_tables(r#""Sales Header" WHERE("Document Type"=CONST(Order))"#),
            vec!["Sales Header"]
        );
        assert!(extract_table_relation_tables("").is_empty());
    }
}
