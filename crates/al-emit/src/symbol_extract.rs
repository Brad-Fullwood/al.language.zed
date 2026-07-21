//! Extract `SymbolReference.json` objects from AL parse trees.

use tree_sitter::{Node, Tree};

use al_symbols::model::{
    AttributeSymbol, EnumValueSymbol, FieldSymbol, KeySymbol, MethodSymbol, ObjectKind,
    ParameterSymbol, PropertyValue, SymbolEntry, VariableSymbol,
};
use al_syntax::language_data::is_type_keyword_node;
use al_syntax::AlParser;

/// Whether a tree-sitter node kind denotes an AL type — data-driven via
/// `language_data` (the generated `token_classification`), plus the
/// `control_keyword` node that table field declarations use for their type.
fn is_type_node(kind: &str) -> bool {
    is_type_keyword_node(kind) || kind == "control_keyword"
}

/// A query `dataitem` (element) and its columns/nested dataitems.
#[derive(Debug, Clone, Default)]
pub struct QueryElement {
    pub name: String,
    /// The dataitem's source table (`RelatedTable`).
    pub related_table: Option<String>,
    /// `(column name, source column expression)`.
    pub columns: Vec<(String, String)>,
    pub children: Vec<QueryElement>,
}

/// One page control or action node (area / group / field / action / …).
#[derive(Debug, Clone, Default)]
pub struct PageControl {
    /// The AL keyword (`area`, `group`, `field`, `part`, `action`, …).
    pub keyword: String,
    pub name: String,
    /// For fields: the source expression (e.g. `Rec."No."`).
    pub source_expr: Option<String>,
    pub properties: Vec<PropertyValue>,
    pub children: Vec<PageControl>,
}

/// A page/report extension change operation (`addlast(Anchor) { … }`).
#[derive(Debug, Clone, Default)]
pub struct ControlChange {
    /// The change keyword (`add`, `addfirst`, `addlast`, `modify`, …).
    pub kind: String,
    pub anchor: String,
    /// Added controls (for add operations).
    pub controls: Vec<PageControl>,
}

/// A report-extension dataset change operation (`add(Item) { column(...) }`).
#[derive(Debug, Clone, Default)]
pub struct DatasetChange {
    /// The change keyword (`add`, `modify`, …).
    pub kind: String,
    /// The anchor dataitem/column it targets.
    pub anchor: String,
    /// Added columns as `(name, source field)`.
    pub columns: Vec<(String, String)>,
    /// Added dataitems (recursive).
    pub dataitems: Vec<QueryElement>,
}

/// A report rendering layout (`layout(MyLayout) { Type = RDLC; LayoutFile = '…'; }`).
#[derive(Debug, Clone, Default)]
pub struct ReportLayout {
    pub name: String,
    /// The layout's declared properties (`Type`, `LayoutFile`, `Caption`, …).
    pub properties: Vec<PropertyValue>,
}

/// One permission clause in a permissionset (`tabledata "X" = RIMD`).
#[derive(Debug, Clone)]
pub struct PermissionDecl {
    /// `tabledata` / `table` / `codeunit` / `page` / …
    pub object_type: String,
    pub object_name: String,
    /// `RIMD` / `X` / `RIMDX` / …
    pub permission: String,
}

/// An extracted object plus the source file it came from (alc records this as
/// `ReferenceSourceFileName`).
#[derive(Debug, Clone)]
pub struct EmitObject {
    pub entry: SymbolEntry,
    pub source_file: String,
    /// Object declaration range in 0-based UTF-16 coordinates. Native build
    /// diagnostics use this instead of collapsing structural errors to 1:1.
    pub source_range: al_syntax::SyntaxRange,
    /// Per-enum-value properties (e.g. a value's `Caption`), parallel to
    /// `entry.enum_values`. Held here rather than on the shared
    /// `EnumValueSymbol` model to avoid churn across unrelated readers.
    pub enum_value_properties: Vec<Vec<PropertyValue>>,
    /// Query dataitems/columns (queries only) — not modelled in `SymbolEntry`.
    pub query_elements: Vec<QueryElement>,
    /// Permission clauses (permissionsets only).
    pub permissions: Vec<PermissionDecl>,
    /// Page layout controls (pages / pageextensions).
    pub page_controls: Vec<PageControl>,
    /// Page actions (pages / pageextensions).
    pub page_actions: Vec<PageControl>,
    /// Layout change operations (pageextensions; reportextension request page).
    pub control_changes: Vec<ControlChange>,
    /// Dataset change operations (reportextensions).
    pub dataset_changes: Vec<DatasetChange>,
    /// Rendering layouts (reports / reportextensions).
    pub report_layouts: Vec<ReportLayout>,
    /// Table field groups (`fieldgroups { fieldgroup(...) }`).
    pub field_groups: Vec<KeySymbol>,
}

