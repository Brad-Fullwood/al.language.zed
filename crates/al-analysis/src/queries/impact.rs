//! Dependency impact analysis query.
//!
//! "If I change this symbol, what breaks?" Searches the symbol index and workspace
//! files for all consumers of a named symbol (table field, procedure, object, etc.).

use al_syntax::IdentifierText;
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolEntry};
use serde::Serialize;

use crate::workspace_sources::{self, WorkspaceSource};
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
    /// The object that declares the queried member. Reported so the agent can
    /// find the definition, and kept distinct from a consumer because changing
    /// a field does not "break" the table that declares it.
    Declares,
}

/// How sure the analysis is that the reported consumer really uses the queried
/// symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImpactConfidence {
    /// The reference was bound to the queried symbol — through the symbol
    /// index, a source-table/extends relationship, or a receiver in the source
    /// that resolves to the queried object.
    #[default]
    High,
    /// The name matches but no receiver could be bound to the queried object,
    /// so the consumer may be an unrelated same-named identifier.
    Low,
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
    /// How sure the analysis is about this consumer.
    #[serde(default)]
    pub confidence: ImpactConfidence,
    /// Why the confidence is not `high` (only set for low-confidence entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImpactError {
    #[error("impact analysis refused an incomplete workspace snapshot: {reason}")]
    IncompleteWorkspace { reason: String },
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
pub fn impact(workspace: &Workspace, symbol: &str) -> Result<Vec<ImpactEntry>, ImpactError> {
    let workspace_sources = workspace_sources::snapshot(workspace).map_err(|error| {
        ImpactError::IncompleteWorkspace {
            reason: error.to_string(),
        }
    })?;
    let (object_part, member_part) = parse_symbol(symbol);
    let mut results = Vec::new();

    // Bounded: only extensions of the named object via the by_extends index.
    for entry in workspace.symbols.get_extensions_of(&object_part) {
        check_extends(&entry, &object_part, None, &mut results);
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
            check_member_consumers(&entry, &object_part, None, member, &mut results);
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
        search_workspace_files_for_member(
            &workspace_sources,
            &object_part,
            None,
            member,
            &mut results,
        );
    } else {
        search_workspace_files(&workspace_sources, &object_part, None, &mut results);
    }

    Ok(results)
}

/// The workspace's own consumers of many symbols, from one snapshot.
///
/// [`impact`] snapshots the workspace and scans every symbol entry, package
/// entries included, on each call. A package diff asks about hundreds of
/// changed symbols and only cares which of them the workspace uses, so this
/// takes the snapshot once and scans only the workspace's entries.
pub struct WorkspaceImpactIndex {
    sources: Vec<WorkspaceSource>,
    entries: Vec<Arc<SymbolEntry>>,
}

impl WorkspaceImpactIndex {
    pub fn new(workspace: &Workspace) -> Result<Self, ImpactError> {
        let sources = workspace_sources::snapshot(workspace).map_err(|error| {
            ImpactError::IncompleteWorkspace {
                reason: error.to_string(),
            }
        })?;
        let entries = workspace
            .symbols
            .all_entries()
            .into_iter()
            .filter(|entry| {
                !entry.synthetic
                    && al_symbols::source_availability::is_workspace_package(&entry.package)
            })
            .collect();
        Ok(Self { sources, entries })
    }

    /// Workspace consumers of `symbol` (`Object` or `Object.Member`), in
    /// [`impact`]'s row shape. For a member, only the code that uses that
    /// member counts; an extension of the object or a page built on it is a
    /// use of the object.
    ///
    /// With `kind`, a use has to be able to name an object of that kind: page
    /// "Payment Terms" and table "Payment Terms" share a name, and a
    /// `Record "Payment Terms"` variable does not use the page.
    pub fn consumers(&self, symbol: &str, kind: Option<ObjectKind>) -> Vec<ImpactEntry> {
        const SOURCE_TABLE_KINDS: &[ObjectKind] = &[
            ObjectKind::Page,
            ObjectKind::PageExtension,
            ObjectKind::Report,
            ObjectKind::ReportExtension,
            ObjectKind::Query,
        ];
        let (object_part, member_part) = parse_symbol(symbol);
        let mut results = Vec::new();
        for entry in &self.entries {
            match &member_part {
                Some(member) => {
                    check_member_consumers(entry, &object_part, kind, member, &mut results)
                }
                // Extending an object, or showing it as a page's source
                // table, is a use of the object. It is not a use of each of
                // its members: listing every table extension of Customer
                // against each removed Customer field buried the real uses.
                None => {
                    check_extends(entry, &object_part, kind, &mut results);
                    // A source table, a `Record` parameter and a
                    // `TableRelation` all name a table.
                    if kind_allows(kind, ObjectKind::Table) {
                        if SOURCE_TABLE_KINDS.contains(&entry.kind) {
                            check_source_table(entry, &object_part, &mut results);
                        }
                        check_object_consumers(entry, &object_part, &mut results);
                    }
                }
            }
        }
        match &member_part {
            Some(member) => search_workspace_files_for_member(
                &self.sources,
                &object_part,
                kind,
                member,
                &mut results,
            ),
            None => search_workspace_files(&self.sources, &object_part, kind, &mut results),
        }
        results
    }
}

