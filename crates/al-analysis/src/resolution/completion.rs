//! What a resolved receiver offers at a completion request.
//!
//! The candidates come from workspace globals, procedures and fields, from the
//! composed members of a symbol-package object, and from the CodeAnalysis
//! builtin catalog. Documentation for symbol-package procedures is parsed once
//! per symbol-index generation, because otherwise every keystroke after a `.`
//! re-read and re-parsed several thousand lines of extracted source.

use std::path::{Path, PathBuf};

use al_workspace::{Workspace, WorkspaceStateError};

use crate::queries::{AlSymbolKind, Position};

use super::fields::{field_decl_nodes, parse_field_node};
use super::members::{builtin_for, composed_members_for, RECORD_SYSTEM_FIELDS};
use super::type_text::{
    extract_doc_comment, format_builtin_signature, format_method_signature, format_type_detail,
};
use super::workspace_objects::resolve_object_path;
use super::xml_doc::format_xml_doc;
use super::ResolvedType;

/// Transport-agnostic completion candidate returned by resolution helpers.
/// Callers in al-lsp convert this to `tower_lsp::lsp_types::CompletionItem`.
#[derive(Debug, Clone)]
pub(crate) struct CompletionCandidate {
    pub label: String,
    pub kind: CompletionCandidateKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub insert_text: Option<String>,
    pub sort_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionCandidateKind {
    Variable,
    Method,
    Field,
    EnumMember,
}

/// `procedure name (lowercased) -> formatted XML doc` for one symbol-package
/// source file.
type ProcDocs = std::sync::Arc<std::collections::HashMap<String, String>>;

/// Documentation maps per symbol-package source file, with the symbol-index
/// generation they were built from.
///
/// `completion_items_for_receiver` runs on every `textDocument/completion`
/// request, so without this each keystroke after `Cust.` re-read the extracted
/// Customer source from disk twice and ran a full tree-sitter parse plus
/// document-symbol extraction over several thousand lines, all of it to fill
/// in the `documentation` field of the completion items. The extracted source
/// only changes when the package does, which is what the generation tracks.
static SYMBOL_PACKAGE_DOCS: std::sync::LazyLock<
    std::sync::RwLock<std::collections::HashMap<PathBuf, (u64, ProcDocs)>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashMap::new()));

/// The documentation map for one already-extracted symbol-package source file.
fn proc_docs_for_file(path: &Path, generation: u64) -> ProcDocs {
    {
        let cache = SYMBOL_PACKAGE_DOCS
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached_generation, docs)) = cache.get(path) {
            if *cached_generation == generation {
                return std::sync::Arc::clone(docs);
            }
        }
    }
    let mut map = std::collections::HashMap::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        let result = al_syntax::AlParser::parse_quick(&content);
        for symbol in al_syntax::extract_document_symbols(&result.tree, &content) {
            let Some(children) = symbol.children else {
                continue;
            };
            for child in children {
                if !crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind)) {
                    continue;
                }
                if let Some(doc) =
                    extract_doc_comment(&content, child.selection_range.start.line as usize)
                {
                    map.entry(child.name.to_lowercase())
                        .or_insert_with(|| format_xml_doc(&doc));
                }
            }
        }
    }
    let docs: ProcDocs = std::sync::Arc::new(map);
    let mut cache = SYMBOL_PACKAGE_DOCS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        path.to_path_buf(),
        (generation, std::sync::Arc::clone(&docs)),
    );
    docs
}

/// Procedure documentation for a symbol-package object, one map per source
/// file the package ships for it (the same sources go-to-definition opens).
/// Empty when the package ships none, and completion then shows no
/// documentation.
fn symbol_package_proc_docs(workspace: &Workspace, object_name: &str) -> Vec<ProcDocs> {
    let generation = workspace.symbols.generation();
    workspace
        .symbols
        .get_by_name(object_name)
        .into_iter()
        .filter_map(|entry| {
            let path = crate::queries::virtual_file_path(workspace, &entry)?;
            Some(proc_docs_for_file(&path, generation))
        })
        .filter(|docs| !docs.is_empty())
        .collect()
}

