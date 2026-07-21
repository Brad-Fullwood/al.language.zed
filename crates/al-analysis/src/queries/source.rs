//! Source extraction query — `al source`.
//!
//! Returns source code for an AL object, optionally filtered to a specific
//! procedure or trigger. Three source levels:
//! - `workspace`: full source from .al file, tree-sitter range for procedures
//! - `package`: source extracted from .app ZIP archive
//! - `outline`: rendered from SymbolReference.json (full signatures, no bodies)

use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry};
use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLevel {
    Workspace,
    Package,
    Outline,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    pub k: ObjectKind,
    pub id: i32,
    /// Object name.
    pub n: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proc_name: Option<String>,
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

pub fn source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    proc_filter: Option<&str>,
    trigger_filter: Option<&str>,
) -> Option<SourceResult> {
    if let Some(result) =
        try_workspace_source(workspace, name, kind_filter, proc_filter, trigger_filter)
    {
        return Some(result);
    }

    try_package_source(workspace, name, kind_filter, proc_filter, trigger_filter)
}

fn try_workspace_source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    proc_filter: Option<&str>,
    trigger_filter: Option<&str>,
) -> Option<SourceResult> {
    let file_path = workspace.file_index.find_by_object_name(name)?;

    // SILENT: non-absolute paths can't become file URIs
    let uri = url::Url::from_file_path(&file_path).ok()?;
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, &uri)?;

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
        let root = tree.root_node();
        if let Some((node, sig)) = find_procedure_node(&root, &text, member_name) {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            let code = node.utf8_text(text.as_bytes()).unwrap_or("").to_string();

            let relative_path = file_path
                .file_name()
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

    if let Some(ref path) = app_path {
        if let Ok(source_index) = al_symbols::source_index::get_or_build(path) {
            if let Some(full_source) = source_index.extract_source_for_entry(entry) {
                let member_filter = proc_filter.or(trigger_filter);
                if let Some(member_name) = member_filter {
                    if let Some((code, sig)) =
                        extract_procedure_from_text(&full_source, member_name)
                    {
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
        let method = entry
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(member_name))?;
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
            note: Some(
                "Rendered from symbol metadata — signature only, no implementation body"
                    .to_string(),
            ),
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
        note: Some(
            "Rendered from symbol metadata — full signatures and fields, no implementation bodies"
                .to_string(),
        ),
    })
}

/// Find a procedure/trigger node in a tree-sitter tree and return (node, signature).
///
/// Iterative tree-sitter traversal avoids stack overflow on deeply nested AL.
fn find_procedure_node<'a>(
    root: &'a tree_sitter::Node<'a>,
    source: &str,
    name: &str,
) -> Option<(tree_sitter::Node<'a>, String)> {
    let mut stack: Vec<tree_sitter::Node<'a>> = vec![*root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == "procedure_declaration" || kind == "trigger_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                let node_name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let clean = node_name.trim_matches('"');
                if clean.eq_ignore_ascii_case(name) {
                    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
                    let sig = extract_signature_from_text(text);
                    return Some((node, sig));
                }
            }
        }
        // Push children in reverse so leftmost child is processed first (preserves
        // original pre-order traversal semantics).
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

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
                    let rest = &text[end..];
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
        text.lines().next().unwrap_or(text).to_string()
    } else {
        text[..end].trim().to_string()
    }
}

fn extract_procedure_from_text(source: &str, name: &str) -> Option<(String, String)> {
    let result = al_syntax::AlParser::parse_quick(source);
    let root = result.tree.root_node();
    let (node, sig) = find_procedure_node(&root, source, name)?;
    let code = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
    Some((code, sig))
}

/// Render a complete outline from a SymbolEntry.
///
/// Delegates to [`al_symbols::virtual_file::render_outline`] which produces
/// valid AL syntax with full procedure signatures, fields, keys, enum values,
/// event declarations with attributes, and global variables.
pub fn render_outline(entry: &SymbolEntry) -> String {
    al_symbols::virtual_file::render_outline(entry)
}

