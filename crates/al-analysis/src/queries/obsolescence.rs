//! Obsolescence timeline query.
//!
//! Reports everything an extension has marked obsolete, with its projected
//! removal timeline and caller count.
//!
//! AL spells obsolescence two ways, and both are read here. Objects and their
//! named elements carry the `ObsoleteState`, `ObsoleteReason` and `ObsoleteTag`
//! *properties*; procedures, variables and other symbols carry the
//! `[Obsolete]` *attribute*. See "Obsolete objects, methods, and symbols in AL"
//! on Microsoft Learn.

use std::collections::HashMap;

use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ObsoleteState {
    Pending,
    Removed,
    /// Set while a table or field moves to another extension.
    Moved,
    PendingMove,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsoleteEntry {
    pub object: String,
    pub symbol: String,
    /// `"object"`, `"procedure"`, or the AL keyword of the element that
    /// carries the properties: `"field"`, `"key"`, `"value"` (an enum value),
    /// `"action"`, `"group"`, `"part"` and the rest of the page controls.
    pub kind: String,
    pub state: ObsoleteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-based line number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub caller_count: u32,
}

/// Whether the timeline counts callers.
///
/// Counting walks every workspace tree, which a caller that discards
/// `caller_count` should not pay for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallerCounts {
    Count,
    Skip,
}

pub fn obsolescence_timeline(
    workspace: &Workspace,
) -> Result<Vec<ObsoleteEntry>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    Ok(timeline_from_sources(
        workspace,
        &sources,
        CallerCounts::Count,
    ))
}

/// The timeline over a snapshot the caller already holds.
///
/// `obsolete_usages` runs inside `workspace_diagnostics`, which is scheduled on
/// every debounced change. It has its own snapshot and reads none of the
/// caller counts, so taking a second snapshot and walking every tree once per
/// obsolete symbol was work whose result it threw away.
pub(crate) fn timeline_from_sources(
    workspace: &Workspace,
    sources: &[crate::workspace_sources::WorkspaceSource],
    counts: CallerCounts,
) -> Vec<ObsoleteEntry> {
    let mut results = Vec::new();

    // One pass over each tree, rather than one pass per obsolete symbol: a
    // project with 2000 files and 50 obsolete procedures did 100,000 full tree
    // walks for a single query.
    let call_counts = match counts {
        CallerCounts::Count => Some(call_site_counts(sources)),
        CallerCounts::Skip => None,
    };

    for source in sources {
        scan_file_for_obsolete(
            &source.path.to_string_lossy(),
            &source.text,
            &source.tree,
            call_counts.as_ref(),
            &mut results,
        );
    }

    // Package objects. The workspace's own objects are scanned from source
    // above; their symbol-index copies would list them twice.
    let symbols = workspace.symbols.all_entries();
    let caller_count = |name: &str| {
        call_counts
            .as_ref()
            .and_then(|counts| counts.get(&name.to_ascii_lowercase()).copied())
            .unwrap_or(0)
    };
    for sym in symbols.iter().filter(|s| {
        !s.synthetic && !al_symbols::source_availability::is_workspace_package(&s.package)
    }) {
        // Objects and fields say it with properties. Only procedures were
        // read, so Base Application's 62 obsolete tables and 125 obsolete
        // fields never appeared.
        if let Some(entry) = property_entry(&sym.properties, &sym.name, &sym.name, "object") {
            results.push(entry);
        }
        for field in &sym.fields {
            if let Some(entry) = property_entry(&field.properties, &sym.name, &field.name, "field")
            {
                results.push(entry);
            }
        }
        for method in &sym.methods {
            for attr in &method.attributes {
                if attr.name.eq_ignore_ascii_case("Obsolete") {
                    let reason = attr
                        .arguments
                        .first()
                        .cloned()
                        .map(|s| s.trim_matches('\'').trim_matches('"').to_string());
                    let tag = attr
                        .arguments
                        .get(1)
                        .cloned()
                        .map(|s| s.trim_matches('\'').trim_matches('"').to_string());
                    results.push(ObsoleteEntry {
                        object: sym.name.clone(),
                        symbol: method.name.clone(),
                        kind: "procedure".to_string(),
                        state: ObsoleteState::Pending,
                        reason,
                        tag,
                        file: None,
                        line: None,
                        // By name, as for the workspace's own procedures.
                        caller_count: caller_count(&method.name),
                    });
                }
            }
        }
    }

    results
}