/// Extract every top-level object declared in `source`. `source_file` is the
/// in-`.app` archive path recorded on each object (e.g. `src/Lib.al`).
pub fn extract_objects(source: &str, source_file: &str) -> Vec<EmitObject> {
    let result = AlParser::parse_quick(source);
    extract_objects_from_tree(source, source_file, &result.tree)
}

/// Extract top-level objects from an existing parse tree.
///
/// The verified build path parses every file once, checks that tree for syntax
/// errors, then passes the same snapshot here. Keeping this entry point avoids
/// paying for a second parse and, more importantly, prevents verification and
/// emission from observing different source snapshots.
pub fn extract_objects_from_tree(source: &str, source_file: &str, tree: &Tree) -> Vec<EmitObject> {
    let src = source.as_bytes();
    let root = tree.root_node();
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "object_declaration" {
            if let Some(ex) = extract_object(child, src) {
                out.push(EmitObject {
                    entry: ex.entry,
                    source_file: source_file.to_string(),
                    source_range: al_syntax::ts_range_to_syntax(&child.range(), src),
                    enum_value_properties: ex.enum_value_properties,
                    query_elements: ex.query_elements,
                    permissions: ex.permissions,
                    page_controls: ex.page_controls,
                    page_actions: ex.page_actions,
                    control_changes: ex.control_changes,
                    dataset_changes: ex.dataset_changes,
                    report_layouts: ex.report_layouts,
                    field_groups: ex.field_groups,
                });
            }
        }
    }
    out
}

#[derive(Default)]
struct ExtractedObject {
    entry: SymbolEntry,
    enum_value_properties: Vec<Vec<PropertyValue>>,
    query_elements: Vec<QueryElement>,
    permissions: Vec<PermissionDecl>,
    page_controls: Vec<PageControl>,
    page_actions: Vec<PageControl>,
    control_changes: Vec<ControlChange>,
    dataset_changes: Vec<DatasetChange>,
    report_layouts: Vec<ReportLayout>,
    field_groups: Vec<KeySymbol>,
}

