//! Reference counts behind the code lens: every call site and every
//! `[EventSubscriber]` argument in the workspace, bound to the declaration it
//! names.

use std::collections::{HashMap, HashSet};

use al_workspace::Workspace;
use url::Url;

use super::call_site;
use crate::queries::binding::{BindKey, DeclLocCache};

/// `(object kind, object name, member name)`, all folded to lowercase.
type MemberKey = (String, String, String);

/// Canonical declaration → the distinct `(uri, line, column)` sites naming it.
type Occurrences = HashMap<BindKey, HashSet<(String, u32, u32)>>;

/// Build a map of canonical declaration binding → distinct reference count by
/// scanning every file in the workspace exactly once.
///
/// Complexity: O(F) where F is the number of workspace files (times the work
/// of walking each file's parse tree).  The caller then does O(P) lookups —
/// total O(F + P) versus the previous O(P * F).
pub(super) fn build_reference_counts(
    workspace: &Workspace,
    current_uri: &Url,
) -> Result<HashMap<BindKey, usize>, String> {
    let mut seen: Occurrences = HashMap::new();
    // EventSubscriber attributes contain string literals rather than normal
    // identifier references, so binding them requires the complete workspace
    // declaration map before reference collection begins.
    let mut member_bindings: HashMap<MemberKey, BindKey> = HashMap::new();

    // One binder for the whole request. `references` and `rename` memoize the
    // same lookups for the same reason: without it every call site in every
    // workspace file runs a full go-to-definition query.
    let mut binder = DeclLocCache::new();

    // Two passes, because a subscriber can precede the event it names.
    for_each_source(workspace, current_uri, |uri, text, tree| {
        record_member_bindings(
            workspace,
            uri,
            text,
            tree,
            &mut binder,
            &mut member_bindings,
        )
    })?;
    for_each_source(workspace, current_uri, |uri, text, tree| {
        record_file(
            workspace,
            uri,
            text,
            tree,
            &mut binder,
            &member_bindings,
            &mut seen,
        )
    })?;

    Ok(seen.into_iter().map(|(k, v)| (k, v.len())).collect())
}

/// Visit the open document at `current_uri` (its unsaved text), then every
/// other indexed workspace file with a cached parse.
fn for_each_source(
    workspace: &Workspace,
    current_uri: &Url,
    mut visit: impl FnMut(&Url, &str, &tree_sitter::Tree) -> Result<(), String>,
) -> Result<(), String> {
    if let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, current_uri)
    {
        visit(current_uri, &text, &tree)?;
    }
    let current_path = current_uri.to_file_path().ok();
    let file_paths: Vec<std::path::PathBuf> = workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    for file_path in file_paths {
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        if let Ok(file_uri) = Url::from_file_path(&file_path) {
            visit(&file_uri, &file_text, &file_tree)?;
        }
    }
    Ok(())
}

fn record_member_bindings(
    workspace: &Workspace,
    file_uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
    binder: &mut DeclLocCache,
    member_bindings: &mut HashMap<MemberKey, BindKey>,
) -> Result<(), String> {
    let objects = al_syntax::find_object_declarations(tree, text);
    if objects.is_empty() {
        return Ok(());
    }
    let source = text.as_bytes();
    let mut declarations: Vec<(tree_sitter::Range, String)> = Vec::new();
    al_syntax::walk_tree(tree.root_node(), &mut |node| {
        if !matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
        ) {
            return;
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source) {
                declarations.push((name_node.range(), name.to_string()));
            }
        }
    });
    for (name_range, name) in declarations {
        // Attribute the member to the object whose range contains it: a
        // file declaring a table then its card page has members in both.
        let start = name_range.start_byte;
        let Some(object) = objects
            .iter()
            .find(|object| start >= object.range.start_byte && start < object.range.end_byte)
        else {
            continue;
        };
        let declaration = binder
            .decl_loc_for_reference(workspace, file_uri, text, tree, &name_range)
            .map_err(|error| error.to_string())?;
        member_bindings.insert(
            (
                object.kind.to_lowercase(),
                object.name.to_lowercase(),
                al_syntax::clean_identifier(&name).to_lowercase(),
            ),
            declaration,
        );
    }
    Ok(())
}

