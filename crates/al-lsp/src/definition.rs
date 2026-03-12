//! Go-to-definition handler.
//!
//! Lookup order:
//! 1. Local definitions (variables, parameters in the same procedure)
//! 2. Cross-file definitions (procedures in workspace .al files)
//! 3. Package symbol definitions (objects from .app files — virtual file generation)

use al_symbols::SymbolEntry;
use al_syntax::AlParser;
use tower_lsp::lsp_types::*;

use crate::parsing;
use crate::resolution::{self, ResolvedMemberKind};
use crate::server::AlServer;

/// Handle textDocument/definition.
pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        tracing::debug!("definition: empty clean_name, returning None");
        return None;
    }

    let looks_like_object_name_early = node.kind() == "quoted_identifier" || clean_name.contains(' ');
    tracing::debug!(
        name = %clean_name,
        node_kind = %node.kind(),
        looks_like_object_name = looks_like_object_name_early,
        "definition: looking up"
    );

    if let Some(access) = resolution::access_path_at(&tree, &text, position) {
        tracing::debug!(receiver = %access.receiver, member = %access.member, "definition: access path found");
        if let Some(receiver) =
            resolution::resolve_expression_type(server, uri, &text, &tree, &access.receiver, position)
        {
            tracing::debug!(receiver_type = %receiver.type_name, receiver_subtype = ?receiver.type_subtype, "definition: receiver type resolved");
            if let Some(member) = resolution::resolve_member(server, uri, &receiver, &access.member) {
                tracing::debug!(member_name = %member.name, member_kind = ?member.kind, "definition: member resolved");
                match member.kind {
                    ResolvedMemberKind::Variable { range: Some(range), .. }
                    | ResolvedMemberKind::Procedure { range: Some(range), .. }
                    | ResolvedMemberKind::Field { range: Some(range) }
                    | ResolvedMemberKind::EnumValue { range: Some(range) } => {
                        tracing::debug!(name = %clean_name, ?range, "definition: returning access path member with range");
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: member.uri.unwrap_or_else(|| uri.clone()),
                            range,
                        }));
                    }
                    _ => {
                        // Member resolved but no range — try virtual file from package symbols
                        tracing::debug!(member_kind = ?member.kind, "definition: access path member has no range, trying package symbol");
                        if let Some(entry) = find_package_entry_for_type(server, &receiver.type_name, receiver.type_subtype.as_deref()) {
                            if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, Some(&access.member)) {
                                return Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: file_uri,
                                    range,
                                }));
                            }
                        }
                    }
                }
            } else {
                tracing::debug!(receiver_type = %receiver.type_name, member = %access.member, "definition: member resolution failed");
            }
        } else {
            tracing::debug!(receiver = %access.receiver, "definition: receiver type resolution failed");
        }
    } else {
        tracing::debug!(name = %clean_name, "definition: no access path at position");
    }

    let looks_like_object_name = node.kind() == "quoted_identifier" || clean_name.contains(' ');
    if looks_like_object_name {
        tracing::debug!(name = %clean_name, "definition: checking workspace object (looks like object name)");
        if let Some((obj_uri, range)) = resolution::resolve_workspace_object_definition(server, clean_name) {
            tracing::debug!(name = %clean_name, uri = %obj_uri, ?range, "definition: returning workspace object definition");
            return Some(GotoDefinitionResponse::Scalar(Location { uri: obj_uri, range }));
        } else {
            tracing::debug!(name = %clean_name, "definition: workspace object not found");
        }

        // Try package symbols — generate virtual AL file for navigation
        let pkg_entries = server.symbols.get_by_name(clean_name);
        if let Some(entry) = pkg_entries.into_iter().find(|e| !e.kind.is_extension()) {
            if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, None) {
                tracing::debug!(
                    name = %clean_name,
                    package = %entry.package,
                    kind = %entry.kind,
                    uri = %file_uri,
                    "definition: returning virtual file for package object"
                );
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: file_uri,
                    range,
                }));
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    if let Some(decl) = resolver.resolve_type(clean_name, position) {
        tracing::debug!(
            name = %clean_name,
            type_name = %decl.type_name,
            scope = ?decl.scope,
            "definition: TypeResolver found declaration"
        );
        let def_range = al_syntax::ts_range_to_lsp(&decl.range);
        if def_range.start != position {
            tracing::debug!(name = %clean_name, ?def_range, "definition: returning TypeResolver result (different position)");
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: def_range,
            }));
        } else {
            tracing::debug!(name = %clean_name, "definition: TypeResolver range matches cursor, skipping");
        }
    } else {
        tracing::debug!(name = %clean_name, "definition: TypeResolver found nothing");
    }

    // 1. Local textual fallback
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    tracing::debug!(name = %clean_name, ref_count = refs.len(), "definition: textual fallback references");
    if refs.len() > 1 {
        let first = &refs[0];
        let def_range = al_syntax::ts_range_to_lsp(first);
        if def_range.start != position {
            tracing::debug!(name = %clean_name, ?def_range, "definition: returning first textual reference");
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: def_range,
            }));
        } else {
            tracing::debug!(name = %clean_name, "definition: first textual ref matches cursor, skipping");
        }
    }

    // Pre-resolve the current file path once for all workspace lookups
    let current_path = uri.to_file_path().ok();

    // 2. Cross-file definitions: check workspace object name index first
    if let Some(obj_path_entry) = server.workspace_objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        tracing::debug!(name = %clean_name, path = ?file_path, "definition: cross-file workspace object index hit");
        // Skip the current file
        let is_current = current_path.as_ref().is_some_and(|cp| *cp == file_path);
        if !is_current {
            if let Some(file_text_entry) = server.workspace_files.get(&file_path) {
                let file_text = file_text_entry.value();
                let result = AlParser::parse_quick(file_text);
                if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, file_text) {
                    if let Ok(file_uri) = Url::from_file_path(&file_path) {
                        tracing::debug!(
                            name = %clean_name,
                            obj_kind = %obj_info.kind,
                            obj_name = %obj_info.name,
                            uri = %file_uri,
                            "definition: returning cross-file workspace object"
                        );
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: file_uri,
                            range: al_syntax::ts_range_to_lsp(&obj_info.range),
                        }));
                    }
                }
            }
        } else {
            tracing::debug!(name = %clean_name, "definition: cross-file workspace object is current file, skipping");
        }
    }

    // 2b. Search workspace files for matching procedures (not in the name index)
    let workspace_file_count = server.workspace_files.len();
    tracing::debug!(name = %clean_name, file_count = workspace_file_count, "definition: scanning workspace files for procedures");
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file (already handled above)
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let result = AlParser::parse_quick(file_text);

        // Check procedures in other files
        let doc_symbols = al_syntax::extract_document_symbols(&result.tree, file_text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                        && child.name.eq_ignore_ascii_case(clean_name)
                    {
                        if let Ok(file_uri) = Url::from_file_path(file_path) {
                            tracing::debug!(
                                name = %clean_name,
                                file = ?file_path,
                                symbol_kind = ?child.kind,
                                "definition: found matching procedure in workspace file"
                            );
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: file_uri,
                                range: child.selection_range,
                            }));
                        }
                    }
                }
            }
        }
    }

    // 3. Package symbols — generate virtual AL file
    let symbols = server.symbols.get_by_name(clean_name);
    if let Some(entry) = symbols.into_iter().find(|e| !e.kind.is_extension()) {
        if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, None) {
            tracing::debug!(
                name = %clean_name,
                package = %entry.package,
                kind = %entry.kind,
                "definition: returning virtual file for package symbol (fallback)"
            );
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: file_uri,
                range,
            }));
        }
    }

    tracing::debug!(
        name = %clean_name,
        index_size = server.symbols.len(),
        workspace_files = server.workspace_files.len(),
        workspace_objects = server.workspace_objects.len(),
        "definition: symbol not found anywhere"
    );
    None
}