fn extract_object(node: Node, src: &[u8]) -> Option<ExtractedObject> {
    let kind_str = node
        .child_by_field_name("kind")
        .map(|k| {
            let raw = k.kind();
            if raw == "object_keyword" {
                text(k, src).to_lowercase()
            } else {
                raw.strip_prefix("kw_").unwrap_or(raw).to_string()
            }
        })
        .unwrap_or_default();
    let kind = kind_str.parse::<ObjectKind>().ok()?;

    let id = node
        .child_by_field_name("id")
        .and_then(|n| text(n, src).parse::<i32>().ok())
        .unwrap_or(0);
    let name = al_syntax::extract_object_name(node, src).unwrap_or_default();

    let body = node.child_by_field_name("body");

    let mut entry = SymbolEntry {
        kind,
        id,
        name,
        ..Default::default()
    };
    let mut enum_value_props: Vec<Vec<PropertyValue>> = Vec::new();
    let mut query_elements: Vec<QueryElement> = Vec::new();
    let mut permissions: Vec<PermissionDecl> = Vec::new();
    let mut page_controls: Vec<PageControl> = Vec::new();
    let mut page_actions: Vec<PageControl> = Vec::new();
    let mut control_changes: Vec<ControlChange> = Vec::new();
    let mut dataset_changes: Vec<DatasetChange> = Vec::new();
    let mut report_layouts: Vec<ReportLayout> = Vec::new();
    let mut field_groups: Vec<KeySymbol> = Vec::new();

    // The grammar uses `implements_clause` for BOTH `extends` (extension target)
    // and `implements` (interfaces); the leading keyword disambiguates.
    let mut c = node.walk();
    for child in node.children(&mut c) {
        if child.kind() != "implements_clause" {
            continue;
        }
        // `extends` (extensions) and `customizes` (page customizations) both name
        // a base object; `implements` names interfaces.
        let is_extends = child_of_kind(child, "metadata_keyword")
            .map(|k| {
                let kw = text(k, src);
                kw.eq_ignore_ascii_case("extends") || kw.eq_ignore_ascii_case("customizes")
            })
            .unwrap_or(false);
        let mut ic = child.walk();
        for n in child.children(&mut ic) {
            if n.kind() == "name" || n.kind() == "name_or_keyword" {
                if is_extends {
                    entry.extends = Some(unquote(text(n, src)));
                } else {
                    // Interface names are recorded verbatim (with quotes).
                    entry.implements.push(text(n, src).to_string());
                }
            }
        }
    }

    if let Some(body) = body {
        match kind {
            ObjectKind::Table | ObjectKind::TableExtension => {
                entry.fields = extract_table_fields(body, src);
                entry.keys = extract_table_keys(body, src);
                field_groups = extract_field_groups(body, src);
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::Codeunit => {
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::Interface => {
                entry.methods = extract_methods(body, src);
            }
            ObjectKind::Enum | ObjectKind::EnumExtension => {
                let (values, props) = extract_enum_values(body, src);
                entry.enum_values = values;
                enum_value_props = props;
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::Query => {
                query_elements = extract_query_elements(body, src);
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::Page => {
                page_controls = section_with_keyword(body, src, "layout")
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_page_nodes(b, src))
                    .unwrap_or_default();
                page_actions = section_with_keyword(body, src, "actions")
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_page_nodes(b, src))
                    .unwrap_or_default();
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::PageExtension | ObjectKind::PageCustomization => {
                control_changes = section_with_keyword(body, src, "layout")
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_control_changes(b, src))
                    .unwrap_or_default();
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::Report => {
                query_elements = extract_dataitems_in_section(body, src, "dataset");
                // The request page's layout controls.
                page_controls = section_with_keyword(body, src, "requestpage")
                    .and_then(|s| s.child_by_field_name("body"))
                    .and_then(|rb| section_with_keyword(rb, src, "layout"))
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_page_nodes(b, src))
                    .unwrap_or_default();
                report_layouts = extract_report_layouts(body, src);
                entry.variables = extract_global_variables(body, src);
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::ReportExtension => {
                dataset_changes = section_with_keyword(body, src, "dataset")
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_dataset_changes(b, src))
                    .unwrap_or_default();
                control_changes = section_with_keyword(body, src, "requestpage")
                    .and_then(|s| s.child_by_field_name("body"))
                    .and_then(|rb| section_with_keyword(rb, src, "layout"))
                    .and_then(|s| s.child_by_field_name("body"))
                    .map(|b| extract_control_changes(b, src))
                    .unwrap_or_default();
                report_layouts = extract_report_layouts(body, src);
                entry.variables = extract_global_variables(body, src);
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
            ObjectKind::PermissionSet | ObjectKind::PermissionSetExtension => {
                permissions = extract_permissions(body, src);
                // The `Permissions` property is emitted as the Permissions array,
                // not as a plain property.
                entry.properties = extract_object_properties(body, src)
                    .into_iter()
                    .filter(|p| !p.name.eq_ignore_ascii_case("Permissions"))
                    .collect();
            }
            _ => {
                entry.methods = extract_methods(body, src);
                entry.properties = extract_object_properties(body, src);
            }
        }
    }
    Some(ExtractedObject {
        entry,
        enum_value_properties: enum_value_props,
        query_elements,
        permissions,
        page_controls,
        page_actions,
        control_changes,
        dataset_changes,
        report_layouts,
        field_groups,
    })
}

/// Whether a keyword is a page/report extension change operation.
fn is_change_keyword(kw: &str) -> bool {
    matches!(
        kw,
        "add"
            | "addfirst"
            | "addlast"
            | "addbefore"
            | "addafter"
            | "movefirst"
            | "movelast"
            | "movebefore"
            | "moveafter"
            | "modify"
    )
}

fn extract_control_changes(layout_body: Node, src: &[u8]) -> Vec<ControlChange> {
    let mut out = Vec::new();
    let mut c = layout_body.walk();
    for child in layout_body.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let kw = child
            .child_by_field_name("keyword")
            .map(|k| text(k, src).to_ascii_lowercase())
            .unwrap_or_default();
        if !is_change_keyword(&kw) {
            continue;
        }
        let anchor = child_of_kind(child, "parenthesized_block")
            .map(|p| paren_parts(p, src))
            .and_then(|p| p.first().map(|s| unquote(s)))
            .unwrap_or_default();
        let controls = child
            .child_by_field_name("body")
            .map(|b| extract_page_nodes(b, src))
            .unwrap_or_default();
        out.push(ControlChange {
            kind: kw,
            anchor,
            controls,
        });
    }
    out
}

/// Extract `rendering { layout(Name) { props } }` entries from a report body.
fn extract_report_layouts(body: Node, src: &[u8]) -> Vec<ReportLayout> {
    let Some(rendering) =
        section_with_keyword(body, src, "rendering").and_then(|s| s.child_by_field_name("body"))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = rendering.walk();
    for child in rendering.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let kw = child
            .child_by_field_name("keyword")
            .map(|k| text(k, src).to_ascii_lowercase())
            .unwrap_or_default();
        if kw != "layout" {
            continue;
        }
        let name = child_of_kind(child, "parenthesized_block")
            .map(|p| paren_parts(p, src))
            .and_then(|p| p.first().map(|s| unquote(s)))
            .unwrap_or_default();
        let properties = child
            .child_by_field_name("body")
            .map(|b| extract_object_properties(b, src))
            .unwrap_or_default();
        out.push(ReportLayout { name, properties });
    }
    out
}

fn extract_dataset_changes(dataset_body: Node, src: &[u8]) -> Vec<DatasetChange> {
    let mut out = Vec::new();
    let mut c = dataset_body.walk();
    for child in dataset_body.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let kw = child
            .child_by_field_name("keyword")
            .map(|k| text(k, src).to_ascii_lowercase())
            .unwrap_or_default();
        if !is_change_keyword(&kw) {
            continue;
        }
        let anchor = child_of_kind(child, "parenthesized_block")
            .map(|p| paren_parts(p, src))
            .and_then(|p| p.first().map(|s| unquote(s)))
            .unwrap_or_default();
        let mut columns = Vec::new();
        let mut dataitems = Vec::new();
        if let Some(body) = child.child_by_field_name("body") {
            let mut bc = body.walk();
            for sub in body.children(&mut bc) {
                if sub.kind() != "object_section" {
                    continue;
                }
                let skw = sub
                    .child_by_field_name("keyword")
                    .map(|k| text(k, src).to_ascii_lowercase())
                    .unwrap_or_default();
                match skw.as_str() {
                    "column" => {
                        let p = child_of_kind(sub, "parenthesized_block")
                            .map(|p| paren_parts(p, src))
                            .unwrap_or_default();
                        let cname = p.first().map(|s| unquote(s)).unwrap_or_default();
                        let source = p
                            .get(1)
                            .map(|s| unquote(s))
                            .unwrap_or_else(|| cname.clone());
                        columns.push((cname, source));
                    }
                    "dataitem" => dataitems.push(extract_dataitem(sub, src)),
                    _ => {}
                }
            }
        }
        out.push(DatasetChange {
            kind: kw,
            anchor,
            columns,
            dataitems,
        });
    }
    out
}

