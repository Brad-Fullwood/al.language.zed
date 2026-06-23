//! Dependency impact analysis query.
//!
//! "If I change this symbol, what breaks?" Searches the symbol index and workspace
//! files for all consumers of a named symbol (table field, procedure, object, etc.).

use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolEntry};
use serde::Serialize;

use al_workspace::Workspace;

/// How a consumer references the symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImpactType {
    /// Field displayed on a page.
    Display,
    /// Field/procedure read in code.
    Read,
    /// Field used as a filter or key.
    Filter,
    /// Procedure called from another object.
    Call,
    /// Object extended by an extension object.
    Extends,
    /// Event subscribed to.
    Subscribe,
}

/// A single consumer of the queried symbol.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactEntry {
    #[serde(rename = "k")]
    pub kind: ObjectKind,
    pub id: i32,
    #[serde(rename = "n")]
    pub name: String,
    /// Which procedure references the symbol (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proc: Option<String>,
    /// Which field references the symbol (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// How the symbol is consumed.
    #[serde(rename = "type")]
    pub impact_type: ImpactType,
    /// Package the consumer belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

/// Find all consumers of a symbol across the symbol index and workspace files.
///
/// The `symbol` argument can be:
/// - An object name: `"Customer"` — finds extensions, page source tables, etc.
/// - A qualified field: `"Customer.\"Credit Limit\""` or `Customer.Credit Limit`
/// - A qualified procedure: `"Sales-Post.PostDocument"`
///
/// Returns a list of impact entries describing how each consumer references the symbol.
///
/// Uses targeted index lookups (`get_extensions_of`, `get_by_kind`) for the
/// extends/source-table checks. The member-scoped checks (event subscriber,
/// parameter types, table relations) still require iterating all entries
/// because no reverse-index exists from member-name to consumer; that scan is
/// gated behind `member_part.is_some()` to avoid running it for object-only
/// queries.
#[must_use]
pub fn impact(workspace: &Workspace, symbol: &str) -> Vec<ImpactEntry> {
    let (object_part, member_part) = parse_symbol(symbol);
    let mut results = Vec::new();

    // Bounded: only extensions of the named object via the by_extends index.
    for entry in workspace.symbols.get_extensions_of(&object_part) {
        check_extends(&entry, &object_part, &mut results);
    }

    // Bounded: only kinds that can carry a SourceTable property.
    const SOURCE_TABLE_KINDS: &[ObjectKind] = &[
        ObjectKind::Page,
        ObjectKind::PageExtension,
        ObjectKind::Report,
        ObjectKind::ReportExtension,
        ObjectKind::Query,
    ];
    for kind in SOURCE_TABLE_KINDS {
        for entry in workspace.symbols.get_by_kind(*kind) {
            check_source_table(&entry, &object_part, &mut results);
        }
    }

    // Member-scoped scan (full pass — no reverse index available).
    if let Some(member) = &member_part {
        for entry in workspace.symbols.all_entries() {
            check_member_consumers(&entry, &object_part, member, &mut results);
        }
    } else {
        // Object-scoped scan: parameter-type and TableRelation references to the
        // base object. These do not depend on a member, so they belong in the
        // object-only query (a member query like `Customer.OnBeforePost` must not
        // report every method that merely takes a `Record Customer` parameter).
        for entry in workspace.symbols.all_entries() {
            check_object_consumers(&entry, &object_part, &mut results);
        }
    }

    if let Some(member) = &member_part {
        search_workspace_files(workspace, member, &mut results);
    } else {
        search_workspace_files(workspace, &object_part, &mut results);
    }

    results
}

/// Parse a symbol specifier into (object_name, optional_member_name).
///
/// Examples:
/// - `"Customer"` → `("Customer", None)`
/// - `"Customer.\"Credit Limit\""` → `("Customer", Some("Credit Limit"))`
/// - `"Sales-Post.PostDocument"` → `("Sales-Post", Some("PostDocument"))`
fn parse_symbol(symbol: &str) -> (String, Option<String>) {
    if let Some(dot_pos) = find_unquoted_dot(symbol) {
        let object = symbol[..dot_pos].trim().trim_matches('"').to_string();
        let member = symbol[dot_pos + 1..].trim().trim_matches('"').to_string();
        (object, Some(member))
    } else {
        (symbol.trim().trim_matches('"').to_string(), None)
    }
}

