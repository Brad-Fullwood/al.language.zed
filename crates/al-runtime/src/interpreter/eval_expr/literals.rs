//! Literal evaluation: integers, decimals, strings, dates, times, set
//! literals, and the niladic clock builtins (`Today`, `Time`,
//! `CurrentDateTime`, `WorkDate`) that read like identifiers.

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{self, Value};

use super::entry::eval_expr;
use super::helpers::utf8_text;

/// Unescape an AL string literal: strip exactly ONE leading and ONE trailing
/// quote, then collapse each doubled quote (`''`) to a single one. Stripping
/// *all* leading/trailing quotes first would corrupt literals that begin or
/// end with an escaped quote: `''''` is the one-character string `'`, and
/// `'abc'''` ends with `abc'`.
pub(super) fn unescape_al_string(text: &str) -> String {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .unwrap_or(trimmed);
    inner.replace("''", "'")
}

/// Type an integer literal. An `l`/`L` suffix — AL's `BigInteger` literal
/// marker — or a magnitude outside the 32-bit `Integer` range yields a
/// `BigInteger`; otherwise `Integer`.
pub(super) fn int_literal_value(text: &str) -> Option<Value> {
    let text = text.trim();
    let had_suffix = text.ends_with(['l', 'L']);
    let n: i64 = text.trim_end_matches(['l', 'L']).parse().ok()?;
    let fits_i32 = (i32::MIN as i64..=i32::MAX as i64).contains(&n);
    Some(if had_suffix || !fits_i32 {
        Value::BigInteger(n)
    } else {
        Value::Integer(n)
    })
}

/// Evaluate an AL set literal represented by the grammar's generic
/// `bracketed_block`.
pub(super) fn eval_set_literal(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(error_info("set literal: invalid source text"));
    };
    let Some(inner) = text
        .strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
    else {
        return Eval::Error(error_info("set literal: missing brackets"));
    };
    let members = match split_set_members(inner) {
        Ok(members) => members,
        Err(message) => return Eval::Error(error_info(&message)),
    };
    let mut values = Vec::with_capacity(members.len());
    for member in members {
        match eval_expression_fragment(member, stack, ctx) {
            Eval::Normal(value) => values.push(value),
            other => return other,
        }
    }
    Eval::Normal(Value::list(values))
}

/// Split on commas at the set literal's top level while respecting AL strings,
/// quoted identifiers, and nested call/index/group delimiters.
fn split_set_members(source: &str) -> Result<Vec<&str>, String> {
    if source.trim().is_empty() {
        return Ok(Vec::new());
    }

    let bytes = source.as_bytes();
    let mut members = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if !single_quoted && !double_quoted {
            if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
                line_comment = true;
                index += 2;
                continue;
            }
            if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                block_comment = true;
                index += 2;
                continue;
            }
            if bytes[index] == b'#' {
                line_comment = true;
                index += 1;
                continue;
            }
        }
        match bytes[index] {
            b'\'' if !double_quoted => {
                if single_quoted && bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                single_quoted = !single_quoted;
            }
            b'"' if !single_quoted => {
                if double_quoted && bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                    continue;
                }
                double_quoted = !double_quoted;
            }
            b'(' if !single_quoted && !double_quoted => paren_depth += 1,
            b')' if !single_quoted && !double_quoted => {
                paren_depth = paren_depth
                    .checked_sub(1)
                    .ok_or_else(|| "set literal: unmatched `)`".to_string())?;
            }
            b'[' if !single_quoted && !double_quoted => bracket_depth += 1,
            b']' if !single_quoted && !double_quoted => {
                bracket_depth = bracket_depth
                    .checked_sub(1)
                    .ok_or_else(|| "set literal: unmatched `]`".to_string())?;
            }
            b'{' if !single_quoted && !double_quoted => brace_depth += 1,
            b'}' if !single_quoted && !double_quoted => {
                brace_depth = brace_depth
                    .checked_sub(1)
                    .ok_or_else(|| "set literal: unmatched `}`".to_string())?;
            }
            b',' if !single_quoted
                && !double_quoted
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0 =>
            {
                let member = source[start..index].trim();
                if member.is_empty() {
                    return Err("set literal: empty member".to_string());
                }
                members.push(member);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }

    if single_quoted
        || double_quoted
        || paren_depth != 0
        || bracket_depth != 0
        || brace_depth != 0
        || block_comment
    {
        return Err("set literal: unterminated quote or nested delimiter".to_string());
    }
    let member = source[start..].trim();
    if member.is_empty() {
        return Err("set literal: empty trailing member".to_string());
    }
    members.push(member);
    Ok(members)
}

