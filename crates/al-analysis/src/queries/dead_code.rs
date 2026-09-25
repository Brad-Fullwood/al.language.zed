//! Dead code detection query.
//!
//! Cross-file analysis to find unused procedures, unreferenced table fields,
//! and orphaned event subscribers. Returns a list of `UnusedSymbol` entries
//! with the reason each symbol is considered dead.

use al_syntax::IdentifierText;
use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedKind {
    Procedure,
    Field,
    Subscriber,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedReason {
    /// No references found across any workspace files.
    ZeroReferences,
    /// Event subscriber targets a publisher that no longer exists.
    PublisherRemoved,
    /// The publisher object still exists but no longer declares the event the
    /// subscriber names (removed or renamed).
    EventRemoved,
}

/// How certain the analysis is that the symbol is genuinely dead.
///
/// Static analysis over workspace source cannot prove some symbols
/// dead — table fields are reachable via `FieldRef`/`RecordRef` by number,
/// report layouts, and other extensions; public procedures are callable
/// from any dependent extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// Provably unreachable within AL semantics (e.g. a `local` procedure
    /// with zero call sites in its own object, or a subscriber whose
    /// publisher no longer exists).
    High,
    /// No workspace references found, but the symbol is reachable through
    /// channels static analysis cannot see (other extensions, FieldRef by
    /// number, report layouts, the platform).
    Medium,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedSymbol {
    #[serde(rename = "k")]
    pub kind: UnusedKind,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "obj")]
    pub object: String,
    /// File path where the symbol is defined (if in workspace).
    #[serde(rename = "f", skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-based.
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub reason: UnusedReason,
    pub confidence: Confidence,
    /// For medium-confidence findings: why the symbol might still be live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn dead_code(workspace: &Workspace) -> Result<Vec<UnusedSymbol>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    let mut results = Vec::new();

    // The owned, sorted collection keeps borrows stable and output deterministic.
    let mut parsed_files: Vec<crate::workspace_sources::WorkspaceSource> = sources;
    parsed_files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut all_call_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 32);
    // Build member-access names once for constant-time field lookups.
    let mut all_member_access_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 16);
    for source in &parsed_files {
        all_call_names.extend(al_syntax::collect_call_site_names(
            &source.tree,
            &source.text,
        ));
        all_member_access_names.extend(al_syntax::collect_member_access_names(
            &source.tree,
            &source.text,
        ));
    }

    // Keep the query on the daemon dispatch thread. Rayon worker startup made
    // this small, latency-sensitive request hang indefinitely on Windows even
    // though the same parsed inputs complete immediately in serial. The
    // path-sorted input still makes output deterministic.
    let per_file_results: Vec<Vec<UnusedSymbol>> = parsed_files
        .iter()
        .map(|source| {
            let file_path = source.path.to_string_lossy();
            let file_text = source.text.as_str();
            let mut local = Vec::new();

            // Per object declaration, not per file: a file may hold several
            // objects, and walking from the root reported every one of their
            // unused members under the first object's name.
            for (object, node) in source.object_nodes() {
                let obj_info = &object.info;
                find_unused_procedures(
                    &file_path,
                    file_text,
                    node,
                    &obj_info.name,
                    &all_call_names,
                    &mut local,
                );

                let obj_kind_lower = obj_info.kind.to_lowercase();
                let is_table = al_syntax::language_data::object_type_by_keyword(&obj_kind_lower)
                    .map(|ot| ot.node_kind == "kw_table")
                    .unwrap_or(false);
                if is_table {
                    find_unused_fields(
                        &file_path,
                        file_text,
                        node,
                        &obj_info.name,
                        &all_member_access_names,
                        &mut local,
                    );
                }

                find_orphaned_subscribers(
                    &file_path,
                    file_text,
                    node,
                    &obj_info.name,
                    workspace,
                    &mut local,
                );
            }
            local
        })
        .collect();

    for v in per_file_results {
        results.extend(v);
    }

    Ok(results)
}

