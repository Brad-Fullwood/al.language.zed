//! Chained calls: `S.Trim().ToUpper()`, `S.Split(',').Count()`,
//! `Format(N).PadLeft(4, '0')`, `Rec.Name.ToUpper()`, `Names[2].Trim()`.
//!
//! Method dispatch routes by the receiver *variable*, so the value the chain
//! has built so far is bound to a temporary in the current frame and the next
//! call is made on that. Leading `::` steps name an enum value
//! (`Colour::Blue.AsInteger()`) or, after `Enum`, an enum type
//! (`Enum::Colour.FromInteger(3)`).

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_expr::{eval_expr, eval_scope_access};
use crate::interpreter::eval_stmt::{eval_call_arguments, eval_call_parts, find_argument_list};
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::interpreter::{enums, eval_error, indexing, records};

/// Evaluate `node` when it is a call made on the result of another suffix;
/// `None` when it is not such a chain.
pub(crate) fn eval_chained_call(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let (primary, suffixes) = chain_parts(node)?;
    let (last, prefix) = suffixes.split_last()?;
    if last.kind() != "member_call_suffix" {
        return None;
    }
    if let Some(result) = enum_type_call(primary, prefix, *last, source, stack, ctx) {
        return Some(match result {
            Ok(value) => Eval::Normal(value),
            Err(error) => error,
        });
    }
    // The prefix is an expression whatever position the chain is in.
    let statement = std::mem::take(&mut ctx.stmt_position);
    let receiver = match eval_prefix(primary, prefix, source, stack, ctx) {
        Ok(value) => value,
        Err(error) => return Some(error),
    };
    ctx.stmt_position = statement;
    Some(call_on(receiver, *last, source, stack, ctx))
}