/// The control/action keywords (containers + leaves) we descend into.
fn is_page_node_keyword(kw: &str) -> bool {
    matches!(
        kw,
        "area"
            | "group"
            | "cuegroup"
            | "repeater"
            | "fixed"
            | "grid"
            | "part"
            | "systempart"
            | "field"
            | "label"
            | "usercontrol"
            | "action"
            | "separator"
            | "actionref"
            | "systemaction"
            | "fileuploadaction"
    )
}

fn extract_page_nodes(parent_body: Node, src: &[u8]) -> Vec<PageControl> {
    let mut out = Vec::new();
    let mut c = parent_body.walk();
    for child in parent_body.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let kw = child
            .child_by_field_name("keyword")
            .map(|k| text(k, src).to_ascii_lowercase())
            .unwrap_or_default();
        if is_page_node_keyword(&kw) {
            out.push(extract_page_node(child, &kw, src));
        }
    }
    out
}

fn extract_page_node(section: Node, keyword: &str, src: &[u8]) -> PageControl {
    // parenthesized_block: `(Name)` for containers, `(Name; <expr tokens>)` for
    // fields. The expression is the tokens after the first `;`.
    let mut name = String::new();
    let mut expr_tokens: Vec<String> = Vec::new();
    let mut seen_semi = false;
    if let Some(pblock) = child_of_kind(section, "parenthesized_block") {
        let mut pc = pblock.walk();
        for tok in pblock.children(&mut pc) {
            match tok.kind() {
                "(" | ")" => {}
                "semicolon" if !seen_semi => seen_semi = true,
                _ if !seen_semi => name = unquote(text(tok, src)),
                _ => expr_tokens.push(text(tok, src).to_string()),
            }
        }
    }
    let source_expr = if expr_tokens.is_empty() {
        None
    } else {
        Some(expr_tokens.join(""))
    };
    let body = section.child_by_field_name("body");
    let properties = body
        .map(|b| extract_object_properties(b, src))
        .unwrap_or_default();
    let children = body.map(|b| extract_page_nodes(b, src)).unwrap_or_default();
    PageControl {
        keyword: keyword.to_string(),
        name,
        source_expr,
        properties,
        children,
    }
}

