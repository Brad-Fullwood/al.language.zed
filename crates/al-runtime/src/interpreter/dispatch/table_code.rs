//! Table code: a workspace table's triggers, field triggers and procedures,
//! run on a record variable that becomes their implicit `Rec`.
//!
//! Inside table code a bare field name reads or writes that record and a
//! bare record method (`Modify()`, `TestField(Name)`) acts on it, as in BC.

use al_syntax::IdentifierText;

use crate::interpreter::eval_error;
use crate::interpreter::records::{
    find_table_object, object_name_of, parse_field_def, record_binding, section_body,
    sections_with_keyword, IMPLICIT_RECORD,
};
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;

use super::frames::{collect_params, collect_return};
use super::workspace_procedure::{
    object_has_global_declarations, run_declaration, Declaration, DeclarationSite,
};
use super::{DispatchCtx, MAX_RECURSION_DEPTH};

/// A piece of code a table declares.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TableCode<'a> {
    /// `procedure Name(...)` on the table.
    Procedure(&'a str),
    /// A table trigger: `OnInsert`, `OnModify`, `OnDelete`, `OnRename`.
    Trigger(&'a str),
    /// A field's trigger, such as the `OnValidate` of field `Name`.
    FieldTrigger { field: &'a str, trigger: &'a str },
}

/// Whether table `table_name` declares `code`.
pub(crate) fn declares(ctx: &DispatchCtx, table_name: &str, code: TableCode<'_>) -> bool {
    let Some(path) = ctx.source.find_by_object_name(table_name) else {
        return false;
    };
    let Some((text, tree)) = ctx.source.get_cached_parse(&path) else {
        return false;
    };
    let source = text.as_bytes();
    find_table_object(tree.root_node(), source, table_name)
        .and_then(|object| find_code(object, source, code))
        .is_some()
}

/// Run `code` of `rec`'s table with `rec` as the implicit record and `x_rec`
/// as `xRec`. `rec` must carry its view handle so the code reads and writes
/// the caller's buffer. `None` when the table declares no such code.
pub(crate) fn run_table_code(
    rec: Value,
    x_rec: Value,
    code: TableCode<'_>,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let Value::Record(record) = &rec else {
        return None;
    };
    let path = ctx.source.find_by_object_name(&record.table_name)?;
    let (text, tree) = ctx.source.get_cached_parse(&path)?;
    let source = text.as_bytes();
    let object = find_table_object(tree.root_node(), source, &record.table_name)?;
    let node = find_code(object, source, code)?;
    if ctx.recursion_depth >= MAX_RECURSION_DEPTH {
        return Some(eval_error(format!(
            "call depth of {MAX_RECURSION_DEPTH} exceeded in table '{}'",
            record.table_name
        )));
    }
    let object_name = object_name_of(object, source).unwrap_or_else(|| record.table_name.clone());
    let name = match code {
        TableCode::Procedure(name) | TableCode::Trigger(name) => name,
        TableCode::FieldTrigger { trigger, .. } => trigger,
    };
    Some(run_declaration(
        Declaration {
            node,
            name,
            params: collect_params(node, source),
            return_decl: collect_return(node, source),
        },
        DeclarationSite {
            path: &path,
            source,
            object_name: &object_name,
            // A table's globals belong to the record instance; each call
            // starts them fresh, which is exact for triggers and procedures
            // that do not keep state between calls.
            globals_root: object_has_global_declarations(object).then_some(object),
            implicit_record: Some((rec, x_rec)),
        },
        args,
        stack,
        ctx,
    ))
}

