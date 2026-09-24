//! `Name[index]` on an array variable or a Text/Code value: reading an
//! element and assigning one.
//!
//! The grammar keeps the text inside `[...]` as a run of tokens rather than an
//! expression node. A lone integer or variable is read directly; anything
//! else is parsed on its own as an expression and evaluated in the caller's
//! scope.

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;
use crate::interpreter::{error_info, eval_error};

/// The variable name and `index_suffix` of `Name[index]`, looking through the
/// expression wrappers the grammar puts around it. `None` for anything else,
/// including `Rec.Field[1]` and `Name[1][2]`.
pub(crate) fn indexed_variable<'tree>(
    node: Node<'tree>,
    source: &[u8],
) -> Option<(String, Node<'tree>)> {
    let mut postfix = node;
    while matches!(postfix.kind(), "expression" | "unary_expression") {
        if postfix.named_child_count() != 1 {
            return None;
        }
        postfix = postfix.named_child(0)?;
    }
    if postfix.kind() != "postfix_expression" || postfix.named_child_count() != 2 {
        return None;
    }
    let primary = postfix.named_child(0)?;
    let suffix = postfix.named_child(1)?;
    if primary.kind() != "primary_expression" || suffix.kind() != "index_suffix" {
        return None;
    }
    let name = primary.named_child(0).filter(|n| n.kind() == "name")?;
    let text = name.utf8_text(source).ok()?;
    Some((text.unquote_identifier().to_ascii_lowercase(), suffix))
}

/// The 1-based position an `index_suffix` names.
fn eval_index(
    suffix: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<i64, Eval> {
    let block = suffix.child_by_field_name("index").unwrap_or(suffix);
    let text = block
        .utf8_text(source)
        .unwrap_or_default()
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    if text.contains(',') {
        return Err(eval_error(format!(
            "indexing with several dimensions ([{text}]) is not supported by the local runtime"
        )));
    }
    let value = if let Ok(literal) = text.parse::<i64>() {
        Value::Integer(literal)
    } else if let Some(value) = stack.lookup(&text.unquote_identifier().to_ascii_lowercase()) {
        value.clone()
    } else {
        eval_standalone_expression(text, stack, ctx)?
    };
    match value {
        Value::Integer(index) | Value::BigInteger(index) => Ok(index),
        other => Err(eval_error(format!(
            "an index must be an Integer, got {}",
            other.type_name()
        ))),
    }
}

/// Evaluate `text` as an AL expression in the caller's scope. Coverage is
/// paused so the throwaway tree's positions are not recorded against the
/// caller's file.
fn eval_standalone_expression(
    text: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Value, Eval> {
    let wrapper = format!("codeunit 1 Index {{ procedure Index() begin exit({text}); end; }}");
    let parsed = al_syntax::parser::AlParser::parse_quick(&wrapper);
    let root = parsed.tree.root_node();
    let expression = find_exit_expression(root)
        .filter(|_| !root.has_error())
        .ok_or_else(|| eval_error(format!("'{text}' is not an index expression")))?;
    let coverage = ctx.coverage.take();
    let result = eval_expr(expression, wrapper.as_bytes(), stack, ctx);
    ctx.coverage = coverage;
    match result {
        Eval::Normal(value) => Ok(value),
        other => Err(other),
    }
}

fn find_exit_expression(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "expression" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(find_exit_expression)
}

/// Checked zero-based position of a 1-based `index` into `len` elements.
fn position(index: i64, len: usize, name: &str) -> Result<usize, Eval> {
    index
        .checked_sub(1)
        .and_then(|zero_based| usize::try_from(zero_based).ok())
        .filter(|zero_based| *zero_based < len)
        .ok_or_else(|| {
            eval_error(format!(
                "index {index} is outside the bounds of '{name}' (1..{len})"
            ))
        })
}

/// `Name[index]` as a value: the array element, or the character of a Text.
pub(crate) fn read_element(
    name: &str,
    suffix: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match eval_index(suffix, source, stack, ctx)
        .and_then(|index| read_element_at(name, index, stack))
    {
        Ok(value) => Eval::Normal(value),
        Err(error) => error,
    }
}

/// `Name[index] := value`. `combine` turns the element's current value and
/// `value` into the stored one (the base operator of `+=` and friends).
pub(crate) fn write_element(
    name: &str,
    suffix: Node<'_>,
    source: &[u8],
    value: Value,
    combine: &dyn Fn(Value, Value) -> Eval,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let index = match eval_index(suffix, source, stack, ctx) {
        Ok(index) => index,
        Err(error) => return error,
    };
    let current = match read_element_at(name, index, stack) {
        Ok(current) => current,
        Err(error) => return error,
    };
    let value = match combine(current, value) {
        Eval::Normal(value) => value,
        other => return other,
    };
    let Some(slot) = stack.lookup_mut(name) else {
        return Eval::Error(error_info(format!("unbound identifier: {name}")));
    };
    let is_code = matches!(slot, Value::Code(_));
    match slot {
        Value::Array(items) => {
            let at = (index - 1) as usize;
            match Value::coerce_into_slot(&items[at], value, None) {
                Ok(value) => items[at] = value,
                Err(message) => return eval_error(message),
            }
        }
        Value::Text(text) | Value::Code(text) => {
            let mut replacement = match value {
                Value::Char(c) => c,
                Value::Text(t) | Value::Code(t) if t.chars().count() == 1 => {
                    t.chars().next().unwrap_or(' ')
                }
                other => {
                    return eval_error(format!(
                        "a character of '{name}' takes a Char, got {}",
                        other.type_name()
                    ))
                }
            };
            if is_code {
                replacement = replacement.to_ascii_uppercase();
            }
            *text = text
                .chars()
                .enumerate()
                .map(|(at, c)| {
                    if at as i64 == index - 1 {
                        replacement
                    } else {
                        c
                    }
                })
                .collect();
        }
        _ => unreachable!("read_element_at accepted only arrays and text"),
    }
    Eval::Normal(Value::Empty)
}

fn read_element_at(name: &str, index: i64, stack: &ScopeStack) -> Result<Value, Eval> {
    match stack.lookup(name) {
        Some(Value::Array(items)) => position(index, items.len(), name).map(|at| items[at].clone()),
        Some(Value::Text(text) | Value::Code(text)) => {
            let chars: Vec<char> = text.chars().collect();
            position(index, chars.len(), name).map(|at| Value::Char(chars[at]))
        }
        Some(other) => Err(eval_error(format!(
            "'{name}' is a {}, not an array",
            other.type_name()
        ))),
        None => Err(Eval::Error(error_info(format!(
            "unbound identifier: {name}"
        )))),
    }
}
