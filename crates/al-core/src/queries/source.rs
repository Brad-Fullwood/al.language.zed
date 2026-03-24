//! Source extraction query — `al source`.
//!
//! Returns source code for an AL object, optionally filtered to a specific
//! procedure or trigger. Three source levels:
//! - `workspace`: full source from .al file, tree-sitter range for procedures
//! - `package`: source extracted from .app ZIP archive
//! - `outline`: rendered from SymbolReference.json (full signatures, no bodies)

use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry};
use serde::Serialize;

use crate::workspace::Workspace;

/// Source level indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLevel {
    Workspace,
    Package,
    Outline,
}

/// Result of a source extraction query.
#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    /// Object kind.
    pub k: ObjectKind,
    /// Object ID.
    pub id: i32,
    /// Object name.
    pub n: String,
    /// Procedure/trigger filter (if applied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proc_name: Option<String>,
    /// Source level.
    pub src: SourceLevel,
    /// Package name (for package/outline sources).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pkg: Option<String>,
    /// Procedure signature (if filtered to a specific procedure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Source code range in workspace file (if workspace + procedure filter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<SourceRange>,
    /// The source code or outline text.
    pub code: String,
    /// Note about the source (e.g., for outline mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// File range for workspace source.
#[derive(Debug, Clone, Serialize)]
pub struct SourceRange {
    /// Relative file path.
    pub f: String,
    /// Start line (1-based).
    pub l: u32,
    /// End line (1-based).
    pub end: u32,
}

/// Query source for an object, optionally filtered to a procedure or trigger.
pub fn source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    proc_filter: Option<&str>,
    trigger_filter: Option<&str>,
) -> Option<SourceResult> {
    // 1. Try workspace files first
    if let Some(result) = try_workspace_source(workspace, name, kind_filter, proc_filter, trigger_filter) {
        return Some(result);
    }

    // 2. Try package source (symbol index)
    try_package_source(workspace, name, kind_filter, proc_filter, trigger_filter)
}

/// Try to extract source from workspace .al files.
fn try_workspace_source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    proc_filter: Option<&str>,
    trigger_filter: Option<&str>,
) -> Option<SourceResult> {
    let file_path = workspace.file_index.find_by_object_name(name)?;

    // Read the file content
    // SILENT: non-absolute paths can't become file URIs
    let uri = url::Url::from_file_path(&file_path).ok()?;
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, &uri)?;

    // Find the object declaration to get kind and id
    let obj_info = al_syntax::find_object_declaration(&tree, &text)?;
    let kind: ObjectKind = obj_info.kind.parse().ok()?;
    let id = obj_info.id.unwrap_or(0) as i32;

    if let Some(k) = kind_filter {
        if k != kind {
            return None;
        }
    }

    let member_filter = proc_filter.or(trigger_filter);

    if let Some(member_name) = member_filter {
        // Extract specific procedure/trigger using tree-sitter
        let root = tree.root_node();
        if let Some((node, sig)) = find_procedure_node(&root, &text, member_name) {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            let code = node.utf8_text(text.as_bytes()).unwrap_or("").to_string();

            let relative_path = file_path.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();

            return Some(SourceResult {
                k: kind,
                id,
                n: name.to_string(),
                proc_name: Some(member_name.to_string()),
                src: SourceLevel::Workspace,
                pkg: None,
                sig: Some(sig),
                range: Some(SourceRange {
                    f: relative_path,
                    l: start_line as u32 + 1,
                    end: end_line as u32 + 1,
                }),
                code,
                note: None,
            });
        }
        return None;
    }

    // Full object source
    Some(SourceResult {
        k: kind,
        id,
        n: name.to_string(),
        proc_name: None,
        src: SourceLevel::Workspace,
        pkg: None,
        sig: None,
        range: None,
        code: (*text).clone(),
        note: None,
    })
}