fn find_unquoted_dot(s: &str) -> Option<usize> {
    let mut in_quotes = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

/// Push an Extends impact for an entry already known to extend `target_object`.
///
/// `entry` comes from `get_extensions_of(target_object)`, so the by_extends index
/// has already done a case-insensitive name match. The double-check below is
/// retained as a defence against stale index state.
fn check_extends(entry: &Arc<SymbolEntry>, target_object: &str, results: &mut Vec<ImpactEntry>) {
    let target_lower = target_object.to_lowercase();
    if let Some(ref extends_name) = entry.extends {
        if extends_name.to_lowercase() == target_lower {
            results.push(ImpactEntry {
                kind: entry.kind,
                id: entry.id,
                name: entry.name.clone(),
                proc: None,
                field: None,
                impact_type: ImpactType::Extends,
                package: Some(entry.package.clone()),
            });
        }
    }
}

/// Check a Page/Report/Query entry for a SourceTable property pointing at `target_object`.
fn check_source_table(
    entry: &Arc<SymbolEntry>,
    target_object: &str,
    results: &mut Vec<ImpactEntry>,
) {
    let target_lower = target_object.to_lowercase();
    for prop in &entry.properties {
        if prop.name.eq_ignore_ascii_case("SourceTable")
            && prop
                .value
                .trim_matches('"')
                .eq_ignore_ascii_case(&target_lower)
        {
            results.push(ImpactEntry {
                kind: entry.kind,
                id: entry.id,
                name: entry.name.clone(),
                proc: None,
                field: None,
                impact_type: ImpactType::Display,
                package: Some(entry.package.clone()),
            });
        }
    }
}

/// Member-scoped check: event subscribers targeting `object::member`.
///
/// Only EventSubscriber attributes are member-specific. Parameter-type and
/// TableRelation references depend on the object alone and are handled by
/// [`check_object_consumers`] in the object-only query; reporting them here
/// would falsely flag every method taking a `Record <object>` parameter for a
/// member query such as `Customer.OnBeforePost`.
fn check_member_consumers(
    entry: &Arc<SymbolEntry>,
    target_object: &str,
    member: &str,
    results: &mut Vec<ImpactEntry>,
) {
    let target_lower = target_object.to_lowercase();
    let member_lower = member.to_lowercase();
    for method in &entry.methods {
        // The attribute carries (event-element-name, target-object, target-event)
        // so it must have at least 3 arguments; otherwise it cannot identify a
        // concrete event and is skipped.
        for attr in &method.attributes {
            if attr.name != "EventSubscriber" || attr.arguments.len() < 3 {
                continue;
            }
            let target_obj_arg = attr.arguments.get(1).map(|s| {
                let s = s.trim();
                if let Some(pos) = s.find("::") {
                    s[pos + 2..]
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_lowercase()
                } else {
                    s.trim_matches('"').trim_matches('\'').to_lowercase()
                }
            });
            let target_event_arg = attr
                .arguments
                .get(2)
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_lowercase());

            if target_obj_arg.as_deref() == Some(&target_lower)
                && target_event_arg.as_deref() == Some(&member_lower)
            {
                results.push(ImpactEntry {
                    kind: entry.kind,
                    id: entry.id,
                    name: entry.name.clone(),
                    proc: Some(method.name.clone()),
                    field: None,
                    impact_type: ImpactType::Subscribe,
                    package: Some(entry.package.clone()),
                });
            }
        }
    }
}

