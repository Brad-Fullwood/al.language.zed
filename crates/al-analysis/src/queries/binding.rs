//! Canonical declaration resolution shared by symbol-aware queries.
//!
//! Several queries (`rename`, `references`) need to decide whether two textual
//! occurrences of a name bind to the *same* declaration. Go-to-definition alone
//! is unreliable for this: invoked on a declaration's own name it skips the
//! same-file declaration at the cursor and falls through to a usage (or an
//! unrelated same-named object). [`decl_loc`] papers over that by treating a
//! declaration-name cursor as its own canonical site and only deferring to
//! go-to-definition for genuine usages.

use url::Url;

use super::Position;
use super::Range;
use al_workspace::{Workspace, WorkspaceStateError};

/// A location identity: `(uri_string, line, character)`. Two positions bind to
/// the same symbol iff their [`decl_loc`] values are equal.
pub(crate) type BindKey = (String, u32, u32);

/// The canonical declaration a position binds to.
///
/// When `pos` sits on a declaration's own name, that name *is* the canonical
/// declaration — an object-local identity that keeps two objects' same-named
/// `procedure Post()` declarations distinct. (Go-to-definition on a
/// declaration is unreliable: it skips the same-file decl at the cursor and can
/// fall through to an unrelated same-named procedure in another object.) For
/// every other position (a usage) we defer to go-to-definition, which resolves
/// a bare procedure call same-file-first and a qualified call to its true owner.
pub(crate) fn decl_loc(
    workspace: &Workspace,
    uri: &Url,
    pos: Position,
) -> Result<BindKey, WorkspaceStateError> {
    if let Some(key) = enclosing_declaration_name(workspace, uri, pos) {
        return Ok(key);
    }
    if let Some(loc) =
        super::definition::definition(workspace, uri, pos)?.and_then(|locs| locs.into_iter().next())
    {
        return Ok((
            loc.uri.to_string(),
            loc.range.start.line,
            loc.range.start.character,
        ));
    }
    Ok((uri.to_string(), pos.line, pos.character))
}

/// Per-query memo for [`decl_loc`].
///
/// `rename` and `references` run the full go-to-definition binder once per
/// candidate occurrence in every workspace file, which makes them
/// O(occurrences × definition-query) on a common identifier. Within one file,
/// two occurrences that share (enclosing declaration, syntactic role,
/// qualifier, spelling) necessarily bind to the same declaration — AL has no
/// shadowing inside a procedure body — so the binder only has to run once per
/// distinct group.
///
/// The *role* component (parent node kind + field name) keeps a declaration's
/// own name distinct from a usage that happens to be spelled the same, e.g.
/// `procedure Foo(Customer: Record Customer)`, where the parameter name and the
/// type name are both `Customer` in the same scope but bind differently.
#[derive(Default)]
pub(crate) struct DeclLocCache {
    entries: std::collections::HashMap<CacheKey, BindKey>,
}

/// `(uri, enclosing declaration start byte, syntactic role, qualifier, name)`.
type CacheKey = (String, usize, String, String, String);

impl DeclLocCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// [`decl_loc`] for the occurrence spanning `reference`, memoized.
    pub(crate) fn decl_loc_for_reference(
        &mut self,
        workspace: &Workspace,
        uri: &Url,
        text: &str,
        tree: &tree_sitter::Tree,
        reference: &tree_sitter::Range,
    ) -> Result<BindKey, WorkspaceStateError> {
        let range: Range = al_syntax::ts_range_to_syntax(reference, text.as_bytes()).into();
        let position: Position = range.start;
        let key = (
            uri.to_string(),
            enclosing_declaration_start(tree, reference.start_byte),
            occurrence_role(tree, reference.start_byte, reference.end_byte),
            qualifier_before(text, reference.start_byte),
            text.get(reference.start_byte..reference.end_byte)
                .unwrap_or_default()
                .to_lowercase(),
        );
        if let Some(cached) = self.entries.get(&key) {
            return Ok(cached.clone());
        }
        let resolved = decl_loc(workspace, uri, position)?;
        self.entries.insert(key, resolved.clone());
        Ok(resolved)
    }
}

