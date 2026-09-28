//! Member access on a `postfix_expression`: record field reads
//! (`Rec."Field"`), indexed reads, chained receiver values, and
//! scope-qualified enum member access (`"Enum"::Member`). A call suffix is
//! recognised here but the call itself is delegated to `eval_stmt`.

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::chain;
use crate::interpreter::dispatch::table_code::{self, TableCode};
use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::indexing;
use crate::interpreter::records;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;

use super::entry::eval_expr;
use super::helpers::{named_child, utf8_text};

/// Evaluate a `postfix_expression`: a primary expression followed by zero or
/// more suffixes. Three shapes:
///   * call suffix (`Foo(args)`, `Recv.Proc(args)`, `Cu::Run(args)`) →
///     dispatched through `eval_stmt::eval_call` (needs `ctx`).
///   * scope suffix (`"Enum"::Member`, `Enum::"T"::"V"`) → an Option value.
///   * no suffix → transparent wrapper around the primary expression.
pub(super) fn eval_postfix(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // A trailing call suffix means this is a procedure/method call — let the
    // shared call machinery (which owns argument evaluation + dispatch) run it.
    if crate::interpreter::eval_stmt::is_call_postfix(node) {
        return crate::interpreter::eval_stmt::eval_call(node, source, stack, ctx);
    }

    // Scope suffix(es) (`::Member`) without a call → scope-qualified enum access.
    let scope_members: Vec<Node<'_>> = {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .filter(|c| c.kind() == "scope_suffix")
            .collect()
    };
    if !scope_members.is_empty() {
        return eval_scope_access(node, &scope_members, source, ctx);
    }

    // Record field read: `Rec."Field"` (a `member_suffix`, not a call) where the
    // receiver resolves to a bound `Value::Record`.
    if let Some((recv, field)) = records::record_field_access(node, source) {
        match stack.lookup(&recv) {
            Some(Value::Record(_)) => {
                if let Some((table, handle)) = records::record_binding(&recv, stack, ctx) {
                    // A record method or table procedure may drop its
                    // parentheses too (`R.Insert;`, `if R.FindFirst then`).
                    // A field of the same name wins.
                    let method = records::supports_record_method(&field)
                        || table_code::declares(ctx, &table.name, TableCode::Procedure(&field));
                    if method && !records::declares_field(&table, &field, ctx) {
                        return crate::interpreter::eval_stmt::eval_call_parts(
                            Some(&recv),
                            &field,
                            None,
                            source,
                            stack,
                            ctx,
                        );
                    }
                    return records::field_get(&table, handle, &field, ctx);
                }
            }
            // A method with no arguments may drop its parentheses:
            // `S.Length`, `Names.Count`.
            Some(_) if node.named_child_count() == 2 => {
                return crate::interpreter::eval_stmt::eval_call_parts(
                    Some(&recv),
                    &field,
                    None,
                    source,
                    stack,
                    ctx,
                );
            }
            _ => {}
        }
    }

    if let Some((name, suffix)) = indexing::indexed_variable(node, source) {
        return indexing::read_element(&name, suffix, source, stack, ctx);
    }
    if let Some(result) = chain::eval_chained_value(node, source, stack, ctx) {
        return result;
    }
    if node.named_child_count() > 1 {
        return Eval::Error(error_info(format!(
            "'{}' is not an expression the local runtime can evaluate",
            utf8_text(node, source).unwrap_or_default().trim()
        )));
    }

    // Plain wrapper — evaluate the primary expression.
    match named_child(node, 0) {
        Some(inner) => eval_expr(inner, source, stack, ctx),
        None => Eval::Error(error_info("empty postfix expression")),
    }
}

/// Evaluate a scope-qualified enum/option member access into a `Value::Option`.
///
/// Two grammar shapes:
///   * `"Enum Type"::Member`  → primary is the enum type, one scope suffix.
///   * `Enum::"Type"::"Value"` → primary is the `Enum` keyword; the first
///     scope suffix names the type, the last names the member.
///
/// Workspace enum declarations are resolved through the same source catalog as
/// procedure dispatch, preserving explicit (including sparse) ordinals.
pub(crate) fn eval_scope_access(
    node: Node<'_>,
    scope_members: &[Node<'_>],
    source: &[u8],
    ctx: &DispatchCtx,
) -> Eval {
    let member_name = |n: Node<'_>| -> Option<String> {
        n.child_by_field_name("member")
            .or_else(|| n.named_child(0))
            .and_then(|m| m.utf8_text(source).ok())
            .map(|t| t.unquote_identifier().into_owned())
    };

    let primary_text = node
        .named_child(0)
        .and_then(|p| p.utf8_text(source).ok())
        .map(|t| t.unquote_identifier().into_owned())
        .unwrap_or_default();

    let (type_name, member) = if primary_text.eq_ignore_ascii_case("enum") {
        // `Enum::"Type"::"Value"` — type is the first suffix, member the last.
        let type_name = scope_members.first().and_then(|n| member_name(*n));
        let member = scope_members.last().and_then(|n| member_name(*n));
        match (type_name, member) {
            (Some(t), Some(m)) if scope_members.len() >= 2 => (t, m),
            // `Enum::Member` with a single suffix is malformed without a type;
            // treat the suffix as the member with an unknown type.
            (Some(t), _) => (String::new(), t),
            _ => return Eval::Error(error_info("scope access: missing enum member")),
        }
    } else {
        // `"Type"::Member` — primary is the type, the suffix is the member.
        let Some(member) = scope_members.last().and_then(|n| member_name(*n)) else {
            return Eval::Error(error_info("scope access: missing enum member"));
        };
        (primary_text, member)
    };

    let Some(ordinal) = resolve_workspace_enum_ordinal(ctx, &type_name, &member) else {
        return Eval::Error(error_info(format!(
            "enum member '{type_name}::{member}' has no workspace declaration; live BC execution is required"
        )));
    };
    Eval::Normal(Value::Option {
        type_name,
        member,
        ordinal,
    })
}

pub(super) fn resolve_workspace_enum_ordinal(
    ctx: &DispatchCtx,
    type_name: &str,
    member: &str,
) -> Option<i64> {
    crate::interpreter::enums::workspace_enum_ordinal(ctx, type_name, member)
}