// ---------------------------------------------------------------------------
// Virtual file helpers (delegates to al_symbols::virtual_file)
// ---------------------------------------------------------------------------

/// Get or create a virtual AL file for a package symbol, returning LSP types.
///
/// When `member_name` is provided, the returned range points to the specific
/// field / method / enum-value definition inside the generated file instead
/// of falling back to the top of the file.
fn get_or_create_virtual_file(
    server: &AlServer,
    entry: &SymbolEntry,
    member_name: Option<&str>,
) -> Option<(Url, Range)> {
    let app_path = server.symbols.app_path(&entry.package);
    let allow_fallback = server
        .outline_fallback_approved
        .load(std::sync::atomic::Ordering::Relaxed);
    match al_symbols::virtual_file::get_or_create(entry, app_path.as_deref(), allow_fallback) {
        Ok(path) => {
            let uri = Url::from_file_path(&path).ok()?;
            let range = member_name
                .and_then(|name| find_member_range_in_file(&path, name))
                .unwrap_or_default();
            Some((uri, range))
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to create virtual file for package symbol");
            None
        }
    }
}

/// Scan a generated virtual AL file for the line that declares `member_name`
/// and return an LSP range pointing at the name within that line.
fn find_member_range_in_file(path: &std::path::Path, member_name: &str) -> Option<Range> {
    let content = std::fs::read_to_string(path).ok()?;
    let needle = member_name.to_lowercase();

    for (line_idx, line) in content.lines().enumerate() {
        let lower = line.to_lowercase();

        // Only look at declaration lines:
        //   field(N; "Member Name"; Type)
        //   value(N; "Member Name") { }
        //   procedure MemberName(...)
        let is_decl = lower.contains("field(")
            || lower.contains("value(")
            || lower.contains("procedure ");

        if !is_decl {
            continue;
        }

        // Try quoted form first: "Member Name"
        let quoted = format!("\"{}\"", needle);
        if let Some(pos) = lower.find(&quoted) {
            let col = pos + 1; // skip opening quote — cursor on the name
            return Some(Range::new(
                Position::new(line_idx as u32, col as u32),
                Position::new(line_idx as u32, (col + member_name.len()) as u32),
            ));
        }

        // Unquoted form (single-word names): match after "; " or "procedure "
        if let Some(pos) = lower.find(&needle) {
            // Simple boundary check within the line
            let before_ok = pos == 0 || !line.as_bytes()[pos - 1].is_ascii_alphanumeric();
            let after_pos = pos + needle.len();
            let after_ok =
                after_pos >= line.len() || !line.as_bytes()[after_pos].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Some(Range::new(
                    Position::new(line_idx as u32, pos as u32),
                    Position::new(line_idx as u32, after_pos as u32),
                ));
            }
        }
    }

    None
}