pub fn render_method_signature(m: &MethodSymbol) -> String {
    let params: Vec<String> = m.parameters.iter().map(|p| p.to_string()).collect();

    let mut sig = format!("procedure {}({})", m.name, params.join("; "));
    if let Some(ref ret) = m.return_type {
        sig.push_str(&format!(": {}", ret));
    }
    sig
}

// ---------------------------------------------------------------------------
// Event-source resolution — `al-explorer event-source`
// ---------------------------------------------------------------------------

/// Result of resolving the publisher behind an `[EventSubscriber(...)]`
/// attribute at a cursor position.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSourceResult {
    /// Publisher object kind from the attribute's `ObjectType::` argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<ObjectKind>,
    pub target_object: String,
    pub target_event: String,
    /// Resolved publisher declaration file — a workspace `.al` file or a
    /// virtual file materialised from a symbol package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 1-based line of the event declaration within `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The declaration line text (trimmed), as a human-readable signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// True when `path` is a virtual file extracted from a `.app` package.
    pub from_package: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Resolve the actual event publisher for the `[EventSubscriber(...)]`
/// attribute at `line_1based` in `file`.
///
/// "Show Event Source" previously ran a name substring search and
/// returned a pile of unrelated matches. This resolves the attribute's
/// `(ObjectType, Object, EventName)` triple to the publisher's declaration
/// — in the workspace when possible, otherwise materialised from the
/// symbol package. Positions without a subscriber attribute get a
/// clear error instead of garbage results.
pub fn event_source(
    workspace: &Workspace,
    file: &std::path::Path,
    line_1based: u32,
) -> Result<EventSourceResult, String> {
    let (source_text, tree) = workspace
        .file_index
        .get_cached_parse(file)
        .or_else(|| {
            let text = std::fs::read_to_string(file).ok()?;
            let result = al_syntax::AlParser::parse_quick(&text);
            Some((text, result.tree))
        })
        .ok_or_else(|| format!("Cannot read or parse '{}'", file.display()))?;

    // Find the procedure/trigger declaration containing (or starting at)
    // the requested line. tree-sitter rows are 0-based.
    let target_row = line_1based.saturating_sub(1) as usize;
    let mut proc_node: Option<tree_sitter::Node> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "procedure_declaration" || child.kind() == "trigger_declaration" {
                let start = child.start_position().row;
                let end = child.end_position().row;
                if target_row >= start && target_row <= end {
                    proc_node = Some(child);
                }
            } else {
                stack.push(child);
            }
        }
    }
    let proc_node = proc_node.ok_or_else(|| {
        format!(
            "No procedure at {}:{} — place the cursor on an event subscriber \
             (the [EventSubscriber] attribute or its procedure) and re-run",
            file.display(),
            line_1based
        )
    })?;

    let attrs = al_insight::calls::collect_procedure_attributes(proc_node, source_text.as_bytes());
    let sub_attr = attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("EventSubscriber"))
        .ok_or_else(|| {
            format!(
                "The procedure at {}:{} has no [EventSubscriber] attribute — \
                 'Show Event Source' only applies to event subscribers",
                file.display(),
                line_1based
            )
        })?;

    let args = al_insight::calls::extract_attribute_args(&sub_attr.1);
    let target_kind = args.first().and_then(|a| {
        let a = a.trim();
        let type_str = a.rsplit("::").next().unwrap_or(a);
        type_str.parse::<ObjectKind>().ok()
    });
    let target_object = args
        .get(1)
        .map(|s| al_syntax::clean_attr_arg(s))
        .unwrap_or_default();
    let target_event = args
        .get(2)
        .map(|s| al_syntax::clean_attr_arg(s))
        .unwrap_or_default();
    if target_object.is_empty() || target_event.is_empty() {
        return Err(format!(
            "Could not parse the EventSubscriber attribute at {}:{}: '{}'",
            file.display(),
            line_1based,
            sub_attr.1
        ));
    }

    if let Some(pub_path) = workspace.file_index.find_by_object_name(&target_object) {
        if let Some((pub_src, pub_tree)) = workspace.file_index.get_cached_parse(&pub_path) {
            if let Some((decl_line, sig)) =
                find_procedure_decl_line(pub_tree.root_node(), &pub_src, &target_event)
            {
                return Ok(EventSourceResult {
                    target_kind,
                    target_object,
                    target_event,
                    path: Some(pub_path.to_string_lossy().into_owned()),
                    line: Some(decl_line),
                    signature: Some(sig),
                    from_package: false,
                    note: None,
                });
            }
        }
    }

    let mut candidates = workspace.symbols.get_by_name(&target_object);
    if let Some(kind) = target_kind {
        candidates.retain(|e| e.kind == kind);
    }
    // Prefer the entry that actually declares the event.
    candidates.sort_by_key(|e| {
        let has_event = e
            .methods
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(&target_event));
        if has_event {
            0
        } else {
            1
        }
    });
    if let Some(entry) = candidates.first() {
        let app_path = workspace.symbols.app_path(&entry.package);
        match al_symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
            Ok(vpath) => {
                let range = al_symbols::virtual_file::find_member_range(
                    &vpath,
                    &target_event,
                    al_symbols::virtual_file::MemberKind::Unknown,
                );
                let line = range.as_ref().map(|r| r.line + 1);
                let signature = entry
                    .methods
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(&target_event))
                    .map(render_method_signature);
                return Ok(EventSourceResult {
                    target_kind,
                    target_object,
                    target_event,
                    path: Some(vpath.to_string_lossy().into_owned()),
                    line,
                    signature,
                    from_package: true,
                    note: None,
                });
            }
            Err(e) => {
                return Ok(EventSourceResult {
                    target_kind,
                    target_object: target_object.clone(),
                    target_event,
                    path: None,
                    line: None,
                    signature: None,
                    from_package: true,
                    note: Some(format!(
                        "Publisher '{}' found in package '{}' but its source could not \
                         be materialised: {}",
                        target_object, entry.package, e
                    )),
                });
            }
        }
    }

    Ok(EventSourceResult {
        target_kind,
        target_object: target_object.clone(),
        target_event,
        path: None,
        line: None,
        signature: None,
        from_package: false,
        note: Some(format!(
            "Publisher '{}' not found in the workspace or any loaded symbol package — \
             check that symbols are downloaded",
            target_object
        )),
    })
}