/// Object-scoped checks: parameter types and field TableRelations referencing
/// `target_object`.
///
/// Uses the precise `Record`/TableRelation parsers from `insight::analysis`
/// rather than substring matching, so querying `Customer` does not falsely
/// match `Record "CustomerBank"` or `TableRelation = "CustomerVendor"`.
fn check_object_consumers(
    entry: &Arc<SymbolEntry>,
    target_object: &str,
    results: &mut Vec<ImpactEntry>,
) {
    use al_insight::analysis::{extract_table_relation_table, is_record_of};

    for method in &entry.methods {
        for param in &method.parameters {
            if is_record_of(&param.type_name, target_object) {
                results.push(ImpactEntry {
                    kind: entry.kind,
                    id: entry.id,
                    name: entry.name.clone(),
                    proc: Some(method.name.clone()),
                    field: None,
                    impact_type: ImpactType::Read,
                    package: Some(entry.package.clone()),
                });
                break;
            }
        }
    }

    // Object-scope (global) Record variables referencing the target. The
    // parameter scan above only sees procedure signatures; a base-app codeunit
    // commonly holds `Cust: Record Customer` as a global, which the generic
    // impact analysis previously missed entirely (only the orphaned
    // insight::analysis::table_impact caught it — now merged here so `al impact`
    // covers global-variable consumers too).
    for var in &entry.variables {
        if is_record_of(&var.type_name, target_object) {
            results.push(ImpactEntry {
                kind: entry.kind,
                id: entry.id,
                name: entry.name.clone(),
                proc: None,
                field: None,
                impact_type: ImpactType::Read,
                package: Some(entry.package.clone()),
            });
            break;
        }
    }

    for field in &entry.fields {
        for prop in &field.properties {
            if prop.name.eq_ignore_ascii_case("TableRelation")
                && extract_table_relation_table(&prop.value)
                    .is_some_and(|t| t.eq_ignore_ascii_case(target_object))
            {
                results.push(ImpactEntry {
                    kind: entry.kind,
                    id: entry.id,
                    name: entry.name.clone(),
                    proc: None,
                    field: Some(field.name.clone()),
                    impact_type: ImpactType::Filter,
                    package: Some(entry.package.clone()),
                });
            }
        }
    }
}

