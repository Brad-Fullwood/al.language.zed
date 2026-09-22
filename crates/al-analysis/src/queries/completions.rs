//! Code completion query.

use url::Url;

use super::Position;
use crate::resolution;
use al_workspace::Workspace;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    #[serde(rename = "insertText")]
    pub insert_text: Option<String>,
    #[serde(rename = "sortText")]
    pub sort_text: Option<String>,
}

/// Completion item kinds (transport-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Snippet,
    Field,
    Property,
    Method,
    Function,
    Variable,
    Class,
    Module,
    Enum,
    EnumMember,
    Value,
    Text,
    Struct,
    Reference,
}

impl serde::Serialize for CompletionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let n: u32 = match self {
            Self::Text => 1,
            Self::Method => 2,
            Self::Function => 3,
            Self::Field => 5,
            Self::Variable => 6,
            Self::Class => 7,
            Self::Module => 9,
            Self::Property => 10,
            Self::Value => 12,
            Self::Enum => 13,
            Self::Keyword => 14,
            Self::Snippet => 15,
            Self::Reference => 18,
            Self::EnumMember => 20,
            Self::Struct => 22,
        };
        serializer.serialize_u32(n)
    }
}

use al_syntax::context::{detect_context, CompletionContext};

/// Get completions at a position in a document.
pub fn completions(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Result<Vec<CompletionEntry>, al_workspace::WorkspaceStateError> {
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return Ok(Vec::new());
    };
    let context = detect_context(&text, position.into());
    tracing::debug!(
        context = ?context, line = position.line, character = position.character,
        "completion: detected context"
    );

    let mut items = Vec::new();

    match context {
        CompletionContext::MemberAccess => {
            if let Some((file_text, tree)) =
                al_source::parsing::get_or_parse(&workspace.documents, uri)
            {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position)
                {
                    if let Some(receiver) = resolution::resolve_expression_type(
                        workspace,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    )? {
                        let lsp_items =
                            resolution::completion_items_for_receiver(workspace, &receiver)?;
                        items.extend(lsp_items.into_iter().map(from_lsp_completion));
                    }
                }
            }
        }
        CompletionContext::EnumAccess => {
            if let Some((file_text, tree)) =
                al_source::parsing::get_or_parse(&workspace.documents, uri)
            {
                if let Some((receiver_expr, _)) = resolution::receiver_chain_before(&text, position)
                {
                    let enum_type = resolution::resolve_expression_type(
                        workspace,
                        uri,
                        &file_text,
                        &tree,
                        &receiver_expr,
                        position,
                    )?
                    .unwrap_or_else(|| resolution::ResolvedType {
                        type_name: receiver_expr.clone(),
                        type_subtype: Some(receiver_expr.clone()),
                    });
                    let lsp_items = resolution::enum_completion_items(workspace, &enum_type)?;
                    items.extend(lsp_items.into_iter().map(from_lsp_completion));
                }
            }
        }
        CompletionContext::TypePosition => {
            for entry in &al_syntax::language_data::keywords().r#type {
                items.push(CompletionEntry {
                    label: entry.keyword.clone(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
            // Filter by what the user has already typed before capping.
            // `get_by_kind` returns the per-kind vector in index insertion
            // order, so taking the first 50 of the Base Application's
            // thousands of tables offered the 50 loaded first — `Customer`
            // almost certainly not among them — and the server sent nothing
            // the client could recover.
            let typed = typed_type_prefix(&text, position);
            for kind in [
                al_symbols::ObjectKind::Table,
                al_symbols::ObjectKind::Enum,
                al_symbols::ObjectKind::Codeunit,
                al_symbols::ObjectKind::Interface,
            ] {
                for arc in matching_type_symbols(workspace, kind, &typed) {
                    // The label is the bare name, so a client filtering
                    // against what the user typed matches it; the quoted form
                    // AL needs goes in `insert_text`.
                    let quoted = needs_quoting(&arc.name);
                    items.push(CompletionEntry {
                        label: arc.name.clone(),
                        kind: CompletionKind::Class,
                        detail: Some(format!("{} {}", arc.kind, arc.id)),
                        documentation: None,
                        insert_text: quoted.then(|| format!("\"{}\"", arc.name)),
                        sort_text: None,
                    });
                }
            }
        }
        CompletionContext::Default => {
            // Include implicit trigger variables (Rec, xRec, CurrPage, etc.) in the
            // default context. A dedicated TriggerBody detection pass would be needed
            // to offer these only inside trigger bodies, but default context is safe.
            for var in al_syntax::language_data::implicit_variables() {
                items.push(CompletionEntry {
                    label: var.name.clone(),
                    kind: CompletionKind::Variable,
                    detail: Some(format!("{} — {}", var.r#type, var.description)),
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
            add_default_completions(workspace, uri, position, &mut items)?;
        }
    }

    if !items.is_empty() {
        finalize_completion_items(&mut items);
    }
    Ok(items)
}

/// Full completions: native resolution first, then .NET CodeAnalysis bridge for member access.
///
/// This is the single code path for all entry points (LSP and daemon).
/// SemanticBridge already enforces a 30s internal timeout — no outer wrapper needed.
pub async fn completions_full(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Result<Vec<CompletionEntry>, String> {
    let items = completions(workspace, uri, position).map_err(|error| error.to_string())?;
    if !items.is_empty() {
        return Ok(items);
    }

    // Bridge fallback: only for member access context.
    // Use get_text_arc to share the cached Arc<String> instead of deep-cloning
    // the entire file contents on every keystroke.
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return Ok(items);
    };
    let ctx = al_syntax::context::detect_context(&text, position.into());
    if !matches!(ctx, al_syntax::context::CompletionContext::MemberAccess) {
        return Ok(items);
    }

    let Some(guard) = al_workspace::get_or_init_bridge(workspace).await else {
        return Ok(items);
    };
    let Some(bridge) = guard.as_ref() else {
        return Ok(items);
    };
    let bridge_generation = bridge.generation();
    let Ok(path) = uri.to_file_path() else {
        return Ok(items);
    };
    // The bridge uses the same zero-based coordinates as LSP.
    let pos = (position.line, position.character);
    // Open-document text takes precedence over on-disk content.
    let unsaved_text = workspace.documents.get_text(uri);
    let configured_package_cache = workspace.config.read().await.package_cache_path.clone();
    let package_cache = match configured_package_cache {
        Some(path) => Some(path),
        None => workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.packages_dir.clone()),
    };
    let bridge_items = match bridge
        .completions_at_with_package_cache(
            &path,
            pos,
            unsaved_text.as_deref(),
            package_cache.as_deref(),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "completions_full: bridge error");
            let restart = matches!(
                e,
                al_semantic::SemanticError::Timeout(_)
                    | al_semantic::SemanticError::Poisoned
                    | al_semantic::SemanticError::HostInit(_)
            );
            drop(guard);
            if restart {
                if let Err(restart_error) =
                    al_workspace::restart_bridge_if_current(workspace, bridge_generation).await
                {
                    tracing::warn!(error = %restart_error, "completions_full: bridge restart failed");
                }
            }
            return Err(format!("semantic completion bridge failed: {e}"));
        }
    };
    if bridge_items.is_empty() {
        return Ok(items);
    }

    fn completion_kind_from_str(s: &str) -> CompletionKind {
        match s {
            "Method" | "Function" => CompletionKind::Method,
            "Property" | "Field" => CompletionKind::Field,
            "Variable" => CompletionKind::Variable,
            "Enum" | "EnumMember" => CompletionKind::EnumMember,
            "Class" | "Struct" => CompletionKind::Class,
            "Module" | "Namespace" => CompletionKind::Module,
            "Keyword" => CompletionKind::Keyword,
            "Snippet" => CompletionKind::Snippet,
            _ => CompletionKind::Text,
        }
    }

    tracing::debug!(
        count = bridge_items.len(),
        "completions_full: bridge results"
    );
    Ok(bridge_items
        .into_iter()
        .map(|item| CompletionEntry {
            sort_text: Some(format!("2_{}", item.label.to_ascii_lowercase())),
            kind: completion_kind_from_str(&item.kind),
            detail: item.detail,
            documentation: item.documentation,
            label: item.label,
            insert_text: None,
        })
        .collect())
}

fn add_default_completions(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    items: &mut Vec<CompletionEntry>,
) -> Result<(), al_workspace::WorkspaceStateError> {
    let kw_data = al_syntax::language_data::keywords();
    for entry in kw_data.control.iter().chain(kw_data.operator.iter()) {
        items.push(CompletionEntry {
            label: entry.keyword.clone(),
            kind: CompletionKind::Keyword,
            detail: None,
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }

    if let Some((file_text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) {
        // Prefer cached symbols populated by the file index; fall back to a
        // fresh extraction only for documents not stored on disk. Avoids the
        // full AST walk on every keystroke.
        let file_path = uri.to_file_path().ok();
        let doc_symbols: Vec<super::AlDocumentSymbol> = file_path
            .as_ref()
            .and_then(|p| workspace.file_index.get_cached_symbols(p))
            .map(|syms| syms.into_iter().map(Into::into).collect())
            .unwrap_or_else(|| {
                al_syntax::extract_document_symbols(&tree, &file_text)
                    .into_iter()
                    .map(Into::into)
                    .collect()
            });
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if super::is_procedure_symbol(child.kind) {
                        items.push(CompletionEntry {
                            label: child.name.clone(),
                            kind: CompletionKind::Function,
                            detail: child.detail.clone(),
                            documentation: None,
                            insert_text: None,
                            sort_text: None,
                        });
                    }
                }
            }
        }

        let resolver = al_syntax::type_resolver::TypeResolver::new(&tree, &file_text);
        let vars = resolver.variables_at(position.into());
        for var in &vars {
            let subtype = var
                .type_subtype
                .as_ref()
                .map(|s| format!(" \"{}\"", s))
                .unwrap_or_default();
            let label = super::scope_label(&var.scope);
            items.push(CompletionEntry {
                label: var.name.clone(),
                kind: CompletionKind::Variable,
                detail: Some(format!("{}{} ({})", var.type_name, subtype, label)),
                documentation: None,
                insert_text: None,
                sort_text: Some(format!("0_{}", var.name)),
            });
        }
    }

    // Use the precomputed cache instead of scanning all indexed symbols.
    let index_results = workspace.symbols.default_completions_snapshot();
    for entry in &index_results {
        let kind = match entry.kind {
            al_symbols::ObjectKind::Table | al_symbols::ObjectKind::TableExtension => {
                CompletionKind::Struct
            }
            al_symbols::ObjectKind::Codeunit => CompletionKind::Module,
            al_symbols::ObjectKind::Page | al_symbols::ObjectKind::PageExtension => {
                CompletionKind::Class
            }
            al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension => {
                CompletionKind::Enum
            }
            _ => CompletionKind::Reference,
        };
        items.push(CompletionEntry {
            label: entry.name.clone(),
            kind,
            detail: Some(format!("{} {}", entry.kind, entry.id)),
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }

    let builtins =
        workspace
            .builtins
            .read()
            .map_err(|_| al_workspace::WorkspaceStateError::Poisoned {
                component: "builtins",
            })?;
    for bt in builtins.iter() {
        items.push(CompletionEntry {
            label: bt.name.clone(),
            kind: CompletionKind::Class,
            detail: Some("built-in type".to_string()),
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }
    drop(builtins); // release read lock promptly
    Ok(())
}

/// How many object names a single type-position request offers per kind.
const TYPE_COMPLETION_CAP: usize = 50;

/// The partially typed name immediately before the cursor in a type position.
///
/// `var Cust: Record Cust` yields `Cust`. An unclosed quote takes everything
/// after it, so `Record "Sales He` yields `Sales He` — an AL quoted name may
/// contain spaces, and stopping at the space would filter on the wrong word.
fn typed_type_prefix(text: &str, position: Position) -> String {
    let Some(line) = text.lines().nth(position.line as usize) else {
        return String::new();
    };
    let byte = al_syntax::utf16_col_to_byte_offset(line, position.character as usize);
    let before = &line[..byte];
    if before.matches('"').count() % 2 == 1 {
        let quote = before.rfind('"').expect("odd count means one is present");
        return before[quote + 1..].to_string();
    }
    let mut prefix: Vec<char> = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    prefix.reverse();
    prefix.into_iter().collect()
}

/// True when AL requires the name to be written in quotes.
fn needs_quoting(name: &str) -> bool {
    name.is_empty()
        || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Objects of `kind` worth offering for the partially typed `prefix`, ranked
/// and capped.
///
/// Prefix matches come before mid-name matches, then alphabetical, so the cap
/// keeps the names the user is most likely reaching for instead of whichever
/// ones the symbol index happened to load first.
fn matching_type_symbols(
    workspace: &Workspace,
    kind: al_symbols::ObjectKind,
    prefix: &str,
) -> Vec<std::sync::Arc<al_symbols::SymbolEntry>> {
    let prefix_lower = prefix.to_lowercase();
    let mut ranked: Vec<(u8, String, std::sync::Arc<al_symbols::SymbolEntry>)> = workspace
        .symbols
        .get_by_kind(kind)
        .into_iter()
        .filter(|arc| !arc.synthetic)
        .filter_map(|arc| {
            let name_lower = arc.name.to_lowercase();
            let rank = if prefix_lower.is_empty() {
                1
            } else if name_lower.starts_with(&prefix_lower) {
                0
            } else if name_lower.contains(&prefix_lower) {
                1
            } else {
                return None;
            };
            Some((rank, name_lower, arc))
        })
        .collect();
    ranked.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    ranked.truncate(TYPE_COMPLETION_CAP);
    ranked.into_iter().map(|(_, _, arc)| arc).collect()
}

fn finalize_completion_items(items: &mut Vec<CompletionEntry>) {
    // Sort so that non-keyword items precede keywords before dedup, ensuring
    // a workspace procedure with the same name as a keyword is not shadowed.
    items.sort_by_key(|item| {
        if item.kind == CompletionKind::Keyword {
            1u8
        } else {
            0u8
        }
    });
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.label.to_lowercase()));

    for item in items.iter_mut() {
        if item.sort_text.is_some() {
            continue;
        }
        let label_lower = item.label.to_lowercase();
        let is_callable = matches!(item.kind, CompletionKind::Function | CompletionKind::Method);
        if is_callable {
            let pc = item
                .detail
                .as_deref()
                .map(|d| super::parse_detail_params(d).len())
                .unwrap_or(0);
            item.sort_text = Some(format!("1_{pc:02}_{label_lower}"));
        } else {
            item.sort_text = Some(format!("1_{label_lower}"));
        }
    }

    // sort_text is now populated for every item, so the fallback
    // to a redundant label lowercase comparison is unnecessary.
    items.sort_by(|a, b| {
        a.sort_text
            .as_deref()
            .unwrap_or("")
            .cmp(b.sort_text.as_deref().unwrap_or(""))
    });
}

/// Convert a tower-lsp CompletionItem to our transport-agnostic type.
fn from_lsp_completion(item: resolution::CompletionCandidate) -> CompletionEntry {
    let kind = match item.kind {
        resolution::CompletionCandidateKind::Variable => CompletionKind::Variable,
        resolution::CompletionCandidateKind::Method => CompletionKind::Method,
        resolution::CompletionCandidateKind::Field => CompletionKind::Field,
        resolution::CompletionCandidateKind::EnumMember => CompletionKind::EnumMember,
    };
    CompletionEntry {
        label: item.label,
        kind,
        detail: item.detail,
        documentation: item.documentation,
        insert_text: item.insert_text,
        sort_text: item.sort_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn completions(workspace: &Workspace, uri: &Url, position: Position) -> Vec<CompletionEntry> {
        super::completions(workspace, uri, position).unwrap()
    }

    fn add_default_completions(
        workspace: &Workspace,
        uri: &Url,
        position: Position,
        items: &mut Vec<CompletionEntry>,
    ) {
        super::add_default_completions(workspace, uri, position, items).unwrap();
    }

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    #[test]
    fn add_default_completions_signature_is_transport_agnostic() {
        // Compile-time guard against re-introducing the lsp_types boundary leak
        // at this boundary. If anyone widens the parameter back to
        // tower_lsp::lsp_types::Position the function pointer coercion below
        // will fail to type-check.
        let _: fn(&Workspace, &Url, Position, &mut Vec<CompletionEntry>) = add_default_completions;
    }

    #[test]
    fn typed_type_prefix_reads_the_partial_name_at_the_cursor() {
        let unquoted = "codeunit 50100 X\n{\n    var\n        Cust: Record Cust\n}\n";
        assert_eq!(
            typed_type_prefix(
                unquoted,
                Position {
                    line: 3,
                    character: 29
                }
            ),
            "Cust"
        );

        // An AL quoted name may contain spaces, so an open quote takes
        // everything after it rather than stopping at the space.
        let quoted = "codeunit 50100 X\n{\n    var\n        H: Record \"Sales He\n}\n";
        assert_eq!(
            typed_type_prefix(
                quoted,
                Position {
                    line: 3,
                    character: 27
                }
            ),
            "Sales He"
        );

        // A closed quote is not a partial name.
        let closed = "codeunit 50100 X\n{\n    var\n        H: Record \"Sales Header\";\n}\n";
        assert_eq!(
            typed_type_prefix(
                closed,
                Position {
                    line: 3,
                    character: 32
                }
            ),
            ""
        );
    }

    #[test]
    fn needs_quoting_matches_al_identifier_rules() {
        assert!(!needs_quoting("Customer"));
        assert!(!needs_quoting("My_Table2"));
        assert!(needs_quoting("Sales Header"));
        assert!(needs_quoting("Sales-Post"));
        assert!(needs_quoting("2Fast"));
    }

    /// `get_by_kind` returns the per-kind vector in index insertion order, so
    /// taking the first 50 of thousands of Base Application tables offered
    /// whichever loaded first and the client had no way to recover the rest.
    #[test]
    fn type_position_filters_by_what_has_been_typed_and_ranks_prefix_matches() {
        let workspace = Workspace::new();
        let mut entries: Vec<al_symbols::SymbolEntry> = (0..80)
            .map(|index| al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Table,
                id: 50_000 + index,
                name: format!("Zzz Filler {index:02}"),
                ..Default::default()
            })
            .collect();
        for (id, name) in [
            (18, "Customer"),
            (36, "Sales Header"),
            (112, "Posted Customer Entry"),
        ] {
            entries.push(al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Table,
                id,
                name: name.to_string(),
                ..Default::default()
            });
        }
        workspace.symbols.add_entries_owned(entries);

        let names = |prefix: &str| {
            matching_type_symbols(&workspace, al_symbols::ObjectKind::Table, prefix)
                .into_iter()
                .map(|arc| arc.name.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names("Cust"),
            vec!["Customer".to_string(), "Posted Customer Entry".to_string()],
            "a prefix match outranks a mid-name match, and nothing else matches"
        );
        assert_eq!(names("Sales He"), vec!["Sales Header".to_string()]);
        assert!(names("Vendor").is_empty());
        assert_eq!(
            names("").len(),
            TYPE_COMPLETION_CAP,
            "an empty prefix still caps"
        );
        assert_eq!(
            names("")[0],
            "Customer",
            "and the cap keeps a deterministic, alphabetical slice"
        );
    }

    /// The label always carried the quotes, so a user who had typed `Cust` got
    /// no match from a client filtering against `"Customer"`.
    #[test]
    fn type_position_labels_are_bare_and_quote_only_in_insert_text() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries_owned(vec![
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Table,
                id: 18,
                name: "Customer".to_string(),
                ..Default::default()
            },
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Table,
                id: 36,
                name: "Sales Header".to_string(),
                ..Default::default()
            },
        ]);

        let uri = test_uri();
        let source = "codeunit 50100 X\n{\n    var\n        C: Record \n}\n";
        workspace
            .documents
            .open(uri.clone(), source.to_string())
            .unwrap();
        let items = completions(
            &workspace,
            &uri,
            Position {
                line: 3,
                character: 19,
            },
        );

        let find = |label: &str| {
            items
                .iter()
                .find(|item| item.label == label)
                .unwrap_or_else(|| {
                    panic!(
                        "no completion labelled {label}: {:?}",
                        items.iter().map(|i| &i.label).collect::<Vec<_>>()
                    )
                })
        };
        assert_eq!(find("Customer").insert_text, None, "no quotes needed");
        assert_eq!(
            find("Sales Header").insert_text.as_deref(),
            Some("\"Sales Header\""),
            "a name with a space is inserted quoted"
        );
    }

    #[test]
    fn completion_kind_serializes_to_lsp_integer() {
        assert_eq!(serde_json::to_value(CompletionKind::Function).unwrap(), 3);
        assert_eq!(serde_json::to_value(CompletionKind::Field).unwrap(), 5);
        assert_eq!(serde_json::to_value(CompletionKind::Variable).unwrap(), 6);
        assert_eq!(serde_json::to_value(CompletionKind::Class).unwrap(), 7);
        assert_eq!(serde_json::to_value(CompletionKind::Keyword).unwrap(), 14);
    }

    #[test]
    fn count_params_works() {
        assert_eq!(super::super::parse_detail_params("()").len(), 0);
        assert_eq!(super::super::parse_detail_params("").len(), 0);
        assert_eq!(super::super::parse_detail_params("(A: Text)").len(), 1);
        assert_eq!(
            super::super::parse_detail_params("(A: Text; B: Integer)").len(),
            2
        );
        assert_eq!(
            super::super::parse_detail_params("(A: Text; B: Integer; C: Boolean)").len(),
            3
        );
        assert_eq!(
            super::super::parse_detail_params("(A: List of [Text]; B: Integer)").len(),
            2
        );
        assert_eq!(
            super::super::parse_detail_params("(A: Text): Boolean").len(),
            1
        );
    }

    #[test]
    fn completions_empty_for_unopened_document() {
        let ws = Workspace::new();
        let uri = test_uri();
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        assert!(
            result.is_empty(),
            "unopened document should return empty completions"
        );
    }

    #[test]
    fn completions_on_empty_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents.open(uri.clone(), String::new()).unwrap();
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        // Empty file — may return keywords but should not panic
        let _ = result;
    }

    #[test]
    fn completions_on_malformed_al() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(uri.clone(), "{{{{not valid al code}}}}".to_string())
            .unwrap();
        let pos = Position {
            line: 0,
            character: 5,
        };
        let result = completions(&ws, &uri, pos);
        // Should not panic on malformed code
        let _ = result;
    }

    #[test]
    fn completions_at_line_beyond_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(uri.clone(), "codeunit 50100 \"X\" { }".to_string())
            .unwrap();
        // Line 100 doesn't exist — should return empty, not panic
        let pos = Position {
            line: 100,
            character: 0,
        };
        let result = completions(&ws, &uri, pos);
        let _ = result; // just ensure no panic
    }

    #[test]
    fn completions_include_keywords_in_begin_block() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(
                uri.clone(),
                r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin

    end;
}"#
                .to_string(),
            )
            .unwrap();
        let pos = Position {
            line: 4,
            character: 8,
        }; // inside begin block
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"if"),
            "should include 'if' keyword, got: {:?}",
            labels
        );
        assert!(
            labels.contains(&"repeat"),
            "should include 'repeat' keyword"
        );
    }

    fn entry(label: &str, kind: CompletionKind) -> CompletionEntry {
        CompletionEntry {
            label: label.to_string(),
            kind,
            detail: None,
            documentation: None,
            insert_text: None,
            sort_text: None,
        }
    }

    #[test]
    fn completion_kind_serializes_all_variants() {
        // Covers every arm of the hand-written Serialize impl, including the
        // less common kinds the original test omitted.
        let cases = [
            (CompletionKind::Text, 1u32),
            (CompletionKind::Method, 2),
            (CompletionKind::Function, 3),
            (CompletionKind::Field, 5),
            (CompletionKind::Variable, 6),
            (CompletionKind::Class, 7),
            (CompletionKind::Module, 9),
            (CompletionKind::Property, 10),
            (CompletionKind::Value, 12),
            (CompletionKind::Enum, 13),
            (CompletionKind::Keyword, 14),
            (CompletionKind::Snippet, 15),
            (CompletionKind::Reference, 18),
            (CompletionKind::EnumMember, 20),
            (CompletionKind::Struct, 22),
        ];
        for (kind, expected) in cases {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                expected,
                "{kind:?} should serialize to {expected}"
            );
        }
    }

    #[test]
    fn finalize_dedups_case_insensitively() {
        let mut items = vec![
            entry("MyProc", CompletionKind::Function),
            entry("myproc", CompletionKind::Variable),
            entry("Other", CompletionKind::Variable),
        ];
        finalize_completion_items(&mut items);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels
                .iter()
                .filter(|l| l.eq_ignore_ascii_case("myproc"))
                .count(),
            1,
            "case-insensitive duplicate labels must collapse to one, got: {labels:?}"
        );
        assert!(labels.contains(&"Other"));
    }

    #[test]
    fn finalize_keeps_non_keyword_over_keyword_on_collision() {
        // A workspace procedure that collides with a keyword name must survive
        // dedup, because non-keywords are sorted ahead of keywords first.
        let mut items = vec![
            entry("if", CompletionKind::Keyword),
            entry("if", CompletionKind::Function),
        ];
        finalize_completion_items(&mut items);
        assert_eq!(items.len(), 1, "collision should leave a single entry");
        assert_eq!(
            items[0].kind,
            CompletionKind::Function,
            "the non-keyword entry must win the collision, not the keyword"
        );
    }

    #[test]
    fn finalize_callable_sort_text_encodes_param_count() {
        // Callables get a "1_<paramcount:02>_<label>" sort key derived from the
        // detail's parameter list, so 0-param overloads sort before 2-param ones.
        let mut zero = entry("Run", CompletionKind::Method);
        zero.detail = Some("()".to_string());
        let mut two = entry("Calc", CompletionKind::Function);
        two.detail = Some("(A: Text; B: Integer)".to_string());
        let mut items = vec![two, zero];
        finalize_completion_items(&mut items);

        let run = items.iter().find(|i| i.label == "Run").unwrap();
        let calc = items.iter().find(|i| i.label == "Calc").unwrap();
        assert_eq!(run.sort_text.as_deref(), Some("1_00_run"));
        assert_eq!(calc.sort_text.as_deref(), Some("1_02_calc"));
    }

    #[test]
    fn finalize_non_callable_sort_text_omits_param_count() {
        let mut items = vec![entry("MyVar", CompletionKind::Variable)];
        finalize_completion_items(&mut items);
        assert_eq!(items[0].sort_text.as_deref(), Some("1_myvar"));
    }

    #[test]
    fn finalize_preserves_existing_sort_text() {
        // Items that already carry a sort_text (e.g. locals tagged "0_") must
        // not be overwritten by the callable/non-callable fallback.
        let mut item = entry("Local", CompletionKind::Variable);
        item.sort_text = Some("0_Local".to_string());
        let mut items = vec![item];
        finalize_completion_items(&mut items);
        assert_eq!(
            items[0].sort_text.as_deref(),
            Some("0_Local"),
            "pre-set sort_text must be preserved"
        );
    }

    #[test]
    fn finalize_orders_by_sort_text() {
        // After finalize, items are sorted by sort_text. A local (0_) precedes a
        // bridge result (2_) precedes a generic keyword/non-callable (1_).
        let mut local = entry("zzz", CompletionKind::Variable);
        local.sort_text = Some("0_zzz".to_string());
        let mut bridge = entry("aaa", CompletionKind::Variable);
        bridge.sort_text = Some("2_aaa".to_string());
        let plain = entry("mmm", CompletionKind::Variable); // becomes "1_mmm"
        let mut items = vec![bridge, plain, local];
        finalize_completion_items(&mut items);
        let order: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(order, vec!["zzz", "mmm", "aaa"]);
    }

    #[test]
    fn from_lsp_completion_maps_all_kinds() {
        use resolution::CompletionCandidateKind as K;
        let cases = [
            (K::Variable, CompletionKind::Variable),
            (K::Method, CompletionKind::Method),
            (K::Field, CompletionKind::Field),
            (K::EnumMember, CompletionKind::EnumMember),
        ];
        for (src, expected) in cases {
            let cand = resolution::CompletionCandidate {
                label: "X".to_string(),
                kind: src,
                detail: None,
                documentation: None,
                insert_text: None,
                sort_text: None,
            };
            assert_eq!(from_lsp_completion(cand).kind, expected, "{src:?}");
        }
    }

    #[test]
    fn from_lsp_completion_passes_through_fields() {
        let cand = resolution::CompletionCandidate {
            label: "Foo".to_string(),
            kind: resolution::CompletionCandidateKind::Method,
            detail: Some("(A: Text)".to_string()),
            documentation: Some("docs".to_string()),
            insert_text: Some("Foo()".to_string()),
            sort_text: Some("1_foo".to_string()),
        };
        let out = from_lsp_completion(cand);
        assert_eq!(out.label, "Foo");
        assert_eq!(out.detail.as_deref(), Some("(A: Text)"));
        assert_eq!(out.documentation.as_deref(), Some("docs"));
        assert_eq!(out.insert_text.as_deref(), Some("Foo()"));
        assert_eq!(out.sort_text.as_deref(), Some("1_foo"));
    }

    #[test]
    fn completions_include_local_procedures() {
        let ws = Workspace::new();
        let uri = test_uri();
        ws.documents
            .open(
                uri.clone(),
                r#"codeunit 50100 "Test"
{
    procedure Helper()
    begin
    end;

    procedure Caller()
    begin

    end;
}"#
                .to_string(),
            )
            .unwrap();
        let pos = Position {
            line: 8,
            character: 8,
        };
        let result = completions(&ws, &uri, pos);
        let labels: Vec<&str> = result.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"Helper"),
            "should include local procedure 'Helper', got: {:?}",
            labels
        );
    }
}