/// The documentation for `name` in the first map that carries it.
fn proc_doc(docs: &[ProcDocs], name: &str) -> Option<String> {
    if docs.is_empty() {
        return None;
    }
    let key = name.to_lowercase();
    docs.iter().find_map(|map| map.get(&key).cloned())
}

/// Resolution primitive: given a [`super::resolve_expression_type`]-resolved receiver,
/// return the transport-agnostic completion candidates available on it (workspace
/// globals + procedures + fields, composed `.app` members, builtin methods from
/// the semantic cache). The query layer (`queries::completions`) orchestrates this
/// with `resolve_expression_type` and converts the result to LSP `CompletionItem`s.
///
/// Deliberately kept here, not in `queries/`: it is a peer of
/// `resolve_expression_type`, returns the resolution-owned (already
/// transport-agnostic) [`CompletionCandidate`], and depends on six private
/// resolution helpers (`resolve_object_path`, `composed_members_for`,
/// `format_type_detail`/`format_method_signature`/`format_builtin_signature`,
/// `workspace_field_items`). Moving it would force those internals to `pub(crate)`
/// and split two tightly-coupled resolution calls across the layer boundary —
/// increasing coupling, not reducing it.
pub(crate) fn completion_items_for_receiver(
    workspace: &Workspace,
    receiver: &ResolvedType,
) -> Result<Vec<CompletionCandidate>, WorkspaceStateError> {
    tracing::debug!(
        receiver = %receiver.type_name,
        receiver_subtype = ?receiver.type_subtype,
        "completion_items_for_receiver: start"
    );
    let mut items = Vec::new();
    let mut workspace_vars = 0usize;
    let mut workspace_symbols = 0usize;
    let mut workspace_fields = 0usize;
    let mut index_methods = 0usize;
    let mut index_fields = 0usize;
    let mut builtin_methods = 0usize;

    if let Some(subtype) = receiver.type_subtype.as_deref() {
        if let Some(path) = resolve_object_path(workspace, None, subtype, Some(&receiver.type_name))
        {
            if let Some((file_text, tree)) = workspace.file_index.get_cached_parse(&path) {
                let resolver = al_syntax::TypeResolver::new(&tree, &file_text);
                for var in resolver.variables_at(Position::default().into()) {
                    if var.scope != al_syntax::VariableScope::Global {
                        continue;
                    }
                    workspace_vars += 1;
                    items.push(CompletionCandidate {
                        label: var.name.clone(),
                        kind: CompletionCandidateKind::Variable,
                        detail: Some(format_type_detail(
                            &var.type_name,
                            var.type_subtype.as_deref(),
                        )),
                        documentation: None,
                        insert_text: None,
                        sort_text: None,
                    });
                }

                for symbol in al_syntax::extract_document_symbols(&tree, &file_text) {
                    if let Some(children) = symbol.children {
                        for child in children {
                            if crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind)) {
                                workspace_symbols += 1;
                                let documentation = extract_doc_comment(
                                    &file_text,
                                    child.selection_range.start.line as usize,
                                )
                                .map(|d| format_xml_doc(&d));
                                items.push(CompletionCandidate {
                                    label: child.name,
                                    kind: CompletionCandidateKind::Method,
                                    detail: child.detail,
                                    documentation,
                                    insert_text: None,
                                    sort_text: None,
                                });
                            }
                        }
                    }
                }

                let field_items = workspace_field_items(&file_text, &tree);
                workspace_fields = field_items.len();
                for field in field_items {
                    items.push(field);
                }
            }
        }

        let pkg_docs = symbol_package_proc_docs(workspace, subtype);
        for members in composed_members_for(workspace, subtype) {
            for method in &members.methods {
                if method.is_local {
                    continue;
                }
                index_methods += 1;
                items.push(CompletionCandidate {
                    label: method.name.clone(),
                    kind: CompletionCandidateKind::Method,
                    detail: Some(format_method_signature(
                        method.name.as_str(),
                        &method.parameters,
                        method.return_type.as_deref(),
                    )),
                    documentation: proc_doc(&pkg_docs, &method.name),
                    insert_text: None,
                    sort_text: None,
                });
            }
            for field in &members.fields {
                index_fields += 1;
                items.push(CompletionCandidate {
                    label: field.name.clone(),
                    kind: CompletionCandidateKind::Field,
                    detail: Some(field.type_name.clone()),
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
        }
    }

    if receiver.type_name.eq_ignore_ascii_case("Record") && receiver.type_subtype.is_some() {
        for (name, type_name) in RECORD_SYSTEM_FIELDS {
            items.push(CompletionCandidate {
                label: name.to_string(),
                kind: CompletionCandidateKind::Field,
                detail: Some(type_name.to_string()),
                documentation: Some("Platform-owned system field".to_string()),
                insert_text: None,
                sort_text: None,
            });
        }
    }

    let cache = workspace
        .semantic_cache
        .read()
        .map_err(|_| WorkspaceStateError::Poisoned {
            component: "semantic_cache",
        })?;
    if let Some(builtin) = builtin_for(&cache, receiver) {
        for method in &builtin.methods {
            builtin_methods += 1;
            items.push(CompletionCandidate {
                label: method.name.clone(),
                kind: CompletionCandidateKind::Method,
                detail: Some(format_builtin_signature(method)),
                documentation: if method.documentation.is_empty() {
                    None
                } else {
                    Some(method.documentation.clone())
                },
                insert_text: None,
                sort_text: None,
            });
        }
    }
    drop(cache);

    if builtin_methods == 0 && receiver.type_name.eq_ignore_ascii_case("Record") {
        // The semantic cache carries `TableClass` only once a Microsoft AL
        // toolchain has been found and its metadata extracted. An editor
        // install usually has no toolchain, and `Customer.` then offered the
        // table's own fields and procedures but not one platform method. The
        // bundled catalog was generated from that same `TableClass` metadata,
        // so use it when the cache has nothing. Names only: a real cache
        // supplies signatures, and its entries are added first, so dedup keeps
        // them.
        for name in al_syntax::language_data::record_methods() {
            builtin_methods += 1;
            items.push(CompletionCandidate {
                label: name.clone(),
                kind: CompletionCandidateKind::Method,
                detail: Some("Record method".to_string()),
                documentation: None,
                insert_text: None,
                sort_text: None,
            });
        }
    }

    tracing::debug!(
        receiver = %receiver.type_name,
        workspace_vars,
        workspace_symbols,
        workspace_fields,
        index_methods,
        index_fields,
        builtin_methods,
        total = items.len(),
        "completion_items_for_receiver: done"
    );
    Ok(items)
}

pub(crate) fn enum_completion_items(
    workspace: &Workspace,
    enum_type: &ResolvedType,
) -> Result<Vec<CompletionCandidate>, WorkspaceStateError> {
    // For enum access, the name might be the type_name (for system enums used directly)
    // or the type_subtype (for Enum "MyEnum" declarations)
    let enum_name = enum_type
        .type_subtype
        .as_deref()
        .unwrap_or(&enum_type.type_name);
    tracing::debug!(enum_name = %enum_name, type_name = %enum_type.type_name, "enum_completion_items: start");

    let mut items = Vec::new();
    let mut workspace_values = 0usize;
    let mut index_values = 0usize;
    let mut builtin_values = 0usize;

    if let Some(path) = workspace
        .file_index
        .object_path_of_kind(enum_name, &["enum", "enumextension"])
    {
        if let Some((file_text, tree)) = workspace.file_index.get_cached_parse(&path) {
            for symbol in al_syntax::extract_document_symbols(&tree, &file_text) {
                if !symbol.name.eq_ignore_ascii_case(enum_name) {
                    continue;
                }
                if let Some(children) = symbol.children {
                    for child in children {
                        if AlSymbolKind::from(child.kind) == AlSymbolKind::EnumMember {
                            workspace_values += 1;
                            items.push(CompletionCandidate {
                                label: child.name,
                                kind: CompletionCandidateKind::EnumMember,
                                detail: child.detail,
                                documentation: None,
                                insert_text: None,
                                sort_text: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Check package symbol index. Use the composed view so values added by
    // EnumExtension objects (indexed under their own names) are included.
    let mut composed_enum_values: Vec<al_symbols::EnumValueSymbol> = Vec::new();
    if let Some(composed) = workspace
        .symbols
        .get_composed_cached(al_symbols::ObjectKind::Enum, enum_name)
    {
        composed_enum_values = composed.all_enum_values.clone();
    }
    // Fall back to (or add) raw entries for any matching EnumExtension that
    // shares the queried name and isn't covered by a base enum composition.
    if composed_enum_values.is_empty() {
        for entry in workspace.symbols.get_by_name(enum_name) {
            if !matches!(
                entry.kind,
                al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension
            ) {
                continue;
            }
            composed_enum_values.extend(entry.enum_values.iter().cloned());
        }
    }
    for value in &composed_enum_values {
        index_values += 1;
        items.push(CompletionCandidate {
            label: value.name.clone(),
            kind: CompletionCandidateKind::EnumMember,
            detail: Some(format!("value({})", value.ordinal)),
            documentation: None,
            insert_text: None,
            sort_text: None,
        });
    }

    // Check builtin types for system enums (e.g., TextEncoding, WebServiceActionResultCode)
    if items.is_empty() {
        let cache = workspace
            .semantic_cache
            .read()
            .map_err(|_| WorkspaceStateError::Poisoned {
                component: "semantic_cache",
            })?;
        if let Some(bt) = cache.get_type(enum_name) {
            if !bt.enum_values.is_empty() {
                for value in &bt.enum_values {
                    builtin_values += 1;
                    items.push(CompletionCandidate {
                        label: value.clone(),
                        kind: CompletionCandidateKind::EnumMember,
                        detail: Some(format!("{}::{}", bt.name, value)),
                        documentation: None,
                        insert_text: None,
                        sort_text: None,
                    });
                }
            }
        }
    }

    tracing::debug!(
        enum_name = %enum_name,
        workspace_values,
        index_values,
        builtin_values,
        total = items.len(),
        "enum_completion_items: done"
    );
    Ok(items)
}

fn workspace_field_items(content: &str, tree: &tree_sitter::Tree) -> Vec<CompletionCandidate> {
    let src = content.as_bytes();
    field_decl_nodes(tree, src)
        .into_iter()
        .filter_map(|node| {
            let (name_part, ty, _, _) = parse_field_node(node, content)?;
            Some(CompletionCandidate {
                label: al_syntax::clean_identifier(name_part),
                kind: CompletionCandidateKind::Field,
                detail: Some(ty.to_string()),
                documentation: None,
                insert_text: None,
                sort_text: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::MethodSymbol;

    use crate::resolution::test_support::{
        completion_items_for_receiver, enum_completion_items, enum_entry, enum_ext_entry,
        enum_value, field, table_entry, table_ext_entry, tree_of, workspace_with,
    };

    /// The documentation map is read and parsed once per generation. Deleting
    /// the file between the two calls proves the second one did not go to disk,
    /// and bumping the generation proves a package reload invalidates it.
    #[test]
    fn symbol_package_docs_are_parsed_once_per_generation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("Cod50100.al");
        std::fs::write(
        &path,
        "codeunit 50100 \"Helper\"\n{\n    /// <summary>Posts the document.</summary>\n    procedure Post()\n    begin\n    end;\n}\n",
    )
    .expect("write");

        let first = proc_docs_for_file(&path, 7);
        assert!(first.contains_key("post"), "got {first:?}");

        std::fs::remove_file(&path).expect("remove");
        let second = proc_docs_for_file(&path, 7);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "the second lookup re-read and re-parsed the source"
        );

        // A new package generation drops what the old one produced.
        let after_reload = proc_docs_for_file(&path, 8);
        assert!(after_reload.is_empty());
    }

    #[test]
    fn workspace_field_items_lists_fields() {
        let text = "table 1 T\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n        field(2; \"Ørn\"; Integer) { }\n        field(50; \"Amount (LCY)\"; Decimal) { }\n    }\n}";
        let items = workspace_field_items(text, &tree_of(text));
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Name"));
        assert!(labels.contains(&"Ørn"));
        assert!(labels.contains(&"Amount (LCY)"), "got {labels:?}");
    }

    #[test]
    fn completion_items_include_extension_added_members() {
        let ws = workspace_with(vec![
            table_entry(18, "Customer", vec![field(1, "No.", "Code")]),
            table_ext_entry(
                50100,
                "Cust Ext",
                "Customer",
                vec![field(50100, "Loyalty Points", "Integer")],
                vec![MethodSymbol {
                    name: "AddPoints".into(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                }],
            ),
        ]);
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let items = completion_items_for_receiver(&ws, &receiver);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"No."), "base field present");
        assert!(
            labels.contains(&"Loyalty Points"),
            "extension field present"
        );
        assert!(labels.contains(&"AddPoints"), "extension method present");
    }

    /// Without a Microsoft AL toolchain the semantic cache is empty, which is
    /// what an editor install and a CI runner both look like. Member
    /// completion on a Record must still offer the platform methods.
    #[test]
    fn record_completion_offers_platform_methods_with_an_empty_semantic_cache() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code")],
        )]);
        assert!(
            ws.semantic_cache.read().unwrap().is_empty(),
            "this test is about the no-toolchain case"
        );
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let items = completion_items_for_receiver(&ws, &receiver);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"No."), "the table's own field");
        for method in ["FindSet", "Insert", "Modify", "SetRange", "CalcFields"] {
            assert!(labels.contains(&method), "{method} missing from {labels:?}");
        }
    }

    /// A loaded cache carries signatures, so its entries must survive dedup
    /// against the bare names in the bundled catalog.
    #[test]
    fn a_loaded_semantic_cache_supplies_the_record_method_signatures() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code")],
        )]);
        al_workspace::set_builtins(
            &ws,
            vec![al_semantic::BuiltinType {
                name: "TableClass".to_string(),
                methods: vec![al_semantic::BuiltinMethod {
                    name: "FindSet".to_string(),
                    parameters: Vec::new(),
                    return_type: Some("Boolean".to_string()),
                    documentation: "Finds a set of records".to_string(),
                }],
                enum_values: Vec::new(),
            }],
            "17.0.0.0",
        );
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let items = completion_items_for_receiver(&ws, &receiver);
        let find_set: Vec<&CompletionCandidate> = items
            .iter()
            .filter(|item| item.label == "FindSet")
            .collect();
        assert_eq!(find_set.len(), 1, "the catalog must not be added as well");
        assert_eq!(
            find_set[0].documentation.as_deref(),
            Some("Finds a set of records"),
            "the cache entry must be the one kept"
        );
    }

    #[test]
    fn enum_completion_items_include_extension_values() {
        let ws = workspace_with(vec![
            enum_entry(
                50000,
                "Color",
                vec![enum_value("Red", 0), enum_value("Green", 1)],
            ),
            enum_ext_entry(50001, "Color Ext", "Color", vec![enum_value("Blue", 2)]),
        ]);
        let enum_type = ResolvedType {
            type_name: "Enum".to_string(),
            type_subtype: Some("Color".to_string()),
        };

        let items = enum_completion_items(&ws, &enum_type);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Red"), "base value present");
        assert!(labels.contains(&"Green"), "base value present");
        assert!(labels.contains(&"Blue"), "extension value present");
    }

    #[test]
    fn record_completions_include_all_platform_system_fields() {
        let ws = Workspace::new();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let items = completion_items_for_receiver(&ws, &receiver);
        for (name, type_name) in RECORD_SYSTEM_FIELDS {
            let item = items
                .iter()
                .find(|item| item.label == name)
                .unwrap_or_else(|| panic!("{name} completion missing"));
            assert_eq!(item.kind, CompletionCandidateKind::Field);
            assert_eq!(item.detail.as_deref(), Some(type_name));
        }
    }
}