fn search_workspace_files(
    workspace: &Workspace,
    search_name: &str,
    results: &mut Vec<ImpactEntry>,
) {
    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        drop(entry); // release dashmap lock before re-accessing the map
        let Some((file_text, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };
        let refs = al_syntax::find_variable_references(&tree, &file_text, search_name);

        if !refs.is_empty() {
            if let Some(obj_info) = al_syntax::find_object_declaration(&tree, &file_text) {
                let kind = obj_info
                    .kind
                    .parse::<ObjectKind>()
                    .unwrap_or(ObjectKind::Codeunit);
                let id = obj_info.id.unwrap_or(0) as i32;

                results.push(ImpactEntry {
                    kind,
                    id,
                    name: obj_info.name,
                    proc: None,
                    field: None,
                    impact_type: ImpactType::Read,
                    package: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with_files(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    fn make_table(id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_page_for_table(id: i32, name: &str, source_table: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Page,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: vec![PropertyValue {
                name: "SourceTable".to_string(),
                value: source_table.to_string(),
            }],
            variables: Vec::new(),
        }
    }

    fn make_table_ext(id: i32, name: &str, extends: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::TableExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "ExtPkg".to_string(),
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
    fn impact_finds_extensions() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_table(18, "Customer"),
            make_table_ext(50100, "Cust Ext", "Customer"),
        ]);

        let results = impact(&ws, "Customer");

        assert!(
            results
                .iter()
                .any(|r| r.name == "Cust Ext" && r.impact_type == ImpactType::Extends),
            "Expected extension to be found. Got: {:?}",
            results
        );
    }

    #[test]
    fn impact_finds_page_source_table() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_table(18, "Customer"),
            make_page_for_table(21, "Customer Card", "Customer"),
        ]);

        let results = impact(&ws, "Customer");

        assert!(
            results
                .iter()
                .any(|r| r.name == "Customer Card" && r.impact_type == ImpactType::Display),
            "Expected page with SourceTable=Customer to be found. Got: {:?}",
            results
        );
    }

    #[test]
    fn impact_finds_workspace_references() {
        let ws = workspace_with_files(vec![(
            "/src/MyCodeunit.al",
            r#"codeunit 50100 "My Codeunit"
{
    procedure DoWork()
    var
        cust: Record Customer;
    begin
        cust.Get('10000');
    end;
}"#,
        )]);

        let results = impact(&ws, "Customer");

        assert!(
            results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Expected workspace file referencing Customer. Got: {:?}",
            results
        );
    }

    #[test]
    fn impact_empty_for_unknown_symbol() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_table(18, "Customer")]);

        let results = impact(&ws, "NonexistentObject");
        assert!(results.is_empty(), "Expected no results for unknown symbol");
    }

    #[test]
    fn impact_parses_qualified_symbol() {
        let (obj, member) = parse_symbol("Customer.\"Credit Limit\"");
        assert_eq!(obj, "Customer");
        assert_eq!(member.as_deref(), Some("Credit Limit"));
    }

    #[test]
    fn impact_parses_unqualified_symbol() {
        let (obj, member) = parse_symbol("Customer");
        assert_eq!(obj, "Customer");
        assert!(member.is_none());
    }

    /// Regression: impact() must not call `search("", usize::MAX)` — extensions
    /// of the target object must come from the by_extends index, so unrelated
    /// objects in the index (here: an unrelated `Item` extension) do not
    /// contaminate results.
    #[test]
    fn impact_extends_uses_targeted_lookup() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_table(18, "Customer"),
            make_table_ext(50100, "Cust Ext", "Customer"),
            make_table(27, "Item"),
            make_table_ext(50101, "Item Ext", "Item"),
        ]);

        let results = impact(&ws, "Customer");

        let extends_results: Vec<_> = results
            .iter()
            .filter(|r| r.impact_type == ImpactType::Extends)
            .collect();
        assert_eq!(
            extends_results.len(),
            1,
            "Targeted lookup must only return extensions of Customer, not Item. Got: {:?}",
            extends_results
        );
        assert_eq!(extends_results[0].name, "Cust Ext");
    }

    #[test]
    fn impact_finds_table_relation() {
        let ws = Workspace::new();
        let mut sales_header = make_table(36, "Sales Header");
        sales_header.fields = vec![FieldSymbol {
            id: 2,
            name: "Sell-to Customer No.".to_string(),
            type_name: "Code".to_string(),
            properties: vec![PropertyValue {
                name: "TableRelation".to_string(),
                value: "Customer".to_string(),
            }],
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), sales_header]);

        let results = impact(&ws, "Customer");

        assert!(
            results.iter().any(|r| r.name == "Sales Header"
                && r.field.as_deref() == Some("Sell-to Customer No.")
                && r.impact_type == ImpactType::Filter),
            "Expected TableRelation to Customer to be found. Got: {:?}",
            results
        );
    }

    /// Regression: TableRelation matching must not be a substring match — a
    /// relation to `CustomerBank` must NOT be reported as an impact on `Customer`.
    #[test]
    fn impact_table_relation_no_substring_false_positive() {
        let ws = Workspace::new();
        let mut sales_header = make_table(36, "Sales Header");
        sales_header.fields = vec![FieldSymbol {
            id: 2,
            name: "Bank No.".to_string(),
            type_name: "Code".to_string(),
            properties: vec![PropertyValue {
                name: "TableRelation".to_string(),
                value: "CustomerBank".to_string(),
            }],
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), sales_header]);

        let results = impact(&ws, "Customer");

        assert!(
            !results.iter().any(|r| r.impact_type == ImpactType::Filter),
            "TableRelation to CustomerBank must not match Customer. Got: {:?}",
            results
        );
    }

    /// Regression: parameter-type matching must not be a substring match — a
    /// `Record "CustomerBank"` parameter must NOT be reported as using `Customer`.
    #[test]
    fn impact_param_type_no_substring_false_positive() {
        let ws = Workspace::new();
        let mut codeunit = make_table(50100, "My Codeunit");
        codeunit.kind = ObjectKind::Codeunit;
        codeunit.fields = Vec::new();
        codeunit.methods = vec![MethodSymbol {
            name: "DoWork".to_string(),
            parameters: vec![ParameterSymbol {
                name: "rec".to_string(),
                type_name: "Record \"CustomerBank\"".to_string(),
                is_var: false,
            }],
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), codeunit]);

        let results = impact(&ws, "Customer");

        assert!(
            !results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Record CustomerBank parameter must not match Customer. Got: {:?}",
            results
        );
    }

    /// A global (object-scope) `Record Customer` variable IS an impact — merged
    /// from the orphaned table_impact, which the generic impact previously missed
    /// (it only scanned method parameters). Red-green: drop the entry.variables
    /// loop in check_object_consumers and this fails.
    #[test]
    fn impact_global_record_variable_is_found() {
        let ws = Workspace::new();
        let mut codeunit = make_table(50100, "My Codeunit");
        codeunit.kind = ObjectKind::Codeunit;
        codeunit.fields = Vec::new();
        codeunit.variables = vec![VariableSymbol {
            name: "Cust".to_string(),
            type_name: "Record Customer".to_string(),
            is_protected: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), codeunit]);

        let results = impact(&ws, "Customer");

        assert!(
            results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "global Record Customer variable must be reported as an impact. Got: {:?}",
            results
        );
    }

    /// Substring guard for the variable path: a `Record "CustomerBank"` global
    /// must NOT match `Customer`.
    #[test]
    fn impact_global_record_variable_no_substring_false_positive() {
        let ws = Workspace::new();
        let mut codeunit = make_table(50100, "My Codeunit");
        codeunit.kind = ObjectKind::Codeunit;
        codeunit.fields = Vec::new();
        codeunit.variables = vec![VariableSymbol {
            name: "Cust".to_string(),
            type_name: "Record \"CustomerBank\"".to_string(),
            is_protected: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), codeunit]);

        let results = impact(&ws, "Customer");

        assert!(
            !results.iter().any(|r| r.name == "My Codeunit"),
            "Record CustomerBank global must not match Customer. Got: {:?}",
            results
        );
    }

    /// A `Record Customer` parameter IS a real (object-scoped) impact.
    #[test]
    fn impact_param_type_exact_match_found() {
        let ws = Workspace::new();
        let mut codeunit = make_table(50100, "My Codeunit");
        codeunit.kind = ObjectKind::Codeunit;
        codeunit.fields = Vec::new();
        codeunit.methods = vec![MethodSymbol {
            name: "DoWork".to_string(),
            parameters: vec![ParameterSymbol {
                name: "rec".to_string(),
                type_name: "Record Customer".to_string(),
                is_var: false,
            }],
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), codeunit]);

        let results = impact(&ws, "Customer");

        assert!(
            results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Record Customer parameter should be a Read impact. Got: {:?}",
            results
        );
    }

    /// Positive coverage: a member-scoped query finds matching EventSubscribers.
    #[test]
    fn impact_finds_event_subscriber() {
        let ws = Workspace::new();
        let mut subscriber = make_table(50100, "Cust Subscriber");
        subscriber.kind = ObjectKind::Codeunit;
        subscriber.fields = Vec::new();
        subscriber.methods = vec![MethodSymbol {
            name: "OnBeforePostHandler".to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: vec![AttributeSymbol {
                name: "EventSubscriber".to_string(),
                arguments: vec![
                    "ObjectType::Table".to_string(),
                    "Database::Customer".to_string(),
                    "OnBeforePost".to_string(),
                ],
            }],
            is_local: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), subscriber]);

        let results = impact(&ws, "Customer.OnBeforePost");

        assert!(
            results
                .iter()
                .any(|r| r.name == "Cust Subscriber" && r.impact_type == ImpactType::Subscribe),
            "EventSubscriber to Customer::OnBeforePost should be found. Got: {:?}",
            results
        );
    }

    /// Regression: a member query (`Customer.OnBeforePost`) must NOT report a
    /// method merely because it takes a `Record Customer` parameter — only
    /// actual subscribers to that event count.
    #[test]
    fn impact_member_query_ignores_unrelated_param_methods() {
        let ws = Workspace::new();
        let mut codeunit = make_table(50100, "My Codeunit");
        codeunit.kind = ObjectKind::Codeunit;
        codeunit.fields = Vec::new();
        codeunit.methods = vec![MethodSymbol {
            name: "Unrelated".to_string(),
            parameters: vec![ParameterSymbol {
                name: "rec".to_string(),
                type_name: "Record Customer".to_string(),
                is_var: false,
            }],
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }];
        ws.symbols
            .add_entries(&[make_table(18, "Customer"), codeunit]);

        let results = impact(&ws, "Customer.OnBeforePost");

        assert!(
            !results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Member query must not report unrelated Record Customer params. Got: {:?}",
            results
        );
    }
}