// Four pre-built lookup sets are distinct membership targets; bundling into one struct would obscure intent.
#[allow(clippy::too_many_arguments)]
fn find_unused_procedures(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    all_call_names: &std::collections::HashSet<String>,
    results: &mut Vec<UnusedSymbol>,
) {
    let source = file_text.as_bytes();
    let mut procs = Vec::new();

    collect_procedures(object, source, &mut procs);

    for (proc_name, is_event, line, is_local) in &procs {
        // Skip event publishers — they're entry points
        if *is_event {
            continue;
        }

        let lname = proc_name.to_ascii_lowercase();
        let referenced = all_call_names.contains(&lname);

        if !referenced {
            // Locality decides confidence. A `local` procedure with
            // zero call sites is provably dead; a public one may be called
            // by dependent extensions we can't see.
            let (confidence, note) = if *is_local {
                (Confidence::High, None)
            } else {
                (
                    Confidence::Medium,
                    Some(
                        "public procedure — may be called by other extensions \
                         or the platform; verify before removing"
                            .to_string(),
                    ),
                )
            };
            results.push(UnusedSymbol {
                kind: UnusedKind::Procedure,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
                confidence,
                note,
            });
        }
    }
}

fn collect_procedures(
    root: tree_sitter::Node,
    source: &[u8],
    procs: &mut Vec<(String, bool, u32, bool)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let name = al_syntax::clean_identifier(name);
                    let line = al_syntax::procedure_keyword_row(node)
                        .unwrap_or_else(|| node.start_position().row)
                        as u32
                        + 1;

                    let is_event = is_framework_invoked_procedure(node, source);

                    // `local`/`internal` procedures are unreachable from
                    // other extensions — locality drives the confidence of
                    // a zero-reference finding.
                    let is_local = node_has_local_modifier(node, source);

                    procs.push((name, is_event, line, is_local));
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn node_has_local_modifier(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Ok(text) = child.utf8_text(source) {
            let lower = text.to_ascii_lowercase();
            if lower == "local" || lower == "internal" {
                return true;
            }
            if lower == "procedure" {
                break;
            }
        }
    }
    false
}