/// Parse a symbol specifier into (object_name, optional_member_name).
///
/// Examples:
/// - `"Customer"` → `("Customer", None)`
/// - `"Customer.\"Credit Limit\""` → `("Customer", Some("Credit Limit"))`
/// - `"Sales-Post.PostDocument"` → `("Sales-Post", Some("PostDocument"))`
fn parse_symbol(symbol: &str) -> (String, Option<String>) {
    if let Some(dot_pos) = find_unquoted_dot(symbol) {
        let object = symbol[..dot_pos].unquote_identifier().into_owned();
        let member = symbol[dot_pos + 1..].unquote_identifier().into_owned();
        (object, Some(member))
    } else {
        (symbol.unquote_identifier().into_owned(), None)
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

/// Whether a use that names an object of `candidate` kind counts under the
/// `kind` filter. No filter keeps everything.
fn kind_allows(kind: Option<ObjectKind>, candidate: ObjectKind) -> bool {
    kind.is_none_or(|kind| kind == candidate)
}

/// Whether a reference written with `keyword` (`Record`, `Page`, `Codeunit`,
/// `Database`, ...) can name an object of `kind`. A keyword that names no
/// object kind (`TestPage`, a variable name) cannot rule the reference out.
fn keyword_allows(keyword: &str, kind: Option<ObjectKind>) -> bool {
    let Some(kind) = kind else {
        return true;
    };
    let keyword = keyword.trim();
    if keyword.eq_ignore_ascii_case("record") || keyword.eq_ignore_ascii_case("database") {
        return kind == ObjectKind::Table;
    }
    keyword
        .parse::<ObjectKind>()
        .map_or(true, |named| named == kind)
}

/// The word written just before `offset`, skipping `::` and whitespace:
/// `Record` in `Record "Payment Terms"`, `Page` in `Page::"Payment Terms"`.
fn keyword_before(text: &str, offset: usize) -> Option<&str> {
    let before = text.get(..offset)?.trim_end();
    let before = before.strip_suffix("::").unwrap_or(before).trim_end();
    let start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |index| index + 1);
    let word = &before[start..];
    (!word.is_empty()).then_some(word)
}

/// Push an Extends impact for an entry already known to extend `target_object`.
///
/// `entry` comes from `get_extensions_of(target_object)`, so the by_extends index
/// has already done a case-insensitive name match. The double-check below is
/// retained as a defence against stale index state.
fn check_extends(
    entry: &Arc<SymbolEntry>,
    target_object: &str,
    kind: Option<ObjectKind>,
    results: &mut Vec<ImpactEntry>,
) {
    if kind.is_some() && entry.kind.base_kind() != kind {
        return;
    }
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
                confidence: ImpactConfidence::High,
                note: None,
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
                .unquote_identifier()
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
                confidence: ImpactConfidence::High,
                note: None,
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
    kind: Option<ObjectKind>,
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
            // AL attribute names are case-insensitive and workspace-derived
            // `SymbolEntry` attributes preserve the source casing, so a source
            // `[eventsubscriber(...)]` must still match.
            if !attr
                .name
                .eq_ignore_ascii_case(al_insight::attr_names::EVENT_SUBSCRIBER)
                || attr.arguments.len() < 3
            {
                continue;
            }
            let target_obj_arg = attr.arguments.get(1).and_then(|s| {
                let s = s.trim();
                if let Some(pos) = s.find("::") {
                    // `Codeunit::"Sales-Post"`, `Database::Customer`.
                    if !keyword_allows(&s[..pos], kind) {
                        return None;
                    }
                    Some(
                        s[pos + 2..]
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_lowercase(),
                    )
                } else {
                    Some(s.trim_matches('"').trim_matches('\'').to_lowercase())
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
                    confidence: ImpactConfidence::High,
                    note: None,
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
    use al_insight::analysis::{extract_table_relation_tables, is_record_of};

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
                    confidence: ImpactConfidence::High,
                    note: None,
                });
                break;
            }
        }
    }

    // Procedure signatures do not include object-scope record variables.
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
                confidence: ImpactConfidence::High,
                note: None,
            });
            break;
        }
    }

    for field in &entry.fields {
        for prop in &field.properties {
            // Every branch of a conditional `TableRelation` is a consumer, not
            // just the first one.
            if prop.name.eq_ignore_ascii_case("TableRelation")
                && extract_table_relation_tables(&prop.value)
                    .iter()
                    .any(|table| table.eq_ignore_ascii_case(target_object))
            {
                results.push(ImpactEntry {
                    kind: entry.kind,
                    id: entry.id,
                    name: entry.name.clone(),
                    proc: None,
                    field: Some(field.name.clone()),
                    impact_type: ImpactType::Filter,
                    package: Some(entry.package.clone()),
                    confidence: ImpactConfidence::High,
                    note: None,
                });
            }
        }
    }
}