fn extract_global_variables(body: Node, src: &[u8]) -> Vec<VariableSymbol> {
    let mut out = Vec::new();
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() != "object_var_section" {
            continue;
        }
        let mut vc = child.walk();
        for v in child.children(&mut vc) {
            if v.kind() != "object_variable_declaration" {
                continue;
            }
            let decl = child_of_kind(v, "regular_variable_declaration").unwrap_or(v);
            let name = decl
                .child_by_field_name("name")
                .map(|n| unquote(text(n, src)))
                .unwrap_or_default();
            let type_name = decl
                .child_by_field_name("type")
                .or_else(|| child_of_kind(decl, "type_reference"))
                .map(|t| extract_type_reference(t, src))
                .unwrap_or_default();
            if !name.is_empty() {
                out.push(VariableSymbol {
                    name,
                    type_name,
                    is_protected: false,
                });
            }
        }
    }
    out
}

fn extract_permissions(body: Node, src: &[u8]) -> Vec<PermissionDecl> {
    let mut c = body.walk();
    let mut tokens: Vec<(String, String)> = Vec::new();
    for child in body.children(&mut c) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let is_perms = child
            .child_by_field_name("name")
            .map(|n| text(n, src).eq_ignore_ascii_case("Permissions"))
            .unwrap_or(false);
        if !is_perms {
            continue;
        }
        let mut pc = child.walk();
        if pc.goto_first_child() {
            loop {
                if pc.field_name() == Some("value") {
                    let n = pc.node();
                    tokens.push((n.kind().to_string(), text(n, src).to_string()));
                }
                if !pc.goto_next_sibling() {
                    break;
                }
            }
        }
        break;
    }

    // Split the token stream on commas into `objtype name = perms` clauses.
    let mut out = Vec::new();
    for group in tokens.split(|(k, _)| k == "comma") {
        // Drop the `=` operator; the remaining tokens are objtype, name, perms.
        let parts: Vec<&(String, String)> = group.iter().filter(|(k, _)| k != "operator").collect();
        if parts.len() >= 3 {
            out.push(PermissionDecl {
                object_type: parts[0].1.clone(),
                object_name: unquote(&parts[1].1),
                permission: parts[2].1.clone(),
            });
        }
    }
    out
}

fn extract_query_elements(body: Node, src: &[u8]) -> Vec<QueryElement> {
    extract_dataitems_in_section(body, src, "elements")
}

/// Extract `dataitem(...)` trees from a named section (`elements` for queries,
/// `dataset` for reports).
fn extract_dataitems_in_section(body: Node, src: &[u8], section_kw: &str) -> Vec<QueryElement> {
    section_with_keyword(body, src, section_kw)
        .and_then(|s| s.child_by_field_name("body"))
        .map(|b| collect_dataitems(b, src))
        .unwrap_or_default()
}

fn collect_dataitems(parent_body: Node, src: &[u8]) -> Vec<QueryElement> {
    let mut out = Vec::new();
    let mut c = parent_body.walk();
    for child in parent_body.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let kw = child
            .child_by_field_name("keyword")
            .map(|k| text(k, src).to_ascii_lowercase())
            .unwrap_or_default();
        if kw != "dataitem" {
            continue;
        }
        out.push(extract_dataitem(child, src));
    }
    out
}