/// True when the procedure is invoked by a framework, not by a direct AL call:
/// an event **publisher** (`[IntegrationEvent]`/`[BusinessEvent]`), an event
/// **subscriber** (`[EventSubscriber]` — dispatched by the event system, never
/// called directly), a **test** method (`[Test]`, runner-invoked), or a test
/// **handler** (`[…Handler]`, invoked by the test runtime). Such procedures have
/// zero textual call sites by design, so the unused-procedure pass must skip
/// them — otherwise it reports the single most common BC extension pattern
/// (subscriber codeunits) as provably dead. Genuinely-orphaned
/// subscribers are still surfaced by `find_orphaned_subscribers`.
fn is_framework_invoked_procedure(node: tree_sitter::Node, source: &[u8]) -> bool {
    fn attr_is_framework(text: &str) -> bool {
        // Extract each attribute's *name* (the token right after `[`, before any
        // `(` args or `]`) so a string argument that merely contains "test"
        // (e.g. `[Obsolete('use TestMethod')]`) can't cause a false skip.
        text.split('[').any(|seg| {
            let name = seg
                .split(|c: char| c == '(' || c == ']' || c == ',' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_lowercase();
            matches!(
                name.as_str(),
                "integrationevent"
                    | "businessevent"
                    | "eventsubscriber"
                    | "test"
                    | "testpermissions"
            ) || name.ends_with("handler")
        })
    }

    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" {
            if let Ok(text) = s.utf8_text(source) {
                if attr_is_framework(text) {
                    return true;
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check children (some grammars nest attributes inside the procedure node)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" {
            if let Ok(text) = child.utf8_text(source) {
                if attr_is_framework(text) {
                    return true;
                }
            }
        }
    }

    false
}

fn find_unused_fields(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    all_member_access_names: &std::collections::HashSet<String>,
    results: &mut Vec<UnusedSymbol>,
) {
    let fields = collect_table_fields(object, file_text);
    let local_primary_names = al_syntax::collect_primary_expression_names_under(object, file_text);

    for (field_name, line) in &fields {
        let field_key = field_name.to_ascii_lowercase();
        let referenced = all_member_access_names.contains(&field_key)
            || local_primary_names.contains(&field_key);

        if !referenced {
            // FieldRef/RecordRef, report layouts, and dependent extensions are
            // not visible to this analysis.
            results.push(UnusedSymbol {
                kind: UnusedKind::Field,
                name: field_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
                confidence: Confidence::Medium,
                note: Some(
                    "no AL name references, but fields can be read via \
                     FieldRef/RecordRef by number, report layouts, or other \
                     extensions — verify before removing"
                        .to_string(),
                ),
            });
        }
    }
}

fn collect_table_fields(object: tree_sitter::Node<'_>, text: &str) -> Vec<(String, u32)> {
    fn collect(symbols: &[al_syntax::SyntaxDocumentSymbol], fields: &mut Vec<(String, u32)>) {
        for symbol in symbols {
            if symbol.kind == al_syntax::SyntaxSymbolKind::Field
                && symbol.detail.as_deref() == Some("field")
            {
                fields.push((symbol.name.clone(), symbol.range.start.line + 1));
            }
            if let Some(children) = symbol.children.as_deref() {
                collect(children, fields);
            }
        }
    }

    let symbols = al_syntax::extract_document_symbols_under(object, text);
    let mut fields = Vec::new();
    collect(&symbols, &mut fields);
    fields
}

fn find_orphaned_subscribers(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    workspace: &Workspace,
    results: &mut Vec<UnusedSymbol>,
) {
    let source = file_text.as_bytes();
    let mut subscribers = Vec::new();

    collect_event_subscribers(object, source, &mut subscribers);

    for (proc_name, target_kind, target_object, target_event, line) in &subscribers {
        // Skip entries where attribute parsing failed to extract a target object name.
        // An empty target would cause false positives (nothing in the index matches "").
        if target_object.is_empty() {
            continue;
        }

        // `ObjectType::Codeunit, 80` names the publisher by ID: find the
        // object with that ID before asking whether it exists by name.
        let target_object = match target_object.trim().parse::<i32>() {
            Ok(id) => match publisher_by_id(workspace, target_kind.as_deref(), id) {
                Some(name) => name,
                None => target_object.clone(),
            },
            Err(_) => target_object.clone(),
        };

        // Check if the target object exists in the symbol index OR in workspace files.
        // Use get_by_name (exact, case-insensitive) rather than search (fuzzy substring)
        // to avoid false negatives where an unrelated symbol name contains the target
        // as a substring.
        let target_lower = target_object.to_lowercase();
        let exists_in_symbols = workspace.symbols.find_by_name(&target_lower).is_some();
        let exists_in_workspace = workspace
            .file_index
            .find_by_object_name(&target_lower)
            .is_some();

        if !exists_in_symbols && !exists_in_workspace {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::PublisherRemoved,
                confidence: Confidence::High,
                note: None,
            });
            continue;
        }

        // The publisher object exists — but does it still declare the event the
        // subscriber names? Comparing only the object name (as this used to)
        // never reported a subscriber to a removed or renamed event, despite
        // "orphaned subscribers" being the documented purpose of this check.
        if target_event.is_empty() {
            continue;
        }
        let Some(published) = publisher_event_names(workspace, &target_lower) else {
            // Cannot enumerate the publisher's events with confidence (e.g. a
            // table/page whose platform events are not declared in source):
            // stay silent rather than emit a false positive.
            continue;
        };
        if !published.contains(&target_event.to_lowercase()) {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::EventRemoved,
                confidence: Confidence::High,
                note: Some(format!(
                    "'{target_object}' no longer publishes '{target_event}'"
                )),
            });
        }
    }
}