/// Find a procedure/trigger declaration by name in a parsed tree; returns
/// (1-based line, trimmed declaration-line text).
fn find_procedure_decl_line(
    root: tree_sitter::Node,
    source: &str,
    name: &str,
) -> Option<(u32, String)> {
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "procedure_declaration" || child.kind() == "trigger_declaration" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(text) = name_node.utf8_text(source.as_bytes()) {
                        let clean = text.trim_matches('"');
                        if clean.eq_ignore_ascii_case(name) {
                            let row = name_node.start_position().row;
                            let sig = source
                                .lines()
                                .nth(row)
                                .map(|l| l.trim().to_string())
                                .unwrap_or_default();
                            return Some((row as u32 + 1, sig));
                        }
                    }
                }
            } else {
                stack.push(child);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_table_entry() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
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
            synthetic: false,
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
                EnumValueSymbol {
                    ordinal: 0,
                    name: "Quote".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 1,
                    name: "Order".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 2,
                    name: "Invoice".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 3,
                    name: "Credit Memo".to_string(),
                },
            ],
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_codeunit_with_events() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
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
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: false,
                    }],
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
            synthetic: false,
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
        assert!(outline
            .contains("    procedure PostSalesDocument(var SalesHeader: Record \"Sales Header\")"));
        assert!(outline.contains(
            "    local procedure ValidateHeader(var SalesHeader: Record \"Sales Header\"): Boolean"
        ));
        assert!(outline.contains("[IntegrationEvent(false, false)]"));
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
            synthetic: false,
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

    /// Regression for the recursive→iterative conversion of find_procedure_node:
    /// a deeply nested if-then-begin chain used to risk stack overflow under recursion.
    /// The iterative version must locate a procedure regardless of nesting depth.
    #[test]
    fn find_procedure_node_handles_deep_nesting() {
        const DEPTH: usize = 200;
        let mut body = String::new();
        for _ in 0..DEPTH {
            body.push_str("if true then begin\n");
        }
        body.push_str("Message('hi');\n");
        for _ in 0..DEPTH {
            body.push_str("end;\n");
        }
        let src = format!(
            "codeunit 50100 \"Deep\"\n{{\n    procedure Target()\n    begin\n        {}\n    end;\n}}\n",
            body
        );

        let parsed = al_syntax::AlParser::parse_quick(&src);
        let root = parsed.tree.root_node();
        let result = find_procedure_node(&root, &src, "Target");
        assert!(
            result.is_some(),
            "Target procedure should be found in deeply nested source"
        );
    }

    // -- extract_signature_from_text edge / boundary paths -----------------

    #[test]
    fn extract_signature_no_parens_falls_back_to_first_line() {
        // No '(' or ')' at all: `end` stays 0 and the function returns the
        // first line (fallback branch).
        let text = "trigger OnInsert\nbegin\nend;";
        let sig = extract_signature_from_text(text);
        assert_eq!(sig, "trigger OnInsert");
    }

    #[test]
    fn extract_signature_strips_trailing_semicolon_on_return_type() {
        // Return type on the same line, terminated by ';' — the ';' must be stripped.
        let text = "procedure GetValue(): Decimal;\nbegin\nend;";
        let sig = extract_signature_from_text(text);
        assert_eq!(sig, "procedure GetValue(): Decimal");
    }

    #[test]
    fn extract_signature_empty_input_returns_empty() {
        // No parens, no newline: lines().next() yields "" -> fallback returns "".
        let sig = extract_signature_from_text("");
        assert_eq!(sig, "");
    }

    #[test]
    fn extract_signature_no_return_type_after_close_paren() {
        // ')' closes the signature and the rest of the line has no ':'.
        let text = "procedure Foo(a: Integer) // comment\nbegin\nend;";
        let sig = extract_signature_from_text(text);
        assert_eq!(sig, "procedure Foo(a: Integer)");
    }

    // -- extract_procedure_from_text ---------------------------------------

    #[test]
    fn extract_procedure_from_text_finds_target() {
        let src = "codeunit 50100 \"Helper\"\n{\n    procedure Alpha()\n    begin\n    end;\n\n    procedure Beta(x: Integer): Boolean\n    begin\n        exit(true);\n    end;\n}\n";
        let (code, sig) = extract_procedure_from_text(src, "Beta").expect("Beta found");
        assert!(code.contains("procedure Beta(x: Integer): Boolean"));
        assert!(code.contains("exit(true)"));
        assert_eq!(sig, "procedure Beta(x: Integer): Boolean");
    }

    #[test]
    fn extract_procedure_from_text_missing_returns_none() {
        let src = "codeunit 50100 \"Helper\"\n{\n    procedure Alpha()\n    begin\n    end;\n}\n";
        assert!(extract_procedure_from_text(src, "DoesNotExist").is_none());
    }

    // -- find_procedure_node branches --------------------------------------

    #[test]
    fn find_procedure_node_matches_trigger_and_quoted_name_case_insensitive() {
        let src = "table 50100 \"My Tab\"\n{\n    trigger OnInsert()\n    begin\n    end;\n\n    procedure \"Do Work\"()\n    begin\n    end;\n}\n";
        let parsed = al_syntax::AlParser::parse_quick(src);
        let root = parsed.tree.root_node();

        assert!(find_procedure_node(&root, src, "oninsert").is_some());

        // Quoted procedure name: the surrounding quotes are trimmed before compare.
        assert!(find_procedure_node(&root, src, "Do Work").is_some());

        assert!(find_procedure_node(&root, src, "Nope").is_none());
    }

    // -- try_package_source via the public source() entrypoint -------------
    //
    // No app path is registered for the package, so source() falls through to
    // the SymbolReference.json outline-rendering branches.

    fn ws_with(entry: SymbolEntry) -> al_workspace::Workspace {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries_owned(vec![entry]);
        ws
    }

    #[test]
    fn source_outline_full_object() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, None, None).expect("found");

        assert_eq!(result.src, SourceLevel::Outline);
        assert_eq!(result.k, ObjectKind::Table);
        assert_eq!(result.id, 18);
        assert_eq!(result.pkg.as_deref(), Some("Base Application"));
        assert!(result.proc_name.is_none());
        assert!(result.sig.is_none());
        assert!(result.note.is_some());
        assert!(result.code.contains("table 18 Customer"));
        assert!(result.code.contains("procedure SetFilter"));
    }

    #[test]
    fn source_outline_procedure_filter_renders_signature_only() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, Some("GetBalance"), None).expect("found");

        assert_eq!(result.src, SourceLevel::Outline);
        assert_eq!(result.proc_name.as_deref(), Some("GetBalance"));
        // code == sig for outline procedure mode, and it is a bare signature.
        assert_eq!(result.code, "procedure GetBalance(): Decimal");
        assert_eq!(
            result.sig.as_deref(),
            Some("procedure GetBalance(): Decimal")
        );
        assert!(result.note.unwrap().contains("signature only"));
    }

    #[test]
    fn source_outline_procedure_filter_case_insensitive() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, Some("setfilter"), None).expect("found");
        assert_eq!(
            result.sig.as_deref(),
            Some("procedure SetFilter(FilterStr: Text)")
        );
    }

    #[test]
    fn source_outline_trigger_filter_used_when_no_proc_filter() {
        let ws = ws_with(make_table_entry());
        // trigger_filter is the fallback member filter; GetBalance is a method here.
        let result = source(&ws, "Customer", None, None, Some("GetBalance")).expect("found");
        assert_eq!(result.proc_name.as_deref(), Some("GetBalance"));
        assert_eq!(result.code, "procedure GetBalance(): Decimal");
    }

    #[test]
    fn source_outline_unknown_procedure_returns_none() {
        let ws = ws_with(make_table_entry());
        assert!(source(&ws, "Customer", None, Some("NoSuchMethod"), None).is_none());
    }

    #[test]
    fn source_kind_filter_mismatch_returns_none() {
        let ws = ws_with(make_table_entry());
        assert!(source(&ws, "Customer", Some(ObjectKind::Codeunit), None, None).is_none());
    }

    #[test]
    fn source_kind_filter_selects_matching_entry() {
        let ws = al_workspace::Workspace::new();
        // Two objects sharing the name "Item": a Table and a Codeunit.
        let mut table = make_table_entry();
        table.name = "Item".to_string();
        table.kind = ObjectKind::Table;
        table.id = 27;
        let codeunit = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 99,
            name: "Item".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        };
        ws.symbols.add_entries_owned(vec![table, codeunit]);

        let cu = source(&ws, "Item", Some(ObjectKind::Codeunit), None, None).expect("codeunit");
        assert_eq!(cu.k, ObjectKind::Codeunit);
        assert_eq!(cu.id, 99);

        let tbl = source(&ws, "Item", Some(ObjectKind::Table), None, None).expect("table");
        assert_eq!(tbl.k, ObjectKind::Table);
        assert_eq!(tbl.id, 27);
    }

    #[test]
    fn source_unknown_name_returns_none() {
        let ws = ws_with(make_table_entry());
        assert!(source(&ws, "DoesNotExist", None, None, None).is_none());
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