fn extract_dataitem(section: Node, src: &[u8]) -> QueryElement {
    let parts = child_of_kind(section, "parenthesized_block")
        .map(|p| paren_parts(p, src))
        .unwrap_or_default();
    let name = parts.first().map(|s| unquote(s)).unwrap_or_default();
    let related_table = parts.get(1).map(|s| unquote(s));
    let mut columns = Vec::new();
    let mut children = Vec::new();
    if let Some(body) = section.child_by_field_name("body") {
        let mut c = body.walk();
        for child in body.children(&mut c) {
            if child.kind() != "object_section" {
                continue;
            }
            let kw = child
                .child_by_field_name("keyword")
                .map(|k| text(k, src).to_ascii_lowercase())
                .unwrap_or_default();
            match kw.as_str() {
                "column" => {
                    let p = child_of_kind(child, "parenthesized_block")
                        .map(|p| paren_parts(p, src))
                        .unwrap_or_default();
                    let cname = p.first().map(|s| unquote(s)).unwrap_or_default();
                    let source = p
                        .get(1)
                        .map(|s| unquote(s))
                        .unwrap_or_else(|| cname.clone());
                    columns.push((cname, source));
                }
                "dataitem" => children.push(extract_dataitem(child, src)),
                _ => {}
            }
        }
    }
    QueryElement {
        name,
        related_table,
        columns,
        children,
    }
}

/// Find a direct `object_section` child of `body` with the given keyword.
fn section_with_keyword<'a>(body: Node<'a>, src: &[u8], keyword: &str) -> Option<Node<'a>> {
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() == "object_section" {
            if let Some(kw) = child.child_by_field_name("keyword") {
                if text(kw, src).eq_ignore_ascii_case(keyword) {
                    return Some(child);
                }
            }
        }
    }
    None
}

fn extract_methods(body: Node, src: &[u8]) -> Vec<MethodSymbol> {
    let mut out = Vec::new();
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() == "procedure_declaration" || child.kind() == "event_procedure_declaration"
        {
            out.push(extract_procedure(child, src));
        }
    }
    out
}

fn extract_procedure(node: Node, src: &[u8]) -> MethodSymbol {
    let name = node
        .child_by_field_name("name")
        .map(|n| text(n, src).to_string())
        .unwrap_or_default();
    let parameters = node
        .child_by_field_name("parameters")
        .map(|p| extract_parameters(p, src))
        .unwrap_or_default();
    let return_type = node
        .child_by_field_name("return_type")
        .map(|t| extract_type_reference(t, src));
    let attributes = extract_attributes(node, src);
    // `local` is a `member_modifier` before the `procedure` keyword (the keyword
    // is nested inside that node), so check the modifier text, not just direct
    // `kw_local` children.
    let is_local = {
        let mut local = false;
        let mut c = node.walk();
        for n in node.children(&mut c) {
            let is_kw_local = n.kind() == "kw_local"
                || (n.kind() == "member_modifier" && text(n, src).eq_ignore_ascii_case("local"));
            if is_kw_local {
                local = true;
            }
        }
        local
    };
    MethodSymbol {
        name,
        parameters,
        return_type,
        attributes,
        is_local,
    }
}

fn extract_parameters(param_list: Node, src: &[u8]) -> Vec<ParameterSymbol> {
    let mut out = Vec::new();
    let mut c = param_list.walk();
    for child in param_list.children(&mut c) {
        if child.kind() != "parameter" {
            continue;
        }
        let mut is_var = false;
        let mut pc = child.walk();
        for n in child.children(&mut pc) {
            if n.kind() == "kw_var" {
                is_var = true;
            }
        }
        let name = child
            .child_by_field_name("name")
            .map(|n| text(n, src).to_string())
            .unwrap_or_default();
        let type_name = child
            .child_by_field_name("type")
            .map(|t| extract_type_reference(t, src))
            .unwrap_or_default();
        out.push(ParameterSymbol {
            name: unquote(&name),
            type_name,
            is_var,
        });
    }
    out
}

fn extract_attributes(proc_node: Node, src: &[u8]) -> Vec<AttributeSymbol> {
    let mut out = Vec::new();
    let mut c = proc_node.walk();
    for child in proc_node.children(&mut c) {
        if child.kind() != "attribute" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .map(|n| text(n, src).to_string())
            .unwrap_or_else(|| {
                // Fall back to the first identifier child.
                child_of_kind(child, "identifier")
                    .map(|n| text(n, src).to_string())
                    .unwrap_or_default()
            });
        let mut arguments = Vec::new();
        let mut ac = child.walk();
        for n in child.children(&mut ac) {
            if n.kind() == "attribute_argument_list" {
                let mut lc = n.walk();
                for arg in n.children(&mut lc) {
                    if arg.kind() == "attribute_argument" {
                        arguments.push(text(arg, src).trim().to_string());
                    }
                }
            }
        }
        out.push(AttributeSymbol { name, arguments });
    }
    out
}