/// Event names published by `object_lower`, or `None` when they cannot be
/// enumerated reliably.
///
/// Only **codeunits** are answered. Tables, pages, reports and xmlports also
/// receive platform-generated events (`OnAfterInsertEvent`, `OnOpenPageEvent`,
/// …) that appear nowhere in source or symbol data, so an "event not found"
/// verdict there would be a false positive.
fn publisher_event_names(
    workspace: &Workspace,
    object_lower: &str,
) -> Option<std::collections::HashSet<String>> {
    if let Some(entry) = workspace.symbols.find_by_name(object_lower) {
        if entry.kind != al_symbols::ObjectKind::Codeunit {
            return None;
        }
        return Some(
            entry
                .methods
                .iter()
                .filter(|method| {
                    method.attributes.iter().any(|attr| {
                        attr.name
                            .eq_ignore_ascii_case(al_insight::attr_names::INTEGRATION_EVENT)
                            || attr
                                .name
                                .eq_ignore_ascii_case(al_insight::attr_names::BUSINESS_EVENT)
                    })
                })
                .map(|method| method.name.to_lowercase())
                .collect(),
        );
    }

    let path = workspace.file_index.find_by_object_name(object_lower)?;
    let (text, tree) = workspace.file_index.get_cached_parse(&path)?;
    let info = al_syntax::find_object_declaration(&tree, &text)?;
    if !info.kind.eq_ignore_ascii_case("codeunit") {
        return None;
    }
    Some(collect_declared_event_names(&tree, &text))
}