fn search_workspace_files(
    workspace_sources: &[WorkspaceSource],
    search_name: &str,
    kind: Option<ObjectKind>,
    results: &mut Vec<ImpactEntry>,
) {
    for source in workspace_sources {
        // Per object declaration, not per file: a file may hold several
        // objects, and a hit anywhere in it was reported as the first one.
        for (object, node) in source.object_nodes() {
            let refs = al_syntax::find_variable_references_under(node, &source.text, search_name);
            // `Record "Payment Terms"` does not name page "Payment Terms".
            let names_kind = refs.iter().any(|reference| {
                keyword_before(&source.text, reference.start_byte)
                    .is_none_or(|keyword| keyword_allows(keyword, kind))
            });
            if !names_kind {
                continue;
            }
            results.push(ImpactEntry {
                kind: object.kind,
                id: object.normalized_id,
                name: object.info.name.clone(),
                proc: None,
                field: None,
                impact_type: ImpactType::Read,
                package: None,
                confidence: ImpactConfidence::High,
                note: None,
            });
        }
    }
}

/// Member-scoped workspace search: report a file as a *confident* consumer only
/// when one of its occurrences of `member` is qualified by a receiver that
/// resolves to `object_name`.
///
/// Searching the *member name alone* — as this used to — reported any
/// same-named identifier of an unrelated object as a consumer. When no
/// occurrence can be bound (an unqualified use outside the declaring object, or
/// a receiver whose type is not declared in this file), the file is still
/// reported but marked [`ImpactConfidence::Low`] rather than asserted.
fn search_workspace_files_for_member(
    workspace_sources: &[WorkspaceSource],
    object_name: &str,
    kind: Option<ObjectKind>,
    member: &str,
    results: &mut Vec<ImpactEntry>,
) {
    let object_lower = object_name.to_lowercase();
    for source in workspace_sources {
        // Per object declaration, not per file: a file may hold several
        // objects, and the receiver bindings, the `extends` target and the
        // `SourceTable` of the first one decided the verdict for all of them.
        for (object, node) in source.object_nodes() {
            let refs = al_syntax::find_variable_references_under(node, &source.text, member);
            if refs.is_empty() {
                continue;
            }

            let object_text = object.text(&source.text);
            let receivers = declared_receivers(node, &source.text, |type_text| {
                type_target_matches(type_text, object_lower.as_str(), kind)
            });
            // A receiver this object declares as something else is a known
            // non-use: `Cust.Picture` with `Cust: Record Customer` does not
            // read Vendor.Picture, and `Terms: Record "Payment Terms"` is not
            // page "Payment Terms".
            let mut other_receivers = declared_receivers(node, &source.text, |_| true);
            other_receivers.retain(|name| !receivers.contains(name));
            let rules_out = |reference: &tree_sitter::Range| {
                receiver_before(&source.text, reference.start_byte)
                    .is_some_and(|receiver| other_receivers.contains(&receiver.to_lowercase()))
            };
            if refs.iter().all(rules_out) {
                continue;
            }
            let declares_target = (object.info.name.to_lowercase() == object_lower
                && kind_allows(kind, object.kind))
                || (extends_target(object_text, &object_lower)
                    && (kind.is_none() || object.kind.base_kind() == kind));
            // A page/report/query bound to the table through `SourceTable` is the
            // ordinary consumer of a field, and `Rec` inside it is that table. The
            // survey saw every such page reported as `confidence: low`.
            let source_table_target = kind_allows(kind, ObjectKind::Table)
                && source_table_of(object_text)
                    .is_some_and(|table| table.to_lowercase() == object_lower);

            let bound = refs.iter().any(|reference| {
                match receiver_before(&source.text, reference.start_byte) {
                    Some(receiver) => {
                        let lower = receiver.to_lowercase();
                        lower == object_lower
                        || receivers.contains(&lower)
                        // `Rec`/`xRec` inside the target object (or an
                        // extension of it, or a page bound to it) refer to
                        // the target itself.
                        || ((declares_target || source_table_target)
                            && matches!(lower.as_str(), "rec" | "xrec"))
                    }
                    // Unqualified use binds to the enclosing object, or to the
                    // page's source table when the file has one.
                    None => declares_target || source_table_target,
                }
            });

            let impact_type = if declares_target {
                ImpactType::Declares
            } else if source_table_target {
                ImpactType::Display
            } else {
                ImpactType::Read
            };
            results.push(ImpactEntry {
                kind: object.kind,
                id: object.normalized_id,
                name: object.info.name.clone(),
                proc: None,
                field: None,
                impact_type,
                package: None,
                confidence: if bound {
                    ImpactConfidence::High
                } else {
                    ImpactConfidence::Low
                },
                note: if bound {
                    None
                } else {
                    Some(format!(
                        "name match only — no receiver in this object resolves to '{object_name}'"
                    ))
                },
            });
        }
    }
}