/// Try to extract source from package (.app) files.
fn try_package_source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    proc_filter: Option<&str>,
    trigger_filter: Option<&str>,
) -> Option<SourceResult> {
    let entries = workspace.symbols.get_by_name(name);
    let entry = if let Some(k) = kind_filter {
        entries.iter().find(|e| e.kind == k)
    } else {
        entries.first()
    }?;

    let app_path = workspace.symbols.app_path(&entry.package);

    // Try extracting source from .app ZIP
    if let Some(ref path) = app_path {
        if let Ok(source_index) = al_symbols::source_index::get_or_build(path) {
            if let Some(full_source) = source_index.extract_source_for_entry(entry) {
                let member_filter = proc_filter.or(trigger_filter);
                if let Some(member_name) = member_filter {
                    // Parse and extract specific procedure from package source
                    if let Some((code, sig)) = extract_procedure_from_text(&full_source, member_name) {
                        return Some(SourceResult {
                            k: entry.kind,
                            id: entry.id,
                            n: entry.name.clone(),
                            proc_name: Some(member_name.to_string()),
                            src: SourceLevel::Package,
                            pkg: Some(entry.package.clone()),
                            sig: Some(sig),
                            range: None,
                            code,
                            note: None,
                        });
                    }
                    return None;
                }

                return Some(SourceResult {
                    k: entry.kind,
                    id: entry.id,
                    n: entry.name.clone(),
                    proc_name: None,
                    src: SourceLevel::Package,
                    pkg: Some(entry.package.clone()),
                    sig: None,
                    range: None,
                    code: full_source,
                    note: None,
                });
            }
        }
    }

    // Render outline from SymbolReference.json (standard output for packages without source)
    let member_filter = proc_filter.or(trigger_filter);
    if let Some(member_name) = member_filter {
        // Find specific procedure in symbol entry
        let method = entry.methods.iter().find(|m| m.name.eq_ignore_ascii_case(member_name))?;
        let sig = render_method_signature(method);
        return Some(SourceResult {
            k: entry.kind,
            id: entry.id,
            n: entry.name.clone(),
            proc_name: Some(member_name.to_string()),
            src: SourceLevel::Outline,
            pkg: Some(entry.package.clone()),
            sig: Some(sig.clone()),
            range: None,
            code: sig,
            note: Some("Rendered from symbol metadata — signature only, no implementation body".to_string()),
        });
    }

    let code = render_outline(entry);
    Some(SourceResult {
        k: entry.kind,
        id: entry.id,
        n: entry.name.clone(),
        proc_name: None,
        src: SourceLevel::Outline,
        pkg: Some(entry.package.clone()),
        sig: None,
        range: None,
        code,
        note: Some("Rendered from symbol metadata — full signatures and fields, no implementation bodies".to_string()),
    })
}

/// Find a procedure/trigger node in a tree-sitter tree and return (node, signature).
fn find_procedure_node<'a>(
    root: &'a tree_sitter::Node<'a>,
    source: &str,
    name: &str,
) -> Option<(tree_sitter::Node<'a>, String)> {
    let mut cursor = root.walk();
    find_procedure_recursive(&mut cursor, source, name)
}

fn find_procedure_recursive<'a>(
    cursor: &mut tree_sitter::TreeCursor<'a>,
    source: &str,
    name: &str,
) -> Option<(tree_sitter::Node<'a>, String)> {
    loop {
        let node = cursor.node();
        let kind = node.kind();

        if kind == "method_declaration" || kind == "trigger_declaration" {
            // Find the method/trigger name child
            if let Some(name_node) = node.child_by_field_name("name") {
                let node_name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let clean = node_name.trim_matches('"');
                if clean.eq_ignore_ascii_case(name) {
                    // Build signature from the first line up to the first newline or ')'
                    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
                    let sig = extract_signature_from_text(text);
                    return Some((node, sig));
                }
            }
        }

        // Recurse into children
        if cursor.goto_first_child() {
            if let Some(result) = find_procedure_recursive(cursor, source, name) {
                return Some(result);
            }
            cursor.goto_parent();
        }

        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// Extract signature from the beginning of a procedure text.
fn extract_signature_from_text(text: &str) -> String {
    // Take text up to and including the first closing paren that completes the signature
    let mut depth = 0i32;
    let mut end = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    // Check for return type immediately after ')' on the same line
                    let rest = &text[end..];
                    // Only look for ':' before the next newline
                    let same_line = rest.split('\n').next().unwrap_or("");
                    if let Some(colon_pos) = same_line.find(':') {
                        // Include the return type (everything after ':' on same line)
                        let return_part = same_line[colon_pos..].trim_end_matches(';').trim_end();
                        end = end + colon_pos + return_part.len();
                    }
                    break;
                }
            }
            '\n' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    if end == 0 {
        // Fallback: first line
        text.lines().next().unwrap_or(text).to_string()
    } else {
        text[..end].trim().to_string()
    }
}