/// Parse one set member as a normal AL expression, then evaluate it against the
/// caller's existing scope and dispatch context.
///
/// Parses are memoized in `ctx.expr_fragment_cache` (keyed by the fragment
/// text): a set literal evaluated inside a loop re-parses each member once,
/// not once per iteration. `tree_sitter::Tree` clones are cheap (refcounted)
/// and the wrapper source is an `Arc<str>`, so taking a clone out of the
/// cache avoids borrowing `ctx` across the evaluation below without
/// reallocating the wrapper text on every hit.
fn eval_expression_fragment(
    expression: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let cached = ctx.expr_fragment_cache.get(expression).cloned();
    let (wrapper, tree) = match cached {
        Some(entry) => entry,
        None => {
            let wrapper = format!(
                "codeunit 0 __SetExpression {{ procedure __Eval(): Variant begin exit({expression}); end; }}"
            );
            let parsed = al_syntax::AlParser::parse_quick(&wrapper);
            if !parsed.errors.is_empty() {
                return Eval::Error(error_info(format!(
                    "set literal member is not a valid expression: `{expression}`"
                )));
            }
            let entry: (std::sync::Arc<str>, _) = (wrapper.into(), parsed.tree);
            ctx.expr_fragment_cache
                .insert(expression.to_string(), entry.clone());
            entry
        }
    };

    fn find_expression<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
        if node.kind() == "exit_statement" {
            let argument_list = node
                .named_child(0)
                .filter(|child| child.kind() == "argument_list")
                .or_else(|| {
                    let mut cursor = node.walk();
                    let found = node
                        .named_children(&mut cursor)
                        .find(|child| child.kind() == "argument_list");
                    found
                })?;
            let mut stack = vec![argument_list];
            while let Some(candidate) = stack.pop() {
                if candidate.kind() == "expression" {
                    return Some(candidate);
                }
                let mut cursor = candidate.walk();
                stack.extend(candidate.named_children(&mut cursor));
            }
            return None;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if let Some(expression) = find_expression(child) {
                return Some(expression);
            }
        }
        None
    }

    let Some(node) = find_expression(tree.root_node()) else {
        return Eval::Error(error_info(
            "set literal member expression could not be recovered",
        ));
    };
    eval_expr(node, wrapper.as_bytes(), stack, ctx)
}

/// Evaluate an AL date literal (`20240701D`, `0D`) into a `Value::Date`.
pub(super) fn eval_date_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(error_info("invalid date literal text"));
    };
    let digits = text.trim().trim_end_matches(['d', 'D']);
    // `0D` is AL's undefined/zero date.
    if digits == "0" {
        return Eval::Normal(Value::Date(0));
    }
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Eval::Error(error_info(format!("malformed date literal: {text}")));
    }
    let parse_component = |digits: &str, component: &str| {
        digits.parse::<i64>().map_err(|error| {
            error_info(format!(
                "malformed {component} in date literal {text}: {error}"
            ))
        })
    };
    let year = match parse_component(&digits[0..4], "year") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    let month = match parse_component(&digits[4..6], "month") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    let day = match parse_component(&digits[6..8], "day") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    };
    if !(1..=9999).contains(&year) || day < 1 || day > max_day {
        return Eval::Error(error_info(format!("date literal out of range: {text}")));
    }
    Eval::Normal(Value::Date(value::al_days_from_ymd(year, month, day)))
}

/// Evaluate an AL time literal (`063030T`, `063030500T`, `0T`) into a
/// `Value::Time` (milliseconds since midnight). Digits are `HHMMSS` with an
/// optional trailing thousandths group.
pub(super) fn eval_time_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(error_info("invalid time literal text"));
    };
    let digits = text.trim().trim_end_matches(['t', 'T']);
    if digits == "0" {
        return Eval::Normal(Value::Time(0));
    }
    if !(6..=9).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Eval::Error(error_info(format!("malformed time literal: {text}")));
    }
    let parse_component = |digits: &str, component: &str| {
        digits.parse::<i64>().map_err(|error| {
            error_info(format!(
                "malformed {component} in time literal {text}: {error}"
            ))
        })
    };
    let hours = match parse_component(&digits[0..2], "hour") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    let minutes = match parse_component(&digits[2..4], "minute") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    let seconds = match parse_component(&digits[4..6], "second") {
        Ok(value) => value,
        Err(error) => return Eval::Error(error),
    };
    // Optional thousandths: pad/truncate the trailing group to exactly 3 digits.
    let millis: i64 = if digits.len() > 6 {
        let frac = &digits[6..];
        let frac3: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
        match parse_component(&frac3, "millisecond") {
            Ok(value) => value,
            Err(error) => return Eval::Error(error),
        }
    } else {
        0
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return Eval::Error(error_info(format!("time literal out of range: {text}")));
    }
    let ms = ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis;
    Eval::Normal(Value::Time(ms))
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Resolve a niladic clock builtin used without parentheses (`Today`,
/// `Time`, `CurrentDateTime`, `WorkDate`). Returns `None` for any other
/// identifier.
pub(super) fn niladic_clock_builtin(name: &str, ctx: &DispatchCtx) -> Option<Value> {
    match name.to_ascii_lowercase().as_str() {
        "today" => Some(Value::Date(crate::interpreter::dispatch::clock_today())),
        "time" => Some(Value::Time(crate::interpreter::dispatch::clock_time())),
        "currentdatetime" => Some(Value::DateTime(
            crate::interpreter::dispatch::clock_current_datetime(),
        )),
        "workdate" => Some(Value::Date(
            ctx.work_date
                .unwrap_or_else(crate::interpreter::dispatch::clock_today),
        )),
        _ => None,
    }
}