/// Build a full type string from a `type_reference` node, e.g. `Integer`,
/// `Text[100]`, `Enum "Color"`, `Record "Customer"`.
fn extract_type_reference(node: Node, src: &[u8]) -> String {
    // `type_reference` wraps the actual node; if given the wrapper, descend.
    let tr = if node.kind() == "type_reference" {
        node
    } else {
        child_of_kind(node, "type_reference").unwrap_or(node)
    };
    let mut base = String::new();
    let mut suffix = String::new();
    let mut subtype = String::new();
    let mut c = tr.walk();
    for child in tr.children(&mut c) {
        let k = child.kind();
        if k == "bracketed_block" {
            suffix = text(child, src).to_string(); // "[100]"
        } else if base.is_empty() && is_type_node(k) {
            base = text(child, src).to_string();
        } else if matches!(k, "name_or_keyword" | "name" | "quoted_identifier") {
            subtype = unquote(text(child, src));
        }
    }
    compose_type(&base, &suffix, &subtype)
}

fn compose_type(base: &str, suffix: &str, subtype: &str) -> String {
    if !subtype.is_empty() {
        format!("{base} \"{subtype}\"")
    } else if !suffix.is_empty() {
        format!("{base}{suffix}")
    } else {
        base.to_string()
    }
}

fn extract_table_fields(body: Node, src: &[u8]) -> Vec<FieldSymbol> {
    let Some(fields_section) = find_section(body, src, "field") else {
        return Vec::new();
    };
    let Some(inner) = fields_section.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = inner.walk();
    for child in inner.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        if let Some(f) = extract_one_field(child, src) {
            out.push(f);
        }
    }
    out
}

fn extract_one_field(section: Node, src: &[u8]) -> Option<FieldSymbol> {
    let pblock = child_of_kind(section, "parenthesized_block")?;
    // parenthesized_block: integer ; name ; <type pieces...>
    let parts = paren_parts(pblock, src);
    let id = parts.first()?.parse::<i32>().ok()?;
    let name = unquote(parts.get(1)?);
    let type_name = field_type_from_block(pblock, src);
    let properties = section
        .child_by_field_name("body")
        .map(|b| extract_object_properties(b, src))
        .unwrap_or_default();
    Some(FieldSymbol {
        id,
        name,
        type_name,
        properties,
    })
}

/// Field types live inside the `parenthesized_block` as a `control_keyword`
/// optionally followed by a `bracketed_block` (length) or a subtype name.
fn field_type_from_block(pblock: Node, src: &[u8]) -> String {
    let mut base = String::new();
    let mut suffix = String::new();
    let mut subtype = String::new();
    let mut seen_semis = 0;
    let mut c = pblock.walk();
    for child in pblock.children(&mut c) {
        let k = child.kind();
        if k == "semicolon" {
            seen_semis += 1;
        } else if seen_semis < 2 {
            // id ; name come before the type.
        } else if k == "bracketed_block" {
            suffix = text(child, src).to_string();
        } else if base.is_empty() && is_type_node(k) {
            base = text(child, src).to_string();
        } else if matches!(
            k,
            "quoted_identifier" | "name_or_keyword" | "name" | "identifier"
        ) {
            if base.is_empty() {
                base = text(child, src).to_string();
            } else {
                subtype = unquote(text(child, src));
            }
        }
    }
    compose_type(&base, &suffix, &subtype)
}

/// Extract `fieldgroups { fieldgroup(Name; f1, f2) }` — same shape as keys.
fn extract_field_groups(body: Node, src: &[u8]) -> Vec<KeySymbol> {
    let Some(section) = find_section(body, src, "fieldgroup") else {
        return Vec::new();
    };
    let Some(inner) = section.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = inner.walk();
    for child in inner.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let Some(pblock) = child_of_kind(child, "parenthesized_block") else {
            continue;
        };
        let parts = paren_parts(pblock, src);
        let Some(name) = parts.first() else { continue };
        out.push(KeySymbol {
            name: unquote(name),
            field_names: parts.iter().skip(1).map(|s| unquote(s)).collect(),
            properties: Vec::new(),
        });
    }
    out
}

fn extract_table_keys(body: Node, src: &[u8]) -> Vec<KeySymbol> {
    let Some(keys_section) = find_section(body, src, "key") else {
        return Vec::new();
    };
    let Some(inner) = keys_section.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut c = inner.walk();
    for child in inner.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        let Some(pblock) = child_of_kind(child, "parenthesized_block") else {
            continue;
        };
        let parts = paren_parts(pblock, src);
        let Some(name) = parts.first() else { continue };
        let field_names = parts.iter().skip(1).map(|s| unquote(s)).collect();
        let properties = child
            .child_by_field_name("body")
            .map(|b| extract_object_properties(b, src))
            .unwrap_or_default();
        out.push(KeySymbol {
            name: unquote(name),
            field_names,
            properties,
        });
    }
    out
}