/// Start byte of the procedure/trigger that contains `byte`, or `usize::MAX`
/// for object-level positions.
fn enclosing_declaration_start(tree: &tree_sitter::Tree, byte: usize) -> usize {
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte);
    while let Some(current) = node {
        if matches!(
            current.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
        ) {
            return current.start_byte();
        }
        node = current.parent();
    }
    usize::MAX
}

/// `parent_kind/field_name` for the node spanning `start..end`.
fn occurrence_role(tree: &tree_sitter::Tree, start: usize, end: usize) -> String {
    let Some(node) = tree.root_node().descendant_for_byte_range(start, end) else {
        return String::new();
    };
    let Some(parent) = node.parent() else {
        return node.kind().to_string();
    };
    let mut cursor = parent.walk();
    let mut field = "";
    if cursor.goto_first_child() {
        loop {
            if cursor.node().id() == node.id() {
                field = cursor.field_name().unwrap_or("");
                break;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    format!("{}/{}/{}", parent.kind(), field, node.kind())
}

/// Lower-cased receiver of a `Receiver.Member` occurrence starting at `byte`,
/// or the empty string when the occurrence is unqualified.
fn qualifier_before(text: &str, byte: usize) -> String {
    let Some(before) = text.get(..byte) else {
        return String::new();
    };
    let before = before.trim_end();
    let Some(before) = before.strip_suffix('.') else {
        return String::new();
    };
    let before = before.trim_end();
    if let Some(stripped) = before.strip_suffix('"') {
        return match stripped.rfind('"') {
            Some(open) => stripped[open + 1..].to_lowercase(),
            None => String::new(),
        };
    }
    let start = before
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|index| index + 1)
        .unwrap_or(0);
    before[start..].to_lowercase()
}

/// If `pos` falls on the *name* of a declaration (procedure, trigger, field, or
/// variable), return that name's location as a `BindKey`. Returns `None` when
/// `pos` is inside a declaration but not on its name (i.e. a usage in the body),
/// so the caller falls back to go-to-definition.
pub(crate) fn enclosing_declaration_name(
    workspace: &Workspace,
    uri: &Url,
    pos: Position,
) -> Option<BindKey> {
    let (text, tree) =
        al_source::parsing::get_or_parse(&workspace.documents, uri).or_else(|| {
            uri.to_file_path()
                .ok()
                .and_then(|path| workspace.file_index.get_cached_parse(&path))
                .map(|(text, tree)| (text.into(), tree))
        })?;
    let node = al_syntax::find_node_at_position(&tree, &text, pos.into())?;
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "procedure_declaration"
            | "trigger_declaration"
            | "event_procedure_declaration"
            | "field_declaration"
            // The actual `Name: Type` unit is `regular_variable_declaration`
            // (both local `var` sections and object-level ones wrap it); the
            // `variable_declaration` container above it has no `name` field.
            | "regular_variable_declaration"
            | "label_declaration"
            | "parameter" => {
                // A `regular_variable_declaration` can declare several names on
                // one line (`Total, Status: Integer` — the `name` field is
                // `multiple`), each a distinct variable. Return whichever name
                // token the cursor sits within, so the 2nd+ name is still
                // recognized as its own canonical declaration site. Only treat
                // this as a declaration site when the cursor is on a name;
                // otherwise it is a body usage.
                let mut names = n.walk();
                for name in n.children_by_field_name("name", &mut names) {
                    if node.start_byte() >= name.start_byte()
                        && node.end_byte() <= name.end_byte()
                    {
                        let range: Range =
                            al_syntax::ts_range_to_syntax(&name.range(), text.as_bytes()).into();
                        return Some((uri.to_string(), range.start.line, range.start.character));
                    }
                }
                return None;
            }
            _ => {}
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl_loc(workspace: &Workspace, uri: &Url, position: Position) -> BindKey {
        super::decl_loc(workspace, uri, position).unwrap()
    }

    #[test]
    fn var_parameter_and_receiver_uses_share_one_binding() {
        let uri = Url::parse("file:///test/parameter_binding.al").unwrap();
        let source = r#"codeunit 50100 "Refs"
{
    procedure Process(var Staging: Record Customer)
    begin
        if Staging.FindSet() then
            Staging.Modify();
    end;
}"#;
        let workspace = Workspace::new();
        workspace
            .documents
            .open(uri.clone(), source.to_string())
            .unwrap();
        let declaration = decl_loc(
            &workspace,
            &uri,
            Position {
                line: 2,
                character: 26,
            },
        );
        let first_use = decl_loc(
            &workspace,
            &uri,
            Position {
                line: 4,
                character: 11,
            },
        );
        let second_use = decl_loc(
            &workspace,
            &uri,
            Position {
                line: 5,
                character: 12,
            },
        );

        assert_eq!(first_use, declaration);
        assert_eq!(second_use, declaration);
    }

    /// The memo must not merge two occurrences that only *look* alike: a
    /// parameter named after its own type binds to the parameter, the type
    /// reference binds to the table.
    #[test]
    fn decl_loc_cache_keeps_same_spelled_declaration_and_type_apart() {
        let uri = Url::parse("file:///test/binder_cache.al").unwrap();
        let source = r#"codeunit 50100 "Cache"
{
    procedure Process(Customer: Record Customer)
    begin
        Customer.Get('10000');
    end;
}"#;
        let workspace = Workspace::new();
        workspace
            .documents
            .open(uri.clone(), source.to_string())
            .unwrap();
        let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, &uri).unwrap();

        let refs = al_syntax::find_variable_references(&tree, &text, "Customer");
        assert!(
            refs.len() >= 3,
            "expected parameter name, type name and usage: {refs:?}"
        );

        let mut cache = super::DeclLocCache::new();
        let cached: Vec<BindKey> = refs
            .iter()
            .map(|r| {
                cache
                    .decl_loc_for_reference(&workspace, &uri, &text, &tree, r)
                    .unwrap()
            })
            .collect();
        let uncached: Vec<BindKey> = refs
            .iter()
            .map(|r| {
                let range: Range = al_syntax::ts_range_to_syntax(r, text.as_bytes()).into();
                super::decl_loc(&workspace, &uri, range.start).unwrap()
            })
            .collect();

        assert_eq!(
            cached, uncached,
            "memoized binder must agree with the uncached binder"
        );
    }

    /// Repeated occurrences in the same scope resolve identically whether or
    /// not the memo is used.
    #[test]
    fn decl_loc_cache_matches_the_uncached_binder_for_repeated_uses() {
        let uri = Url::parse("file:///test/binder_repeat.al").unwrap();
        let source = r#"codeunit 50100 "Repeat"
{
    procedure Run()
    var
        Total: Integer;
    begin
        Total := 1;
        Total := Total + 1;
        Total := Total + Total;
    end;
}"#;
        let workspace = Workspace::new();
        workspace
            .documents
            .open(uri.clone(), source.to_string())
            .unwrap();
        let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, &uri).unwrap();

        let refs = al_syntax::find_variable_references(&tree, &text, "Total");
        let mut cache = super::DeclLocCache::new();
        for r in &refs {
            let range: Range = al_syntax::ts_range_to_syntax(r, text.as_bytes()).into();
            assert_eq!(
                cache
                    .decl_loc_for_reference(&workspace, &uri, &text, &tree, r)
                    .unwrap(),
                super::decl_loc(&workspace, &uri, range.start).unwrap()
            );
        }
    }
}
