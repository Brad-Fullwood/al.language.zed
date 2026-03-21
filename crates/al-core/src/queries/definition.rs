//! Go-to-definition query.

use al_symbols::SymbolEntry;
use url::Url;

use super::{Location, Position, Range};
use crate::resolution::{self, ResolvedMemberKind};
use crate::workspace::Workspace;

/// Find the definition location of the symbol at the given position.
pub fn definition(workspace: &Workspace, uri: &Url, position: Position) -> Option<Vec<Location>> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, lsp_pos)?;
    let source = text.as_bytes();
    let clean_name = super::node_clean_name(node, source)?;

    // Access path resolution
    if let Some(access) = resolution::access_path_at(&tree, &text, lsp_pos) {
        if let Some(receiver) =
            resolution::resolve_expression_type(workspace, uri, &text, &tree, &access.receiver, lsp_pos)
        {
            if let Some(member) = resolution::resolve_member(workspace, uri, &receiver, &access.member) {
                match member.kind {
                    ResolvedMemberKind::Variable { range: Some(range), .. }
                    | ResolvedMemberKind::Procedure { range: Some(range), .. }
                    | ResolvedMemberKind::Field { range: Some(range) }
                    | ResolvedMemberKind::EnumValue { range: Some(range) } => {
                        return Some(vec![Location {
                            uri: member.uri.unwrap_or_else(|| uri.clone()),
                            range: range.into(),
                        }]);
                    }
                    _ => {
                        if let Some(entry) = find_package_entry_for_type(workspace, &receiver.type_name, receiver.type_subtype.as_deref()) {
                            if let Some((file_uri, range)) = super::get_or_create_virtual_file(workspace, &entry, Some(&access.member)) {
                                return Some(vec![Location { uri: file_uri, range: range.into() }]);
                            }
                        }
                    }
                }
            }
        }
    }

    let looks_like_object_name = node.kind() == "quoted_identifier" || clean_name.contains(' ');
    if looks_like_object_name {
        if let Some((obj_uri, range)) = resolution::resolve_workspace_object_definition(workspace, clean_name) {
            return Some(vec![Location { uri: obj_uri, range: range.into() }]);
        }
        let pkg_entries = workspace.symbols.get_by_name(clean_name);
        if let Some(entry) = pkg_entries.into_iter().find(|e| !e.kind.is_extension()) {
            if let Some((file_uri, range)) = super::get_or_create_virtual_file(workspace, &entry, None) {
                return Some(vec![Location { uri: file_uri, range: range.into() }]);
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    if let Some(decl) = resolver.resolve_type(clean_name, lsp_pos) {
        let def_range: Range = al_syntax::ts_range_to_lsp(&decl.range, text.as_bytes()).into();
        if def_range.start != position {
            return Some(vec![Location { uri: uri.clone(), range: def_range }]);
        }
    }

    // textual fallback
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let first = &refs[0];
        let def_range: Range = al_syntax::ts_range_to_lsp(first, text.as_bytes()).into();
        if def_range.start != position {
            return Some(vec![Location { uri: uri.clone(), range: def_range }]);
        }
    }

    let current_path = uri.to_file_path().ok(); // SILENT: non-file URIs legitimately have no path

    if let Some(obj_path_entry) = workspace.file_index.objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        let is_current = current_path.as_ref().is_some_and(|cp| *cp == file_path);
        if !is_current {
            // Use cached object metadata — avoids re-parsing for go-to-definition.
            if let Some(obj_info_entry) = workspace.file_index.object_info.get(&file_path) {
                let obj_info = obj_info_entry.value();
                if let Some(file_text_entry) = workspace.file_index.files.get(&file_path) {
                    if let Ok(file_uri) = Url::from_file_path(&file_path) {
                        return Some(vec![Location {
                            uri: file_uri,
                            range: al_syntax::ts_range_to_lsp(&obj_info.range, file_text_entry.value().as_bytes()).into(),
                        }]);
                    }
                }
            }
        }
    }

    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) { continue; }
        // Use cached parse tree — avoids re-parsing every workspace file on each definition request.
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let doc_symbols = al_syntax::extract_document_symbols(&file_tree, &file_text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if (child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT)
                        && child.name.eq_ignore_ascii_case(clean_name)
                    {
                        if let Ok(file_uri) = Url::from_file_path(&file_path) {
                            return Some(vec![Location {
                                uri: file_uri,
                                range: child.selection_range.into(),
                            }]);
                        }
                    }
                }
            }
        }
    }

    let symbols = workspace.symbols.get_by_name(clean_name);
    if let Some(entry) = symbols.into_iter().find(|e| !e.kind.is_extension()) {
        if let Some((file_uri, range)) = super::get_or_create_virtual_file(workspace, &entry, None) {
            return Some(vec![Location { uri: file_uri, range: range.into() }]);
        }
    }

    None
}

fn find_package_entry_for_type(
    workspace: &Workspace,
    type_name: &str,
    subtype: Option<&str>,
) -> Option<std::sync::Arc<SymbolEntry>> {
    let obj_name = subtype.or({
        if !matches!(type_name, "Record" | "Page" | "Codeunit" | "Report" | "Query" | "Xmlport" | "Enum") {
            Some(type_name)
        } else {
            None
        }
    })?;
    workspace
        .symbols
        .get_by_name(obj_name)
        .into_iter()
        .find(|e| !e.kind.is_extension())
}