/// The `SourceTable` property value declared in an AL page, report or query
/// source file, unquoted.
fn source_table_of(text: &str) -> Option<&str> {
    for line in text.lines() {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, '=');
        let key = parts.next()?.trim();
        if !key.eq_ignore_ascii_case("SourceTable") {
            continue;
        }
        let value = parts.next()?.trim().trim_end_matches(';').trim();
        return Some(
            value
                .strip_prefix('"')
                .and_then(|inner| inner.split('"').next())
                .unwrap_or(value),
        );
    }
    None
}

/// Whether the object declared at the top of `text` is an extension of
/// `object_lower` (`… extends "Customer"`).
fn extends_target(text: &str, object_lower: &str) -> bool {
    let Some(header) = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
    else {
        return false;
    };
    let lower = header.to_lowercase();
    let Some(position) = lower.find(" extends ") else {
        return false;
    };
    let rest = header[position + " extends ".len()..].trim();
    let name = rest
        .strip_prefix('"')
        .and_then(|inner| inner.split('"').next())
        .unwrap_or_else(|| rest.split_whitespace().next().unwrap_or(""));
    name.to_lowercase() == object_lower
}

/// Lower-cased names of the variables and parameters declared under `root`
/// whose type text satisfies `accepts` (e.g. `Cust` for `Cust: Record
/// Customer` when it accepts `Record Customer`).
fn declared_receivers(
    root: tree_sitter::Node<'_>,
    text: &str,
    accepts: impl Fn(&str) -> bool,
) -> std::collections::HashSet<String> {
    let source = text.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "regular_variable_declaration" | "parameter") {
            let targets = node
                .child_by_field_name("type")
                .and_then(|type_node| type_node.utf8_text(source).ok())
                .map(&accepts)
                .unwrap_or(false);
            if targets {
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    if let Ok(text) = name.utf8_text(source) {
                        names.insert(text.unquote_identifier().to_lowercase());
                    }
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Whether an AL type string such as `Record "Sales Header"` or `Codeunit Foo`
/// targets `object_lower`.
fn type_target_matches(type_text: &str, object_lower: &str, kind: Option<ObjectKind>) -> bool {
    let trimmed = type_text.trim();
    let Some((keyword, rest)) = trimmed.split_once(char::is_whitespace) else {
        return false;
    };
    if !keyword_allows(keyword, kind) {
        return false;
    }
    let target = rest.trim();
    let target = target
        .strip_prefix('"')
        .and_then(|inner| inner.split('"').next())
        .unwrap_or_else(|| target.split_whitespace().next().unwrap_or(""));
    target.to_lowercase() == object_lower
}

/// The receiver identifier immediately preceding the `.` before `offset`, if
/// the occurrence at `offset` is a member access.
fn receiver_before(text: &str, offset: usize) -> Option<String> {
    let before = text.get(..offset)?.trim_end();
    let before = before.strip_suffix('.')?;
    let before = before.trim_end();
    if let Some(stripped) = before.strip_suffix('"') {
        let open = stripped.rfind('"')?;
        return Some(stripped[open + 1..].to_string());
    }
    let start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|index| index + 1)
        .unwrap_or(0);
    let receiver = &before[start..];
    if receiver.is_empty() {
        None
    } else {
        Some(receiver.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    #[test]
    fn parse_symbol_unescapes_a_doubled_quote_in_either_part() {
        assert_eq!(
            parse_symbol(r#""Cust ""Main"" Rec"."Name""""#),
            ("Cust \"Main\" Rec".to_string(), Some("Name\"".to_string()))
        );
    }

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
            permissions: Vec::new(),
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
            permissions: Vec::new(),
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
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    /// A file may declare several objects. A consumer reported by the
    /// workspace scan is the object the reference is in, not the file's first
    /// object, and the reference must not also be attributed to an object
    /// that does not contain it.
    #[test]
    fn a_workspace_consumer_in_the_second_object_of_a_file_names_that_object() {
        let ws = workspace_with_files(vec![(
            "/src/Pair.al",
            r#"codeunit 50100 "Ship Helper"
{
    procedure DoNothing()
    begin
    end;
}

codeunit 50101 "Ship Consumer"
{
    procedure Read()
    var
        Cust: Record Customer;
    begin
        Cust.Init();
    end;
}"#,
        )]);
        ws.symbols.add_entries(&[make_table(18, "Customer")]);

        let results = impact(&ws, "Customer").unwrap();
        let consumers: Vec<&str> = results
            .iter()
            .filter(|entry| entry.impact_type == ImpactType::Read)
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(
            consumers.contains(&"Ship Consumer"),
            "the object holding the reference must be reported: {results:?}"
        );
        assert!(
            !consumers.contains(&"Ship Helper"),
            "an object with no reference must not be reported: {results:?}"
        );
    }

    #[test]
    fn impact_finds_extensions() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_table(18, "Customer"),
            make_table_ext(50100, "Cust Ext", "Customer"),
        ]);

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "NonexistentObject").unwrap();
        assert!(results.is_empty(), "Expected no results for unknown symbol");
    }

    /// A broken scratch file is skipped; incoherence still fails the query.
    #[test]
    fn impact_skips_malformed_files_but_rejects_an_incoherent_index() {
        let ws = workspace_with_files(vec![(
            "/src/Broken.al",
            "codeunit 50100 Broken { procedure Incomplete(",
        )]);
        assert!(
            impact(&ws, "Customer").is_ok(),
            "one unparsable file must not block impact analysis"
        );

        let ws = Workspace::new();
        ws.file_index.files.insert(
            PathBuf::from("/src/Ghost.al"),
            "codeunit 50100 Ghost { }".to_string(),
        );
        let error = impact(&ws, "Customer").unwrap_err();
        assert!(error.to_string().contains("incomplete workspace snapshot"));
        assert!(error.to_string().contains("Ghost.al"));
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

    #[test]
    fn impact_extends_uses_targeted_lookup() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_table(18, "Customer"),
            make_table_ext(50100, "Cust Ext", "Customer"),
            make_table(27, "Item"),
            make_table_ext(50101, "Item Ext", "Item"),
        ]);

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "Customer").unwrap();

        assert!(
            results.iter().any(|r| r.name == "Sales Header"
                && r.field.as_deref() == Some("Sell-to Customer No.")
                && r.impact_type == ImpactType::Filter),
            "Expected TableRelation to Customer to be found. Got: {:?}",
            results
        );
    }

    /// A conditional `TableRelation` names one table per branch. Asking about
    /// any of them must find the field.
    #[test]
    fn impact_finds_every_branch_of_a_conditional_table_relation() {
        for target in ["Item", "Resource", "G/L Account"] {
            let ws = Workspace::new();
            let mut sales_line = make_table(37, "Sales Line");
            sales_line.fields = vec![FieldSymbol {
                id: 6,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![PropertyValue {
                    name: "TableRelation".to_string(),
                    value: r#"IF (Type=CONST(Item)) Item."No." ELSE IF (Type=CONST(Resource)) Resource."No." ELSE "G/L Account""#
                        .to_string(),
                }],
            }];
            ws.symbols
                .add_entries(&[make_table(27, target), sales_line]);

            let results = impact(&ws, target).unwrap();
            assert!(
                results.iter().any(|r| r.name == "Sales Line"
                    && r.field.as_deref() == Some("No.")
                    && r.impact_type == ImpactType::Filter),
                "{target} branch must be found. Got: {results:?}"
            );
        }
    }

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

        let results = impact(&ws, "Customer").unwrap();

        assert!(
            !results.iter().any(|r| r.impact_type == ImpactType::Filter),
            "TableRelation to CustomerBank must not match Customer. Got: {:?}",
            results
        );
    }

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

        let results = impact(&ws, "Customer").unwrap();

        assert!(
            !results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Record CustomerBank parameter must not match Customer. Got: {:?}",
            results
        );
    }

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

        let results = impact(&ws, "Customer").unwrap();

        assert!(
            results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "global Record Customer variable must be reported as an impact. Got: {:?}",
            results
        );
    }

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

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "Customer").unwrap();

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

        let results = impact(&ws, "Customer.OnBeforePost").unwrap();

        assert!(
            results
                .iter()
                .any(|r| r.name == "Cust Subscriber" && r.impact_type == ImpactType::Subscribe),
            "EventSubscriber to Customer::OnBeforePost should be found. Got: {:?}",
            results
        );
    }

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

        let results = impact(&ws, "Customer.OnBeforePost").unwrap();

        assert!(
            !results
                .iter()
                .any(|r| r.name == "My Codeunit" && r.impact_type == ImpactType::Read),
            "Member query must not report unrelated Record Customer params. Got: {:?}",
            results
        );
    }

    /// The field's declaring extension and the page that shows it were both
    /// reported as `Read` with `confidence: low`, so the agent could not tell
    /// the definition from a consumer, nor a real hit from a name collision.
    #[test]
    fn field_impact_separates_the_declaration_from_the_pages_that_show_it() {
        let ws = workspace_with_files(vec![
            (
                "/proj/ItemExt.TableExt.al",
                r#"tableextension 50100 "Item Ext" extends Item
{
    fields
    {
        field(50100; "Planning Category"; Code[20])
        {
            DataClassification = CustomerContent;
        }
    }
}
"#,
            ),
            (
                "/proj/ItemPlanning.Page.al",
                r#"page 50100 "Item Planning"
{
    SourceTable = Item;
    layout
    {
        area(content)
        {
            field("Planning Category"; Rec."Planning Category") { ApplicationArea = All; }
        }
    }
}
"#,
            ),
        ]);
        ws.symbols.add_entries(&[make_table(27, "Item")]);

        let results = impact(&ws, "Item.\"Planning Category\"").unwrap();

        let declaration = results
            .iter()
            .find(|entry| entry.name == "Item Ext")
            .expect("the declaring extension must be reported");
        assert_eq!(declaration.impact_type, ImpactType::Declares);

        let page = results
            .iter()
            .find(|entry| entry.name == "Item Planning")
            .expect("the page showing the field must be reported");
        assert_eq!(page.impact_type, ImpactType::Display);
        assert_eq!(
            page.confidence,
            ImpactConfidence::High,
            "Rec inside a page bound to the table resolves to it: {page:?}"
        );
        assert!(page.note.is_none());
    }

    #[test]
    fn a_page_on_another_table_stays_low_confidence() {
        let ws = workspace_with_files(vec![(
            "/proj/VendorCard.Page.al",
            r#"page 50101 "Vendor Planning"
{
    SourceTable = Vendor;
    layout
    {
        area(content)
        {
            field("Planning Category"; Rec."Planning Category") { ApplicationArea = All; }
        }
    }
}
"#,
        )]);
        ws.symbols.add_entries(&[make_table(27, "Item")]);

        let results = impact(&ws, "Item.\"Planning Category\"").unwrap();
        let page = results
            .iter()
            .find(|entry| entry.name == "Vendor Planning")
            .expect("the same-named field elsewhere is still reported");
        assert_eq!(page.confidence, ImpactConfidence::Low);
    }
}