/// Walk a single file's parse tree once, recording the *name* of every
/// call site (`Foo()`, `obj.Foo()`, `T::Foo()`) into `seen`.
///
/// Previously this counted every `identifier` / `quoted_identifier` /
/// `name` node, which conflated declaration sites, type references and
/// bare field references with actual call sites — a procedure declared
/// once and never called appeared as "1 reference" because of the
/// declaration itself, and any field with the same name doubled the
/// count.
///
/// AL grammar shapes (mirrors `al_syntax::is_call_reference`):
/// - bare call `Foo()`: `identifier → name → primary_expression`,
///   whose `postfix_expression` parent has a `call_suffix` child;
/// - method call `obj.Foo()`: `identifier → name → member_call_suffix`
///   as the `member` field;
/// - scope call `T::Foo()`: `identifier → name → scope_call_suffix`
///   as the `member` field.
fn record_file(
    workspace: &Workspace,
    file_uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
    binder: &mut DeclLocCache,
    member_bindings: &HashMap<MemberKey, BindKey>,
    seen: &mut Occurrences,
) -> Result<(), String> {
    let uri_str = &file_uri.to_string();
    let source_bytes = text.as_bytes();
    let mut call_sites = Vec::new();
    al_syntax::walk_tree(tree.root_node(), &mut |node| {
        if node.kind() == "attribute" {
            record_event_subscriber_reference(node, uri_str, source_bytes, member_bindings, seen);
            return;
        }
        if matches!(node.kind(), "identifier" | "quoted_identifier")
            && call_site::is_call_site(node)
        {
            call_sites.push(node.range());
        }
    });
    for ts_range in call_sites {
        let lsp_range = al_syntax::ts_range_to_syntax(&ts_range, source_bytes);
        let declaration = binder
            .decl_loc_for_reference(workspace, file_uri, text, tree, &ts_range)
            .map_err(|error| error.to_string())?;
        let key = (
            uri_str.to_string(),
            lsp_range.start.line,
            lsp_range.start.character,
        );
        seen.entry(declaration).or_default().insert(key);
    }
    Ok(())
}

fn record_event_subscriber_reference(
    node: tree_sitter::Node<'_>,
    uri_str: &str,
    source: &[u8],
    member_bindings: &HashMap<MemberKey, BindKey>,
    seen: &mut Occurrences,
) {
    let attr_name = node
        .child_by_field_name("name")
        .or_else(|| node.child(0))
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");
    if !attr_name.trim().eq_ignore_ascii_case("EventSubscriber") {
        return;
    }
    let mut cursor = node.walk();
    let Some(arg_list) = node
        .children(&mut cursor)
        .find(|child| child.kind() == "attribute_argument_list")
    else {
        return;
    };
    let mut arg_cursor = arg_list.walk();
    let args: Vec<_> = arg_list
        .children(&mut arg_cursor)
        .filter(|child| child.kind() == "attribute_argument")
        .collect();
    let (Some(kind_arg), Some(object_arg), Some(event_arg)) =
        (args.first(), args.get(1), args.get(2))
    else {
        return;
    };
    let clean = |arg: &tree_sitter::Node<'_>| {
        arg.utf8_text(source)
            .ok()
            .map(al_syntax::clean_attr_arg)
            .unwrap_or_default()
            .to_lowercase()
    };
    let member_key = (clean(kind_arg), clean(object_arg), clean(event_arg));
    let Some(declaration) = member_bindings.get(&member_key) else {
        return;
    };
    let range = al_syntax::ts_range_to_syntax(&event_arg.range(), source);
    seen.entry(declaration.clone()).or_default().insert((
        uri_str.to_string(),
        range.start.line,
        range.start.character,
    ));
}