/// Find the package SymbolEntry for a resolved type (e.g., Record "Customer" → Customer table entry).
fn find_package_entry_for_type(
    server: &AlServer,
    type_name: &str,
    subtype: Option<&str>,
) -> Option<std::sync::Arc<SymbolEntry>> {
    // For typed objects (Record, Page, Codeunit, etc.), the subtype is the object name
    let obj_name = subtype.or({
        // If no subtype, the type_name itself might be the object name (e.g., Enum "Status")
        if !matches!(type_name, "Record" | "Page" | "Codeunit" | "Report" | "Query" | "Xmlport"
            | "Integer" | "Text" | "Code" | "Decimal" | "Boolean" | "Date" | "Time" | "DateTime"
            | "BigInteger" | "Guid" | "Blob" | "Media" | "MediaSet" | "Option" | "Duration"
            | "RecordId" | "RecordRef" | "FieldRef" | "FilterPageBuilder" | "JsonToken"
            | "JsonValue" | "JsonObject" | "JsonArray" | "HttpClient" | "HttpContent"
            | "HttpHeaders" | "HttpRequestMessage" | "HttpResponseMessage" | "XmlDocument"
            | "XmlElement" | "XmlAttribute" | "XmlNode" | "TextBuilder" | "Label" | "Variant"
            | "Dialog" | "File" | "InStream" | "OutStream" | "List" | "Dictionary"
            | "Notification" | "Action" | "Char" | "Byte") {
            Some(type_name)
        } else {
            None
        }
    })?;

    server
        .symbols
        .get_by_name(obj_name)
        .into_iter()
        .find(|e| !e.kind.is_extension())
}