/// Evaluate `node` when it is a chain that ends in a field read or an index
/// (`Names.Get(1)[2]`, `Format(N)[1]`); `None` when it is not such a chain.
pub(crate) fn eval_chained_value(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Eval> {
    let (primary, suffixes) = chain_parts(node)?;
    Some(match eval_prefix(primary, &suffixes, source, stack, ctx) {
        Ok(value) => Eval::Normal(value),
        Err(error) => error,
    })
}

/// The primary expression and suffixes of a postfix chain with at least two
/// suffixes, or a call on a literal. `::` steps may only lead it.
fn chain_parts(node: Node<'_>) -> Option<(Node<'_>, Vec<Node<'_>>)> {
    if node.kind() != "postfix_expression" {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let (primary, suffixes) = children.split_first()?;
    // A method on a literal or parenthesised value (`'ab'.PadRight(3)`) has
    // no variable to route by either.
    let value_receiver = primary
        .named_child(0)
        .is_some_and(|inner| !matches!(inner.kind(), "name" | "object_keyword" | "type_keyword"));
    // `Colour::Blue.AsInteger()`: `::` steps may only lead the chain.
    let scope_steps = suffixes
        .iter()
        .take_while(|suffix| suffix.kind() == "scope_suffix")
        .count();
    let chained = (suffixes.len() >= 2 || value_receiver)
        && suffixes[scope_steps..].iter().all(|suffix| {
            matches!(
                suffix.kind(),
                "member_call_suffix" | "call_suffix" | "member_suffix" | "index_suffix"
            )
        });
    chained.then(|| (*primary, suffixes.to_vec()))
}

/// The value of `primary` followed by `suffixes`.
fn eval_prefix(
    primary: Node<'_>,
    suffixes: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Value, Eval> {
    let Some((last, rest)) = suffixes.split_last() else {
        return normal(eval_expr(primary, source, stack, ctx));
    };
    match last.kind() {
        // `Colour::Blue` / `Enum::Colour::Blue` at the head of a chain.
        "scope_suffix" => match primary.parent() {
            Some(postfix) => normal(eval_scope_access(postfix, suffixes, source, ctx)),
            None => Err(eval_error("an enum value needs its expression")),
        },
        // `Format(N)` at the head of a chain: a bare call.
        "call_suffix" if rest.is_empty() => {
            let name = primary_name(primary, source)
                .ok_or_else(|| eval_error("a chained call must start with a name"))?;
            normal(eval_call_parts(
                None,
                &name,
                find_argument_list(*last),
                source,
                stack,
                ctx,
            ))
        }
        "member_call_suffix" => {
            if let Some(result) = enum_type_call(primary, rest, *last, source, stack, ctx) {
                return result;
            }
            let receiver = eval_prefix(primary, rest, source, stack, ctx)?;
            normal(call_on(receiver, *last, source, stack, ctx))
        }
        "member_suffix" => {
            let receiver = eval_prefix(primary, rest, source, stack, ctx)?;
            let field = last
                .child_by_field_name("member")
                .or_else(|| last.named_child(0))
                .and_then(|member| member.utf8_text(source).ok())
                .map(|text| text.unquote_identifier().into_owned())
                .unwrap_or_default();
            with_temporary(
                receiver,
                *last,
                stack,
                |name, stack| match records::record_binding(name, stack, ctx) {
                    Some((table, handle)) => {
                        normal(records::field_get(&table, handle, &field, ctx))
                    }
                    None => Err(eval_error(format!(
                        "'.{field}' needs a record before it in the chain"
                    ))),
                },
            )
        }
        "index_suffix" => {
            let receiver = eval_prefix(primary, rest, source, stack, ctx)?;
            with_temporary(receiver, *last, stack, |name, stack| {
                normal(indexing::read_element(name, *last, source, stack, ctx))
            })
        }
        other => Err(eval_error(format!(
            "unsupported step '{other}' in a chained call"
        ))),
    }
}

/// `Enum::"Type".Method(...)` (FromInteger, Names, Ordinals): a call on the
/// enum type rather than a value. `None` when `primary` + `prefix` is not
/// `Enum::"Type"`.
fn enum_type_call(
    primary: Node<'_>,
    prefix: &[Node<'_>],
    call: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Option<Result<Value, Eval>> {
    let [scope] = prefix else {
        return None;
    };
    let is_type = scope.kind() == "scope_suffix"
        && primary_name(primary, source).is_some_and(|name| name.eq_ignore_ascii_case("enum"));
    if !is_type {
        return None;
    }
    let type_name = scope_member(*scope, source);
    Some(
        eval_call_arguments(find_argument_list(call), source, stack, ctx).and_then(|args| {
            normal(enums::dispatch_enum_static(
                &type_name,
                &member_name(call, source),
                &args,
                ctx,
            ))
        }),
    )
}

/// `receiver.Method(args)` for the `member_call_suffix` `suffix`.
fn call_on(
    receiver: Value,
    suffix: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let method = member_name(suffix, source);
    let supported = match &receiver {
        Value::Text(_) | Value::Code(_) => records::supports_text_method(&method),
        Value::List(_) => records::supports_list_method(&method),
        Value::Dict(_) => records::supports_dict_method(&method),
        Value::Record(_) => records::supports_record_method(&method),
        Value::Codeunit { .. } => true,
        Value::Option { .. } => enums::supports_enum_method(&method),
        Value::TextBuilder(_) => records::supports_textbuilder_method(&method),
        Value::Json(json) => crate::interpreter::json::supports_json_method(json.kind, &method),
        _ => false,
    };
    if !supported {
        return eval_error(format!(
            "{}.{method} in a chained call is not supported by the local runtime",
            receiver.type_name()
        ));
    }
    let arguments = find_argument_list(suffix);
    let result = with_temporary(receiver, suffix, stack, |name, stack| {
        Ok(eval_call_parts(
            Some(name),
            &method,
            arguments,
            source,
            stack,
            ctx,
        ))
    });
    result.unwrap_or_else(|error| error)
}

/// Bind `value` to a name no AL identifier can take, run `body` with that
/// name, then drop the binding.
fn with_temporary<T>(
    value: Value,
    step: Node<'_>,
    stack: &mut ScopeStack,
    body: impl FnOnce(&str, &mut ScopeStack) -> Result<T, Eval>,
) -> Result<T, Eval> {
    let name = format!("<chain@{}>", step.start_byte());
    let Some(frame) = stack.top_mut() else {
        return Err(eval_error("a chained call needs a procedure frame"));
    };
    frame.bind(&name, value);
    let result = body(&name, stack);
    if let Some(frame) = stack.top_mut() {
        frame.locals.remove(&name);
    }
    result
}

fn member_name(suffix: Node<'_>, source: &[u8]) -> String {
    suffix
        .child_by_field_name("member")
        .and_then(|member| member.utf8_text(source).ok())
        .map(|text| text.unquote_identifier().into_owned())
        .unwrap_or_default()
}

fn scope_member(scope: Node<'_>, source: &[u8]) -> String {
    scope
        .child_by_field_name("member")
        .or_else(|| scope.named_child(0))
        .and_then(|member| member.utf8_text(source).ok())
        .map(|text| text.unquote_identifier().into_owned())
        .unwrap_or_default()
}

fn primary_name(primary: Node<'_>, source: &[u8]) -> Option<String> {
    let name = if primary.kind() == "primary_expression" {
        primary.named_child(0)?
    } else {
        primary
    };
    let text = name.utf8_text(source).ok()?;
    Some(text.unquote_identifier().into_owned())
}

fn normal(eval: Eval) -> Result<Value, Eval> {
    match eval {
        Eval::Normal(value) => Ok(value),
        other => Err(other),
    }
}
