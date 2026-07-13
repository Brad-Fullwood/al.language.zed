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
use al_workspace::Workspace;

/// A location identity: `(uri_string, line, character)`. Two positions bind to
/// the same symbol iff their [`decl_loc`] values are equal.
pub(crate) type BindKey = (String, u32, u32);

/// The canonical declaration a position binds to.
///
/// When `pos` sits on a declaration's own name, that name *is* the canonical
/// declaration — an object-local identity that keeps two objects' same-named
/// `procedure Post()` declarations distinct (C18). (Go-to-definition on a
/// declaration is unreliable: it skips the same-file decl at the cursor and can
/// fall through to an unrelated same-named procedure in another object.) For
/// every other position (a usage) we defer to go-to-definition, which resolves
/// a bare procedure call same-file-first and a qualified call to its true owner.
pub(crate) fn decl_loc(workspace: &Workspace, uri: &Url, pos: Position) -> BindKey {
    if let Some(key) = enclosing_declaration_name(workspace, uri, pos) {
        return key;
    }
    if let Some(loc) =
        super::definition::definition(workspace, uri, pos).and_then(|locs| locs.into_iter().next())
    {
        return (
            loc.uri.to_string(),
            loc.range.start.line,
            loc.range.start.character,
        );
    }
    (uri.to_string(), pos.line, pos.character)
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
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
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