/// Handle textDocument/references.
pub(crate) fn handle_references(
    server: &AlServer,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    tracing::debug!(name = %clean_name, include_declaration, "references: looking up");

    let mut locations = Vec::new();

    // Find references in the current file
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    tracing::debug!(name = %clean_name, current_file_refs = refs.len(), "references: current file");
    for r in &refs {
        let range = al_syntax::ts_range_to_lsp(r);
        if !include_declaration && range.start == position {
            continue;
        }
        locations.push(Location {
            uri: uri.clone(),
            range,
        });
    }

    // Find references in workspace files
    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let result = AlParser::parse_quick(file_text);
        let refs = al_syntax::find_variable_references(&result.tree, file_text, clean_name);

        if !refs.is_empty() {
            tracing::debug!(name = %clean_name, file = ?file_path, ref_count = refs.len(), "references: found in workspace file");
        }
        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range: al_syntax::ts_range_to_lsp(r),
                });
            }
        }
    }

    tracing::debug!(name = %clean_name, total_refs = locations.len(), "references: total results");
    if locations.is_empty() {
        None
    } else {
        Some(locations)
    }
}

/// Build the replacement text for a rename, preserving quoted identifiers.
///
/// If the original node was a quoted_identifier (e.g. `"My Procedure"`), wrap
/// the new name in quotes. Otherwise return it as-is.
fn make_rename_text(node_kind: &str, original_text: &str, new_name: &str) -> String {
    let is_quoted = node_kind == "quoted_identifier"
        || (original_text.starts_with('"') && original_text.ends_with('"'));
    if is_quoted {
        let clean = new_name.trim_matches('"');
        format!("\"{}\"", clean)
    } else {
        new_name.to_string()
    }
}

/// Handle textDocument/rename.
///
/// Renames the identifier at the cursor across the current file AND all
/// workspace files, returning a `WorkspaceEdit` with changes grouped by URI.
pub(crate) fn handle_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    tracing::debug!(name = %clean_name, new_name = %new_name, "rename: looking up");

    let mut changes = std::collections::HashMap::new();

    // --- Current file ---
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    tracing::debug!(name = %clean_name, current_file_edits = refs.len(), "rename: current file");
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .map(|r| {
                // Determine per-reference whether the matched node is quoted
                let matched_text = &text[r.start_byte..r.end_byte];
                let replacement = make_rename_text(node.kind(), matched_text, &new_name);
                TextEdit {
                    range: al_syntax::ts_range_to_lsp(r),
                    new_text: replacement,
                }
            })
            .collect();
        changes.insert(uri.clone(), edits);
    }

    // --- Workspace files (cross-file rename) ---
    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file — already handled above
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let file_uri = match Url::from_file_path(file_path) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let result = AlParser::parse_quick(file_text);
        let refs = al_syntax::find_variable_references(&result.tree, file_text, clean_name);

        if !refs.is_empty() {
            tracing::debug!(name = %clean_name, file = ?file_path, edit_count = refs.len(), "rename: found edits in workspace file");
            let edits: Vec<TextEdit> = refs
                .iter()
                .map(|r| {
                    let matched_text = &file_text[r.start_byte..r.end_byte];
                    let replacement = make_rename_text("", matched_text, &new_name);
                    TextEdit {
                        range: al_syntax::ts_range_to_lsp(r),
                        new_text: replacement,
                    }
                })
                .collect();
            changes.insert(file_uri, edits);
        }
    }

    let total_files = changes.len();
    let total_edits: usize = changes.values().map(|v| v.len()).sum();
    tracing::debug!(name = %clean_name, files = total_files, total_edits, "rename: total results");
    if changes.is_empty() {
        tracing::debug!(name = %clean_name, "rename: no edits found");
        return None;
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Handle textDocument/prepareRename.
///
/// Validates that the cursor is on a renameable identifier and returns the
/// range together with a placeholder (the unquoted name).
pub(crate) fn handle_prepare_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    // Only allow renaming identifiers
    if !matches!(
        node.kind(),
        "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
    ) {
        return None;
    }

    // Return range + placeholder so the editor pre-fills the current name
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: al_syntax::ts_range_to_lsp(&node.range()),
        placeholder: clean_name.to_string(),
    })
}