/// Names of procedures carrying an `[IntegrationEvent]` / `[BusinessEvent]`
/// attribute, lower-cased.
fn collect_declared_event_names(
    tree: &tree_sitter::Tree,
    text: &str,
) -> std::collections::HashSet<String> {
    let source = text.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration" | "event_declaration"
        ) {
            let attributes = get_preceding_attribute(node, source).unwrap_or_default();
            let lower = attributes.to_lowercase();
            if lower.contains("integrationevent") || lower.contains("businessevent") {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                {
                    names.insert(name.unquote_identifier().to_lowercase());
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Collect event subscriber procedures iteratively: (proc_name, target_object, target_event, line_1based).
/// A subscriber procedure: its name, the publisher kind and object named by
/// the attribute, the event, and the procedure's line.
type Subscription = (String, Option<String>, String, String, u32);

/// The object of `kind` (any publisher kind when unknown) whose ID is `id`.
fn publisher_by_id(workspace: &Workspace, kind: Option<&str>, id: i32) -> Option<String> {
    let kind = kind.and_then(|kind| kind.parse::<al_symbols::ObjectKind>().ok());
    let matches_kind = |candidate: al_symbols::ObjectKind| {
        kind.map_or(!candidate.is_extension(), |kind| kind == candidate)
    };
    for entry in workspace.symbols.all_entries() {
        if entry.id == id && matches_kind(entry.kind) {
            return Some(entry.name.clone());
        }
    }
    workspace.file_index.object_infos.iter().find_map(|infos| {
        infos.value().iter().find_map(|info| {
            let candidate = info.kind.parse::<al_symbols::ObjectKind>().ok()?;
            (info.id == Some(i64::from(id)) && matches_kind(candidate)).then(|| info.name.clone())
        })
    })
}

/// The publisher kind in the attribute's first argument: `Codeunit` for
/// `ObjectType::Codeunit`.
fn parse_subscriber_kind(attr_text: &str) -> Option<String> {
    let inner = attr_text.split_once('(')?.1;
    let first = split_args(inner).into_iter().next()?;
    let first = first.trim();
    let kind = first.rsplit_once("::").map_or(first, |(_, kind)| kind);
    Some(kind.trim().to_string()).filter(|kind| !kind.is_empty())
}

fn collect_event_subscribers(
    root: tree_sitter::Node,
    source: &[u8],
    subscribers: &mut Vec<Subscription>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            let attr_text = get_preceding_attribute(node, source);
            if let Some(ref text) = attr_text {
                if text.to_lowercase().contains("eventsubscriber") {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(proc_name) = name_node.utf8_text(source) {
                            let proc_name = al_syntax::clean_identifier(proc_name);
                            let (target_object, target_event) = parse_subscriber_args(text);
                            let target_kind = parse_subscriber_kind(text);
                            let line = al_syntax::procedure_keyword_row(node)
                                .unwrap_or_else(|| node.start_position().row)
                                as u32
                                + 1;
                            subscribers.push((
                                proc_name,
                                target_kind,
                                target_object,
                                target_event,
                                line,
                            ));
                        }
                    }
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn get_preceding_attribute(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" {
            return s.utf8_text(source).ok().map(|s| s.to_string());
        }
        if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Parse the target object and event from an EventSubscriber attribute text.
/// Format: `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Name", 'Event', ...)]`
fn parse_subscriber_args(attr_text: &str) -> (String, String) {
    // Find content between the first '(' and the last ')'.
    // Guard against malformed text where '(' appears after ')'.
    let inner = match (attr_text.find('('), attr_text.rfind(')')) {
        (Some(start), Some(end)) if start < end => &attr_text[start + 1..end],
        _ => "",
    };

    let args = split_args(inner);

    // arg[1] is the target object (e.g., Codeunit::"Sales-Post" or "Sales-Post")
    let target_object = args
        .get(1)
        .map(|s| {
            let s = s.trim();
            let s = if let Some(pos) = s.find("::") {
                &s[pos + 2..]
            } else {
                s
            };
            s.trim_matches('"').trim_matches('\'').to_string()
        })
        .unwrap_or_default();

    // arg[2] is the target event
    let target_event = args
        .get(2)
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .unwrap_or_default();

    (target_object, target_event)
}

/// Split attribute arguments by comma, respecting quoted strings.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '"';

    for ch in s.chars() {
        match ch {
            '"' | '\'' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
                current.push(ch);
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
                current.push(ch);
            }
            ',' if !in_quotes => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A file may declare several objects. An unused member belongs to the
    /// object that declares it, not to the file's first object.
    #[test]
    fn an_unused_member_of_the_second_object_in_a_file_names_that_object() {
        let ws = workspace_with_files(vec![(
            "/src/Pair.al",
            r#"table 50100 "Ship Setup"
{
    fields
    {
        field(1; "Primary Key"; Code[10]) { }
    }
}

codeunit 50101 "Ship Helper"
{
    local procedure UnusedHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        let finding = unused
            .iter()
            .find(|symbol| symbol.name == "UnusedHelper")
            .unwrap_or_else(|| panic!("expected the unused helper: {unused:?}"));
        assert_eq!(finding.object, "Ship Helper", "{unused:?}");
        // The table's own key field belongs to the table, not to the codeunit.
        let field = unused
            .iter()
            .find(|symbol| symbol.kind == UnusedKind::Field)
            .unwrap_or_else(|| panic!("expected the unreferenced field: {unused:?}"));
        assert_eq!(field.object, "Ship Setup", "{unused:?}");
    }

    #[test]
    fn unused_procedure_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50100 "My Codeunit"
{
    procedure UsedProc()
    begin
    end;

    procedure UnusedHelper()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50101 "Caller"
{
    procedure DoWork()
    var
        cu: Codeunit "My Codeunit";
    begin
        cu.UsedProc();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "UnusedHelper"
                && u.kind == UnusedKind::Procedure
                && u.reason == UnusedReason::ZeroReferences),
            "Expected UnusedHelper to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused.iter().any(|u| u.name == "UsedProc"),
            "UsedProc should not be flagged as unused"
        );
    }

    #[test]
    fn event_subscriber_and_test_not_flagged_as_dead() {
        // A local [EventSubscriber] is dispatched by the event system and a
        // [Test] is runner-invoked; neither has a direct call site, but neither
        // must be reported as dead code. A genuinely-unused plain helper
        // in the same object still must be.
        let ws = workspace_with_files(vec![
            (
                "/src/Pub.al",
                r#"codeunit 50101 "Some Pub"
{
    [IntegrationEvent(false, false)]
    procedure OnFoo()
    begin
    end;
}"#,
            ),
            (
                "/src/Subs.al",
                r#"codeunit 50100 "Subs"
{
    Subtype = Test;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Some Pub", 'OnFoo', '', false, false)]
    local procedure HandleFoo()
    begin
    end;

    [Test]
    procedure TestSomething()
    begin
    end;

    [ConfirmHandler]
    procedure HandleConfirm(Question: Text; var Reply: Boolean)
    begin
    end;

    local procedure GenuinelyUnused()
    begin
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        for name in ["HandleFoo", "TestSomething", "HandleConfirm"] {
            assert!(
                !unused.iter().any(|u| u.name == name),
                "{name} is framework-invoked and must not be flagged; got {unused:?}"
            );
        }
        assert!(
            unused.iter().any(|u| u.name == "GenuinelyUnused"),
            "a plain uncalled helper must still be flagged; got {unused:?}"
        );
    }

    #[test]
    fn unused_table_field_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
        field(3; "Legacy Flag"; Boolean) { }
    }
}"#,
            ),
            (
                "/src/MyPage.al",
                r#"page 50100 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field("Name"; Rec."Name") { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "Legacy Flag"
                && u.kind == UnusedKind::Field
                && u.reason == UnusedReason::ZeroReferences),
            "Expected 'Legacy Flag' field to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused
                .iter()
                .any(|u| u.name == "No." && u.kind == UnusedKind::Field),
            "'No.' should not be flagged as unused"
        );
        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Field),
            "'Name' should not be flagged as unused"
        );
    }

    #[test]
    fn collect_fields_uses_ast_and_ignores_comments() {
        let text = r#"table 50100 "T"
{
    fields {
        field(
            1;
            "Real";
            Integer)
        { }
        /*
        field(2; "Phantom"; Integer) { }
        */
        /* old: field(99; "Removed"; Integer) */ field(3; "AlsoReal"; Integer) { }
    }
}"#;
        let mut parser = al_syntax::AlParser::new();
        let parsed = parser.parse(text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let fields = collect_table_fields(parsed.tree.root_node(), text);
        let names: Vec<_> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Real", "AlsoReal"]);
    }

    #[test]
    fn orphaned_subscriber_detected() {
        let ws = workspace_with_files(vec![(
            "/src/MySub.al",
            r#"codeunit 50102 "My Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Old Publisher", 'OnOldEvent', '', false, false)]
    local procedure HandleOldEvent()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "HandleOldEvent"
                && u.kind == UnusedKind::Subscriber
                && u.reason == UnusedReason::PublisherRemoved),
            "Expected orphaned subscriber to be detected. Got: {:?}",
            unused
        );
    }

    #[test]
    fn no_false_positives_for_local_calls() {
        let ws = workspace_with_files(vec![(
            "/src/Internal.al",
            r#"codeunit 50103 "Internal"
{
    procedure PublicEntry()
    begin
        InternalHelper();
    end;

    local procedure InternalHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused.iter().any(|u| u.name == "InternalHelper"),
            "InternalHelper is called locally, should not be flagged"
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let unused = dead_code(&ws).unwrap();
        assert!(unused.is_empty());
    }

    #[test]
    fn event_publishers_not_flagged_as_unused_procedures() {
        let ws = workspace_with_files(vec![(
            "/src/Publisher.al",
            r#"codeunit 50104 "My Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost(var SalesHeader: Record "Sales Header")
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused.iter().any(|u| u.name == "OnAfterPost"),
            "Event publishers should not be flagged as dead code"
        );
    }

    #[test]
    fn procedure_named_name_not_masked_by_field_access() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50200 "My Codeunit"
{
    local procedure Name()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"page 50200 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field(NameField; Rec.Name) { }
            field(NameField2; Rec2.Name) { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Procedure),
            "Procedure 'Name' must be flagged as unused despite Rec.Name field accesses. Got: {:?}",
            unused
        );
    }

    #[test]
    fn dead_code_procedure_in_string_literal_does_not_count_as_call() {
        let ws = workspace_with_files(vec![
            (
                "/src/Definer.al",
                r#"codeunit 50300 "Definer"
{
    procedure DoStuff()
    begin
    end;
}"#,
            ),
            (
                "/src/Other.al",
                r#"codeunit 50301 "Other"
{
    procedure Run()
    var
        msg: Text;
    begin
        msg := 'DoStuff(';
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            unused
                .iter()
                .any(|u| u.name == "DoStuff" && u.kind == UnusedKind::Procedure),
            "DoStuff is referenced ONLY inside a string literal — must still be reported as unused. \
             Got: {:?}",
            unused
        );
    }

    #[test]
    fn unused_field_detected_despite_bare_identifier_in_other_files() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50300 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Description"; Text[100]) { }
    }
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50301 "Caller"
{
    procedure DoIt()
    var
        Description: Text[100];
    begin
        Description := 'hello';
        Message(Description);
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            unused.iter().any(|u| u.name == "Description"
                && u.kind == UnusedKind::Field
                && u.object == "My Table"),
            "Field 'Description' must be flagged as unused when only a bare identifier appears elsewhere. Got: {:?}",
            unused
        );
    }

    #[test]
    fn unqualified_field_reference_in_own_table_counts_as_use() {
        let ws = workspace_with_files(vec![(
            "/src/MyTable.al",
            r#"table 50302 "My Table"
{
    fields
    {
        field(1; Amount; Decimal) { }
        field(2; Unused; Decimal) { }
    }

    procedure SetAmount()
    begin
        Amount := 1;
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            !unused
                .iter()
                .any(|item| item.kind == UnusedKind::Field && item.name == "Amount"),
            "own-table bare field reference must count as use: {unused:?}"
        );
        assert!(
            unused
                .iter()
                .any(|item| item.kind == UnusedKind::Field && item.name == "Unused"),
            "unreferenced field must still be found: {unused:?}"
        );
    }

    #[test]
    fn action_trigger_call_is_found_by_the_ast() {
        let ws = workspace_with_files(vec![(
            "/src/MyPage.al",
            r#"page 50303 "My Page"
{
    actions
    {
        area(Processing)
        {
            action(Run)
            {
                trigger OnAction()
                begin
                    Helper();
                end;
            }
        }
    }

    local procedure Helper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            !unused
                .iter()
                .any(|item| item.kind == UnusedKind::Procedure && item.name == "Helper"),
            "action trigger call must keep Helper live: {unused:?}"
        );
    }

    #[test]
    fn dead_code_does_not_skip_real_call_when_other_procedure_name_contains_target() {
        let ws = workspace_with_files(vec![
            (
                "/src/Target.al",
                r#"codeunit 50300 "Target"
{
    procedure Post()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50301 "Caller"
{
    procedure PostDocument()
    var
        target: Codeunit "Target";
    begin
        target.Post();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Post" && u.kind == UnusedKind::Procedure),
            "Procedure 'Post' is called from another file; must NOT be flagged unused. Got: {:?}",
            unused
        );
    }

    #[test]
    fn split_args_respects_quoted_comma() {
        let args = split_args("Codeunit::\"Sales, Post\", 'OnAfter, Run'");
        assert_eq!(args.len(), 2, "got: {args:?}");
        assert_eq!(args[0], "Codeunit::\"Sales, Post\"");
        assert_eq!(args[1], "'OnAfter, Run'");
    }

    #[test]
    fn split_args_empty_input() {
        assert!(split_args("").is_empty());
    }

    #[test]
    fn parse_subscriber_args_well_formed() {
        let (obj, event) = parse_subscriber_args(
            "[EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPostSalesDoc', '', false, false)]",
        );
        assert_eq!(obj, "Sales-Post");
        assert_eq!(event, "OnAfterPostSalesDoc");
    }

    #[test]
    fn parse_subscriber_args_missing_closing_paren() {
        // No matching ')': inner is empty, so both fields default to "".
        let (obj, event) = parse_subscriber_args("[EventSubscriber(ObjectType::Codeunit");
        assert_eq!(obj, "");
        assert_eq!(event, "");
    }

    #[test]
    fn parse_subscriber_args_strips_type_prefix_and_quotes() {
        let (obj, _) =
            parse_subscriber_args("[EventSubscriber(ObjectType::Table, Table::\"Item\", 'OnX')]");
        assert_eq!(obj, "Item");
    }

    /// `ObjectType::Codeunit, 50100` names the publisher by ID, as AL allows
    /// and every package subscriber does. Looking `50100` up as a name called
    /// the subscriber orphaned, with high confidence, and told the user to
    /// delete live code.
    #[test]
    fn a_subscriber_naming_its_publisher_by_id_is_not_orphaned() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Publisher.al"),
            r#"codeunit 50100 "Sales Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost()
    begin
    end;
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Subscriber.al"),
            r#"codeunit 50101 "Sales Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, 50100, 'OnAfterPost', '', false, false)]
    local procedure ByNumericId()
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, 50199, 'OnAfterPost', '', false, false)]
    local procedure ToAMissingId()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        let orphans: Vec<_> = results
            .iter()
            .filter(|r| r.kind == UnusedKind::Subscriber)
            .map(|r| r.name.as_str())
            .collect();

        assert!(!orphans.contains(&"ByNumericId"), "{orphans:?}");
        assert!(orphans.contains(&"ToAMissingId"), "{orphans:?}");
    }

    /// `_target_event` was bound but never used, so a subscriber to a removed
    /// or renamed event on a still-existing publisher was never reported.
    #[test]
    fn orphaned_subscriber_to_a_removed_event_is_reported() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Publisher.al"),
            r#"codeunit 50100 "Sales Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost()
    begin
    end;
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Subscriber.al"),
            r#"codeunit 50101 "Sales Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales Publisher", 'OnBeforePost', '', false, false)]
    local procedure HandleBeforePost()
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales Publisher", 'OnAfterPost', '', false, false)]
    local procedure HandleAfterPost()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        let orphans: Vec<_> = results
            .iter()
            .filter(|r| r.kind == UnusedKind::Subscriber)
            .collect();
        assert!(
            orphans
                .iter()
                .any(|r| r.name == "HandleBeforePost" && r.reason == UnusedReason::EventRemoved),
            "subscriber to the removed 'OnBeforePost' must be reported: {orphans:?}"
        );
        assert!(
            !orphans.iter().any(|r| r.name == "HandleAfterPost"),
            "subscriber to a live event must not be reported: {orphans:?}"
        );
    }

    /// Table/page platform events are declared nowhere, so they must never be
    /// reported as removed.
    #[test]
    fn platform_table_events_are_not_reported_as_removed() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/MyTable.al"),
            r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[50])
        {
            DataClassification = CustomerContent;
        }
    }
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/TableSubscriber.al"),
            r#"codeunit 50101 "Table Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::"My Table", 'OnAfterInsertEvent', '', false, false)]
    local procedure HandleInsert()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        assert!(
            !results
                .iter()
                .any(|r| r.kind == UnusedKind::Subscriber && r.name == "HandleInsert"),
            "platform table events must not be flagged: {results:?}"
        );
    }
}