/// Call-site name (lowercased) to the number of calls across every file.
fn call_site_counts(sources: &[crate::workspace_sources::WorkspaceSource]) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for source in sources {
        for (name, _) in al_syntax::collect_call_sites(&source.tree, &source.text) {
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
}

fn caller_count(counts: Option<&HashMap<String, u32>>, name: &str) -> u32 {
    counts
        .and_then(|counts| counts.get(&name.to_ascii_lowercase()))
        .copied()
        .unwrap_or(0)
}

fn scan_file_for_obsolete(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    counts: Option<&HashMap<String, u32>>,
    results: &mut Vec<ObsoleteEntry>,
) {
    scan_node(
        file_tree.root_node(),
        file_text.as_bytes(),
        file_path,
        "",
        counts,
        results,
    );
}

/// Walk one file, reporting every declaration that carries an obsoletion.
///
/// `object_name` is the object enclosing `node`, tracked as the walk descends
/// so a file holding several objects attributes each element to its own.
fn scan_node(
    node: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    counts: Option<&HashMap<String, u32>>,
    results: &mut Vec<ObsoleteEntry>,
) {
    let mut object_name = object_name;
    let owned_object_name;
    if node.kind() == "object_declaration" {
        owned_object_name = node
            .child_by_field_name("name")
            .and_then(|name| al_syntax::node_text_clean(name, source))
            .unwrap_or_default();
        object_name = &owned_object_name;
    }

    if let Some((kind, symbol, obsoletion)) = declared_obsoletion(node, source, object_name) {
        results.push(ObsoleteEntry {
            object: object_name.to_string(),
            caller_count: caller_count(counts, &symbol),
            symbol,
            kind,
            state: obsoletion.state,
            reason: obsoletion.reason,
            tag: obsoletion.tag,
            file: Some(file_path.to_string()),
            line: Some(node.start_position().row as u32 + 1),
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_node(child, source, file_path, object_name, counts, results);
    }
}

/// What a declaration marks obsolete: `(kind, symbol name, obsoletion)`.
fn declared_obsoletion(
    node: tree_sitter::Node,
    source: &[u8],
    object_name: &str,
) -> Option<(String, String, Obsoletion)> {
    match node.kind() {
        "procedure_declaration" | "event_procedure_declaration" => {
            let obsoletion = obsoletion_from_attribute(node, source)?;
            let name = al_syntax::node_name_or(node, source, "(unknown)");
            Some(("procedure".to_string(), name, obsoletion))
        }
        "object_declaration" => {
            let obsoletion = obsoletion_from_properties(node, source)?;
            Some(("object".to_string(), object_name.to_string(), obsoletion))
        }
        "key_declaration" => {
            let obsoletion = obsoletion_from_properties(node, source)?;
            let name = al_syntax::node_name_or(node, source, "(unknown)");
            Some(("key".to_string(), name, obsoletion))
        }
        "enum_value_declaration" => {
            let obsoletion = obsoletion_from_properties(node, source)?;
            let name = al_syntax::node_name_or(node, source, "(unknown)");
            Some(("value".to_string(), name, obsoletion))
        }
        // A table field, enum value, page control or action: the AL keyword
        // that opens the section is the element kind.
        "object_section" => {
            let obsoletion = obsoletion_from_properties(node, source)?;
            let keyword = node
                .child_by_field_name("keyword")?
                .utf8_text(source)
                .ok()?
                .to_ascii_lowercase();
            let name = al_syntax::node_text_clean(section_header_name(node)?, source)?;
            Some((keyword, name, obsoletion))
        }
        _ => None,
    }
}

/// The name token in an `object_section`'s parenthesized header, skipping a
/// leading id (`field(2; "Posting Date"; Date)`, `value(0; Open)`).
fn section_header_name(section: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut cursor = section.walk();
    let header = section
        .children(&mut cursor)
        .find(|child| child.kind() == "parenthesized_block")?;
    let mut header_cursor = header.walk();
    let mut seen_id = false;
    for child in header.named_children(&mut header_cursor) {
        match child.kind() {
            "integer" if !seen_id => seen_id = true,
            "semicolon" | "comma" if seen_id => {}
            "identifier" | "quoted_identifier" | "name" | "name_or_keyword" => return Some(child),
            _ => return None,
        }
    }
    None
}

struct Obsoletion {
    state: ObsoleteState,
    reason: Option<String>,
    tag: Option<String>,
}

/// Read `ObsoleteState`, `ObsoleteReason` and `ObsoleteTag` from a
/// declaration's own property block.
///
/// Only the block's direct children are read, so a table's properties are not
/// mistaken for its fields' and an enclosing object's obsoletion is not
/// reported again for every element inside it.
fn obsoletion_from_properties(node: tree_sitter::Node, source: &[u8]) -> Option<Obsoletion> {
    // `enum_value_declaration` carries its block as a plain child rather than
    // on a `body` field.
    let body = node.child_by_field_name("body").or_else(|| {
        let mut cursor = node.walk();
        let found = node
            .children(&mut cursor)
            .find(|c| c.kind() == "object_body");
        found
    })?;
    let mut state = None;
    let mut reason = None;
    let mut tag = None;
    let mut cursor = body.walk();
    for property in body.children(&mut cursor) {
        if property.kind() != "property_assignment" {
            continue;
        }
        let Some(name) = property
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
        else {
            continue;
        };
        let value = property
            .child_by_field_name("value")
            .and_then(|n| property_value_text(n, source));
        match name.trim().to_ascii_lowercase().as_str() {
            "obsoletestate" => state = value.as_deref().map(parse_obsolete_state),
            "obsoletereason" => reason = value.filter(|value| !value.is_empty()),
            "obsoletetag" => tag = value.filter(|value| !value.is_empty()),
            _ => {}
        }
    }
    // `ObsoleteState = No` is the default: the declaration is not obsolete.
    state?.map(|state| Obsoletion { state, reason, tag })
}

/// A property value as the developer wrote it: an AL string literal with its
/// `'` delimiters removed and `''` unescaped, or a bare word as-is.
///
/// Trimming quote characters off both ends instead would cut the closing `"`
/// off a reason such as `'Replaced by "Line Discount Amount"'`.
fn property_value_text(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?.trim();
    if node.kind() == "string" && text.len() >= 2 && text.starts_with('\'') && text.ends_with('\'')
    {
        return Some(text[1..text.len() - 1].replace("''", "'"));
    }
    Some(text.to_string())
}

/// A package object's or field's obsolescence from its `ObsoleteState`,
/// `ObsoleteReason` and `ObsoleteTag` properties; `None` when it is not
/// obsolete.
fn property_entry(
    properties: &[al_symbols::PropertyValue],
    object: &str,
    symbol: &str,
    kind: &str,
) -> Option<ObsoleteEntry> {
    let property = |name: &str| {
        properties
            .iter()
            .find(|property| property.name.eq_ignore_ascii_case(name))
            .map(|property| property.value.trim().to_string())
    };
    let state = parse_obsolete_state(&property("ObsoleteState")?)?;
    Some(ObsoleteEntry {
        object: object.to_string(),
        symbol: symbol.to_string(),
        kind: kind.to_string(),
        state,
        reason: property("ObsoleteReason"),
        tag: property("ObsoleteTag"),
        file: None,
        line: None,
        caller_count: 0,
    })
}

/// The `ObsoleteState` values Microsoft Learn documents. `No` is the default
/// and means the declaration is not obsolete, hence the nested `Option`.
fn parse_obsolete_state(value: &str) -> Option<ObsoleteState> {
    match value.trim().to_ascii_lowercase().as_str() {
        "no" => None,
        "pending" => Some(ObsoleteState::Pending),
        "removed" => Some(ObsoleteState::Removed),
        "moved" => Some(ObsoleteState::Moved),
        "pendingmove" => Some(ObsoleteState::PendingMove),
        _ => Some(ObsoleteState::Unknown),
    }
}

/// The `[Obsolete('<reason>', '<tag>')]` attribute on a procedure.
///
/// A procedure carries no `ObsoleteState`, so the attribute alone marks it
/// pending removal.
fn obsoletion_from_attribute(node: tree_sitter::Node, source: &[u8]) -> Option<Obsoletion> {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" {
            if let Ok(text) = s.utf8_text(source) {
                if let Some(result) = parse_obsolete_attr(text) {
                    return Some(result);
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" {
            if let Ok(text) = child.utf8_text(source) {
                if let Some(result) = parse_obsolete_attr(text) {
                    return Some(result);
                }
            }
        }
    }
    None
}

fn parse_obsolete_attr(text: &str) -> Option<Obsoletion> {
    let lower = text.to_lowercase();
    if !lower.trim_start().starts_with("[obsolete") && !lower.contains("obsolete(") {
        return None;
    }
    Some(Obsoletion {
        state: ObsoleteState::Pending,
        reason: extract_attr_arg(text, 0),
        tag: extract_attr_arg(text, 1),
    })
}

fn extract_attr_arg(text: &str, idx: usize) -> Option<String> {
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    if start >= end {
        return None;
    }
    let inner = &text[start + 1..end];

    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_q = false;
    let mut qc = '\'';
    for ch in inner.chars() {
        match ch {
            '\'' | '"' if !in_q => {
                in_q = true;
                qc = ch;
            }
            c if c == qc && in_q => {
                in_q = false;
            }
            ',' if !in_q => {
                args.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }

    args.get(idx)
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn finds_obsolete_procedure() {
        let ws = workspace_with(vec![(
            "/src/Legacy.al",
            r#"codeunit 50100 "Legacy"
{
    [Obsolete('Use NewProc instead', '24.0')]
    procedure OldProc()
    begin
    end;

    procedure NewProc()
    begin
    end;
}"#,
        )]);

        let entries = obsolescence_timeline(&ws).unwrap();
        assert!(
            entries.iter().any(|e| e.symbol == "OldProc"),
            "Should find OldProc as obsolete: {:?}",
            entries
        );
    }

    /// AL marks an object obsolete with properties in its body, not with an
    /// attribute. The scan only read attributes, so no real AL object was ever
    /// reported.
    #[test]
    fn finds_obsolete_object_properties() {
        let ws = workspace_with(vec![(
            "/src/OldBuffer.al",
            r#"table 50100 "Old Shipment Buffer"
{
    ObsoleteState = Pending;
    ObsoleteReason = 'Use table 50101 instead';
    ObsoleteTag = '24.0';

    fields
    {
        field(1; "No."; Code[20]) { }
    }
}"#,
        )]);

        let entries = obsolescence_timeline(&ws).unwrap();
        let entry = entries
            .iter()
            .find(|e| e.kind == "object")
            .unwrap_or_else(|| panic!("no object entry: {entries:?}"));
        assert_eq!(entry.symbol, "Old Shipment Buffer");
        assert_eq!(entry.state, ObsoleteState::Pending);
        assert_eq!(entry.reason.as_deref(), Some("Use table 50101 instead"));
        assert_eq!(entry.tag.as_deref(), Some("24.0"));
    }

    /// An obsolete field is the most common obsolescence in Business Central,
    /// because it is the one that forces data migration.
    #[test]
    fn finds_obsolete_field_key_and_enum_value() {
        let ws = workspace_with(vec![
            (
                "/src/Shipment.al",
                r#"table 50100 "Shipment"
{
    fields
    {
        field(5; "Discount Amount"; Decimal)
        {
            ObsoleteState = Removed;
            ObsoleteReason = 'Replaced by "Line Discount Amount"';
            ObsoleteTag = '23.0';
        }
        field(6; "Line Discount Amount"; Decimal) { }
    }
    keys
    {
        key(Discount; "Discount Amount")
        {
            ObsoleteState = Pending;
            ObsoleteTag = '23.0';
        }
    }
}"#,
            ),
            (
                "/src/Status.al",
                r#"enum 50100 "Shipment Status"
{
    value(0; Open) { }
    value(1; Held)
    {
        ObsoleteState = Pending;
        ObsoleteReason = 'Use Blocked';
    }
}"#,
            ),
        ]);

        let entries = obsolescence_timeline(&ws).unwrap();
        let field = entries
            .iter()
            .find(|e| e.kind == "field")
            .unwrap_or_else(|| panic!("no field entry: {entries:?}"));
        assert_eq!(field.symbol, "Discount Amount");
        assert_eq!(field.state, ObsoleteState::Removed);
        assert_eq!(field.object, "Shipment");
        assert_eq!(field.tag.as_deref(), Some("23.0"));

        let key = entries
            .iter()
            .find(|e| e.kind == "key")
            .unwrap_or_else(|| panic!("no key entry: {entries:?}"));
        assert_eq!(key.symbol, "Discount");
        assert_eq!(key.state, ObsoleteState::Pending);

        let value = entries
            .iter()
            .find(|e| e.kind == "value")
            .unwrap_or_else(|| panic!("no enum value entry: {entries:?}"));
        assert_eq!(value.symbol, "Held");
        assert_eq!(value.object, "Shipment Status");
    }

    /// Page controls and actions carry the same properties.
    #[test]
    fn finds_obsolete_page_control_and_action() {
        let ws = workspace_with(vec![(
            "/src/ShipmentCard.al",
            r#"page 50100 "Shipment Card"
{
    layout
    {
        area(Content)
        {
            field(Discount; Rec."Discount Amount")
            {
                ObsoleteState = Pending;
                ObsoleteReason = 'The field is going away';
            }
        }
    }
    actions
    {
        area(Processing)
        {
            group(Posting)
            {
                action(Post)
                {
                    ObsoleteState = Pending;
                    ObsoleteTag = '24.0';
                }
            }
        }
    }
}"#,
        )]);

        let entries = obsolescence_timeline(&ws).unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.kind == "field" && e.symbol == "Discount"),
            "no page control entry: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.kind == "action" && e.symbol == "Post"),
            "no action entry: {entries:?}"
        );
    }

    /// `ObsoleteState = Removed` used to be classified Pending whenever the
    /// word "pending" appeared anywhere in the declaration's text.
    #[test]
    fn removed_is_not_read_as_pending() {
        let ws = workspace_with(vec![(
            "/src/Gone.al",
            r#"table 50100 "Gone"
{
    ObsoleteState = Removed;
    ObsoleteReason = 'Pending removal was announced in 24.0';
    ObsoleteTag = '25.0';
}"#,
        )]);

        let entries = obsolescence_timeline(&ws).unwrap();
        let entry = entries.iter().find(|e| e.kind == "object").expect("object");
        assert_eq!(entry.state, ObsoleteState::Removed);
    }

    /// `No` is the default, so it must not produce an entry.
    /// Package objects and fields mark obsolescence with properties, and a
    /// package procedure's callers are counted like the workspace's own.
    #[test]
    fn package_objects_and_fields_with_obsolete_state_are_listed() {
        let workspace = Workspace::new();
        let property = |name: &str, value: &str| al_symbols::PropertyValue {
            name: name.to_string(),
            value: value.to_string(),
        };
        workspace.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 5050,
            name: "Old Setup".to_string(),
            package: "Base Application".to_string(),
            properties: vec![
                property("ObsoleteState", "Removed"),
                property("ObsoleteTag", "22.0"),
            ],
            fields: vec![al_symbols::FieldSymbol {
                id: 2,
                name: "Home Page".to_string(),
                type_name: "Text[80]".to_string(),
                properties: vec![
                    property("ObsoleteState", "Pending"),
                    property("ObsoleteReason", "Field length will be increased to 255."),
                ],
            }],
            ..Default::default()
        }]);

        let entries = obsolescence_timeline(&workspace).unwrap();

        let object = entries
            .iter()
            .find(|e| e.kind == "object")
            .expect("object row");
        assert_eq!(object.state, ObsoleteState::Removed);
        assert_eq!(object.tag.as_deref(), Some("22.0"));
        let field = entries
            .iter()
            .find(|e| e.kind == "field")
            .expect("field row");
        assert_eq!(field.symbol, "Home Page");
        assert_eq!(field.state, ObsoleteState::Pending);
    }

    #[test]
    fn obsolete_state_no_is_not_obsolete() {
        let ws = workspace_with(vec![(
            "/src/Active.al",
            r#"table 50100 "Active"
{
    ObsoleteState = No;

    fields
    {
        field(1; "No."; Code[20]) { }
    }
}"#,
        )]);

        assert!(obsolescence_timeline(&ws).unwrap().is_empty());
    }

    /// The counts are the ones the per-symbol tree walks produced.
    #[test]
    fn caller_counts_come_from_one_pass_per_file() {
        let ws = workspace_with(vec![
            (
                "/src/Legacy.al",
                r#"codeunit 50100 "Legacy"
{
    [Obsolete('Use NewProc instead', '24.0')]
    procedure OldProc()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50101 "Caller"
{
    trigger OnRun()
    var
        Legacy: Codeunit "Legacy";
    begin
        Legacy.OldProc();
        Legacy.OldProc();
    end;
}"#,
            ),
        ]);

        let entry = obsolescence_timeline(&ws)
            .unwrap()
            .into_iter()
            .find(|e| e.symbol == "OldProc")
            .expect("OldProc");
        assert_eq!(entry.caller_count, 2);
    }

    #[test]
    fn non_obsolete_not_reported() {
        let ws = workspace_with(vec![(
            "/src/Current.al",
            r#"codeunit 50100 "Current"
{
    procedure ActiveProc()
    begin
    end;
}"#,
        )]);

        let entries = obsolescence_timeline(&ws).unwrap();
        assert!(
            !entries.iter().any(|e| e.symbol == "ActiveProc"),
            "ActiveProc should not be flagged: {:?}",
            entries
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let entries = obsolescence_timeline(&ws).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn finds_obsolete_method_in_symbol_package() {
        // Exercise the symbol-package scan branch and attribute trimming.
        // Build a SymbolEntry with an Obsolete-marked method and confirm
        // it surfaces, with reason and tag arguments correctly trimmed of
        // surrounding quotes.
        use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, ParameterSymbol, SymbolEntry};
        let ws = Workspace::new();
        let entries = vec![SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: "Legacy CU".to_string(),
            package: "TestPkg".to_string(),
            methods: vec![MethodSymbol {
                name: "OldHelper".to_string(),
                parameters: vec![ParameterSymbol {
                    name: "Amount".to_string(),
                    type_name: "Decimal".to_string(),
                    is_var: false,
                }],
                return_type: None,
                attributes: vec![AttributeSymbol {
                    name: "Obsolete".to_string(),
                    arguments: vec![
                        "'Use NewHelper instead'".to_string(),
                        "\"24.0\"".to_string(),
                    ],
                }],
                is_local: false,
            }],
            ..Default::default()
        }];
        ws.symbols.add_entries(&entries);

        let report = obsolescence_timeline(&ws).unwrap();
        let entry = report
            .iter()
            .find(|e| e.symbol == "OldHelper")
            .expect("symbol-package scan must find the Obsolete method");
        assert_eq!(entry.object, "Legacy CU");
        assert_eq!(entry.kind, "procedure");
        assert_eq!(entry.reason.as_deref(), Some("Use NewHelper instead"));
        assert_eq!(entry.tag.as_deref(), Some("24.0"));
    }
}