fn extract_enum_values(body: Node, src: &[u8]) -> (Vec<EnumValueSymbol>, Vec<Vec<PropertyValue>>) {
    let mut values = Vec::new();
    let mut props = Vec::new();
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() != "enum_value_declaration" {
            continue;
        }
        let ordinal = child
            .child_by_field_name("id")
            .and_then(|n| text(n, src).parse::<i32>().ok())
            .unwrap_or(0);
        let name = child
            .child_by_field_name("name")
            .map(|n| unquote(text(n, src)))
            .unwrap_or_default();
        let value_props = child
            .child_by_field_name("body")
            .or_else(|| child_of_kind(child, "object_body"))
            .map(|b| extract_object_properties(b, src))
            .unwrap_or_default();
        values.push(EnumValueSymbol { ordinal, name });
        props.push(value_props);
    }
    (values, props)
}

fn extract_object_properties(body: Node, src: &[u8]) -> Vec<PropertyValue> {
    let mut out = Vec::new();
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let name = child
            .child_by_field_name("name")
            .map(|n| text(n, src).to_string())
            .unwrap_or_default();
        // The grammar's `value` field is `repeat1(...)`, so a list value
        // (`Scripts = 'a', 'b';`) spans several nodes — take the full source span
        // from the first value node to the last, not just the first.
        let mut vc = child.walk();
        let value_nodes: Vec<Node> = child.children_by_field_name("value", &mut vc).collect();
        let value = match (value_nodes.first(), value_nodes.last()) {
            (Some(first), Some(last)) => {
                let raw = std::str::from_utf8(&src[first.start_byte()..last.end_byte()])
                    .unwrap_or_default();
                unquote_prop_value(raw)
            }
            _ => String::new(),
        };
        if !name.is_empty() {
            out.push(PropertyValue { name, value });
        }
    }
    out
}

fn text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Strip a property value's surrounding quotes — single (`'…'` string) or
/// double (`"…"` identifier, e.g. an object reference like `RoleCenter`). Only a
/// *single* quoted token is unwrapped: a multi-token expression like
/// `"Sweep Hdr"."No."` (a TableRelation) keeps its quotes verbatim, detected by
/// an inner occurrence of the same quote.
fn unquote_prop_value(s: &str) -> String {
    let t = s.trim();
    let q = t.chars().next();
    if t.len() >= 2 && (q == Some('\'') || q == Some('"')) && t.ends_with(q.unwrap()) {
        let inner = &t[1..t.len() - 1];
        if !inner.contains(q.unwrap()) {
            return inner.to_string();
        }
    }
    t.to_string()
}

fn child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    // Index-based to avoid the cursor-lifetime trap when returning the Node.
    for i in 0..node.child_count() {
        if let Some(n) = node.child(i) {
            if n.kind() == kind {
                return Some(n);
            }
        }
    }
    None
}

/// The non-punctuation tokens of a `parenthesized_block`, split on `;` and `,`
/// into a flat list (e.g. `(1; "Entry No."; Integer)` → `["1", "\"Entry No.\"", ...]`).
fn paren_parts(pblock: Node, src: &[u8]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut c = pblock.walk();
    for child in pblock.children(&mut c) {
        match child.kind() {
            "(" | ")" | "semicolon" | "comma" => {}
            _ => parts.push(text(child, src).to_string()),
        }
    }
    parts
}

/// Find the `fields`/`keys`-style `object_section` whose inner sections use the
/// given member `keyword` (e.g. inner `field` or `key`).
fn find_section<'a>(body: Node<'a>, src: &[u8], member_keyword: &str) -> Option<Node<'a>> {
    let mut c = body.walk();
    for child in body.children(&mut c) {
        if child.kind() != "object_section" {
            continue;
        }
        // Look one level down for an inner section with the member keyword.
        if let Some(inner) = child.child_by_field_name("body") {
            let mut ic = inner.walk();
            for sub in inner.children(&mut ic) {
                if sub.kind() == "object_section" {
                    if let Some(kw) = sub.child_by_field_name("keyword") {
                        if text(kw, src).eq_ignore_ascii_case(member_keyword) {
                            return Some(child);
                        }
                    }
                }
            }
        }
    }
    None
}