/// A call `procedure(args)` that is a table procedure: on a record variable
/// (`Member.Touch(5)`) or, inside table code, bare (`Touch(5)`). `None` when
/// it is neither, so the caller dispatches it as usual.
pub(crate) fn dispatch_table_procedure(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Eval, Vec<Value>> {
    let variable = match receiver {
        Some(receiver) => receiver,
        None if stack.top().is_some_and(|frame| frame.implicit_record) => IMPLICIT_RECORD,
        None => return Err(args),
    };
    let table_name = match stack.lookup(variable) {
        Some(Value::Record(record)) => record.table_name.clone(),
        _ => return Err(args),
    };
    if !declares(ctx, &table_name, TableCode::Procedure(procedure)) {
        return Err(args);
    }
    // Give the variable its view handle first so the procedure shares it.
    if record_binding(variable, stack, ctx).is_none() {
        return Err(args);
    }
    let rec = stack.lookup(variable).cloned().unwrap_or(Value::Null);
    let x_rec = match receiver {
        None => stack.lookup("xRec").cloned().unwrap_or_else(|| rec.clone()),
        Some(_) => rec.clone(),
    };
    run_table_code(
        rec,
        x_rec,
        TableCode::Procedure(procedure),
        args,
        stack,
        ctx,
    )
    .ok_or_else(Vec::new)
}

/// A plain relation's target table and, when named, field: `Item` or
/// `Item."No."`. `None` for a conditional or filtered relation
/// (`where(...)`, `if (...) ... else ...`), which the local runtime cannot
/// check.
pub fn relation_target(relation: &str) -> Option<(String, Option<String>)> {
    // Keywords outside quoted names: `"Gift Card"` is a table, not an `if`.
    let unquoted: String = relation
        .split('"')
        .step_by(2)
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let conditional = unquoted.contains('(')
        || unquoted
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| matches!(word, "where" | "if" | "else"));
    if conditional {
        return None;
    }
    let (target, field) = match split_outside_quotes(relation, '.') {
        Some((target, field)) => (target, Some(field.trim().unquote_identifier().into_owned())),
        None => (relation, None),
    };
    Some((target.trim().unquote_identifier().into_owned(), field))
}

/// Split at the first `separator` outside double quotes.
fn split_outside_quotes(text: &str, separator: char) -> Option<(&str, &str)> {
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == separator && !quoted => return Some((&text[..at], &text[at + 1..])),
            _ => {}
        }
    }
    None
}

/// The raw `TableRelation` of field `field` of table `table_name` in the
/// parse tree `root`, if it declares one.
pub fn table_relation_in(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    table_name: &str,
    field: &str,
) -> Option<String> {
    let object = find_table_object(root, source, table_name)?;
    let field_body = field_section(object, source, field)?.child_by_field_name("body")?;
    let mut cursor = field_body.walk();
    let relation = field_body.named_children(&mut cursor).find_map(|child| {
        let is_relation = child.kind() == "property_assignment"
            && child
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("TableRelation"));
        is_relation
            .then(|| child.child_by_field_name("value"))
            .flatten()
            .and_then(|value| {
                let start = value.start_byte();
                // The value may span several nodes up to the semicolon.
                let end = child
                    .named_children(&mut child.walk())
                    .filter(|part| part.kind() != "semicolon")
                    .map(|part| part.end_byte())
                    .max()
                    .unwrap_or(value.end_byte());
                std::str::from_utf8(source.get(start..end)?).ok()
            })
            .map(|text| text.trim().to_string())
    });
    relation
}

/// The `field(...)` section of field `field` in table `object`.
fn field_section<'t>(
    object: tree_sitter::Node<'t>,
    source: &[u8],
    field: &str,
) -> Option<tree_sitter::Node<'t>> {
    let body = object.child_by_field_name("body")?;
    let fields = section_body(body, "fields", source)?;
    sections_with_keyword(fields, "field", source)
        .into_iter()
        .find(|section| {
            parse_field_def(*section, source)
                .is_ok_and(|(_, name, _, _)| name.eq_ignore_ascii_case(field))
        })
}

/// The declaration node of `code` in table `object`.
fn find_code<'t>(
    object: tree_sitter::Node<'t>,
    source: &[u8],
    code: TableCode<'_>,
) -> Option<tree_sitter::Node<'t>> {
    let body = object.child_by_field_name("body")?;
    match code {
        TableCode::Procedure(name) => {
            named_child_declaration(body, "procedure_declaration", name, source)
        }
        TableCode::Trigger(name) => {
            named_child_declaration(body, "trigger_declaration", name, source)
        }
        TableCode::FieldTrigger { field, trigger } => {
            let field_body = field_section(object, source, field)?.child_by_field_name("body")?;
            named_child_declaration(field_body, "trigger_declaration", trigger, source)
        }
    }
}

/// The direct child of `body` of `kind` named `name`.
fn named_child_declaration<'t>(
    body: tree_sitter::Node<'t>,
    kind: &str,
    name: &str,
    source: &[u8],
) -> Option<tree_sitter::Node<'t>> {
    let mut cursor = body.walk();
    let found = body.named_children(&mut cursor).find(|child| {
        child.kind() == kind
            && child
                .child_by_field_name("name")
                .and_then(|node| node.utf8_text(source).ok())
                .is_some_and(|text| text.unquote_identifier().eq_ignore_ascii_case(name))
    });
    found
}