/// Extract a specific procedure from source text by parsing with tree-sitter.
fn extract_procedure_from_text(source: &str, name: &str) -> Option<(String, String)> {
    let result = al_syntax::AlParser::parse_quick(source);
    let root = result.tree.root_node();
    let (node, sig) = find_procedure_node(&root, source, name)?;
    let code = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
    Some((code, sig))
}

// ---------------------------------------------------------------------------
// Outline rendering — delegates to al_symbols::virtual_file::render_outline
// ---------------------------------------------------------------------------

/// Render a complete outline from a SymbolEntry.
///
/// Delegates to [`al_symbols::virtual_file::render_outline`] which produces
/// valid AL syntax with full procedure signatures, fields, keys, enum values,
/// event declarations with attributes, and global variables.
pub fn render_outline(entry: &SymbolEntry) -> String {
    al_symbols::virtual_file::render_outline(entry)
}

/// Render a method signature string.
pub fn render_method_signature(m: &MethodSymbol) -> String {
    let params: Vec<String> = m.parameters.iter().map(|p| p.to_string()).collect();

    let mut sig = format!("procedure {}({})", m.name, params.join("; "));
    if let Some(ref ret) = m.return_type {
        sig.push_str(&format!(": {}", ret));
    }
    sig
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_table_entry() -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "SetFilter".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "FilterStr".to_string(),
                        type_name: "Text".to_string(),
                        is_var: false,
                    }],
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                },
                MethodSymbol {
                    name: "GetBalance".to_string(),
                    parameters: Vec::new(),
                    return_type: Some("Decimal".to_string()),
                    attributes: Vec::new(),
                    is_local: false,
                },
            ],
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: vec![],
                },
                FieldSymbol {
                    id: 2,
                    name: "Name".to_string(),
                    type_name: "Text[100]".to_string(),
                    properties: vec![],
                },
            ],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: vec![KeySymbol {
                name: "PK".to_string(),
                field_names: vec!["No.".to_string()],
                properties: vec![],
            }],
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_enum_entry() -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Enum,
            id: 50100,
            name: "Sales Document Type".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: vec![
                EnumValueSymbol { ordinal: 0, name: "Quote".to_string() },
                EnumValueSymbol { ordinal: 1, name: "Order".to_string() },
                EnumValueSymbol { ordinal: 2, name: "Invoice".to_string() },
                EnumValueSymbol { ordinal: 3, name: "Credit Memo".to_string() },
            ],
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_codeunit_with_events() -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "PostSalesDocument".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                },
                MethodSymbol {
                    name: "OnAfterPost".to_string(),
                    parameters: vec![
                        ParameterSymbol {
                            name: "SalesHeader".to_string(),
                            type_name: "Record \"Sales Header\"".to_string(),
                            is_var: false,
                        },
                    ],
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "IntegrationEvent".to_string(),
                        arguments: vec!["false".to_string(), "false".to_string()],
                    }],
                    is_local: false,
                },
                MethodSymbol {
                    name: "ValidateHeader".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: Some("Boolean".to_string()),
                    attributes: Vec::new(),
                    is_local: true,
                },
            ],
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: vec![VariableSymbol {
                name: "TotalAmount".to_string(),
                type_name: "Decimal".to_string(),
                is_protected: false,
            }],
        }
    }

    fn make_table_ext_entry() -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::TableExtension,
            id: 50100,
            name: "Customer Ext".to_string(),
            extends: Some("Customer".to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "My Extension".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 50100,
                name: "Custom Field".to_string(),
                type_name: "Boolean".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn render_outline_table() {
        let entry = make_table_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("table 18 Customer\n{\n"));
        assert!(outline.contains("field(1; \"No.\"; Code[20]) { }"));
        assert!(outline.contains("field(2; Name; Text[100]) { }"));
        assert!(outline.contains("key(PK; No.)"));
        assert!(outline.contains("procedure SetFilter(FilterStr: Text)"));
        assert!(outline.contains("procedure GetBalance(): Decimal"));
        assert!(outline.ends_with("}\n"));
    }

    #[test]
    fn render_outline_enum() {
        let entry = make_enum_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("enum 50100 \"Sales Document Type\"\n{\n"));
        assert!(outline.contains("value(0; Quote) { }"));
        assert!(outline.contains("value(1; Order) { }"));
        assert!(outline.contains("value(3; \"Credit Memo\") { }"));
    }

    #[test]
    fn render_outline_codeunit_with_events() {
        let entry = make_codeunit_with_events();
        let outline = render_outline(&entry);

        assert!(outline.contains("codeunit 80 \"Sales-Post\""));
        // Non-local procedure
        assert!(outline.contains("    procedure PostSalesDocument(var SalesHeader: Record \"Sales Header\")"));
        // Local procedure
        assert!(outline.contains("    local procedure ValidateHeader(var SalesHeader: Record \"Sales Header\"): Boolean"));
        // Event attribute
        assert!(outline.contains("[IntegrationEvent(false, false)]"));
        // Variables
        assert!(outline.contains("TotalAmount: Decimal"));
    }

    #[test]
    fn render_outline_extension() {
        let entry = make_table_ext_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("tableextension 50100 \"Customer Ext\" extends Customer\n{\n"));
        assert!(outline.contains("field(50100; \"Custom Field\"; Boolean) { }"));
    }

    #[test]
    fn render_method_signature_with_params() {
        let method = MethodSymbol {
            name: "PostDocument".to_string(),
            parameters: vec![
                ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                },
                ParameterSymbol {
                    name: "Preview".to_string(),
                    type_name: "Boolean".to_string(),
                    is_var: false,
                },
            ],
            return_type: Some("Boolean".to_string()),
            attributes: Vec::new(),
            is_local: false,
        };

        let sig = render_method_signature(&method);
        assert_eq!(
            sig,
            "procedure PostDocument(var SalesHeader: Record \"Sales Header\"; Preview: Boolean): Boolean"
        );
    }

    #[test]
    fn render_method_signature_no_params_no_return() {
        let method = MethodSymbol {
            name: "OnRun".to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        };

        let sig = render_method_signature(&method);
        assert_eq!(sig, "procedure OnRun()");
    }

    #[test]
    fn extract_signature_from_simple_procedure() {
        let text = "procedure DoWork(x: Integer)\nvar\n    y: Text;\nbegin\nend;";
        let sig = extract_signature_from_text(text);
        assert_eq!(sig, "procedure DoWork(x: Integer)");
    }

    #[test]
    fn extract_signature_with_return_type() {
        let text = "procedure GetValue(): Decimal\nbegin\nend;";
        let sig = extract_signature_from_text(text);
        assert_eq!(sig, "procedure GetValue(): Decimal");
    }

    #[test]
    fn render_outline_empty_object() {
        let entry = SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: "Empty CU".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "pkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };

        let outline = render_outline(&entry);
        assert_eq!(outline, "codeunit 50100 \"Empty CU\"\n{\n}\n");
    }

    #[test]
    fn source_level_serialization() {
        assert_eq!(
            serde_json::to_string(&SourceLevel::Workspace).unwrap(),
            "\"workspace\""
        );
        assert_eq!(
            serde_json::to_string(&SourceLevel::Package).unwrap(),
            "\"package\""
        );
        assert_eq!(
            serde_json::to_string(&SourceLevel::Outline).unwrap(),
            "\"outline\""
        );
    }
}
