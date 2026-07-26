//! Expression evaluator for the AL interpreter.
//!
//! Evaluates the leaf forms a typical AL expression statement walks
//! through: literals, identifier loads, binary/unary arithmetic and
//! comparisons, string concatenation, parenthesised groups, member
//! lookups, and procedure-call expressions (delegated to `dispatch`).

use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::records;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{self, Decimal, ErrorInfo, Value};

pub fn eval_expr(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Expression evaluation recurses per AST nesting level. About 400 nested
    // parentheses overflow a 2 MiB worker stack, so mirror eval_stmt's guard.
    if !stack.enter_expr() {
        return Eval::Error(simple_error(&format!(
            "expression nesting depth exceeded (max {} levels) — likely a pathological or generated test source",
            crate::interpreter::scope::MAX_EXPR_DEPTH
        )));
    }
    let result = eval_expr_inner(node, source, stack, ctx);
    stack.exit_expr();
    record_condition_trace(node, &result, ctx);
    result
}

/// Attach a source-ordered Boolean-condition trace to an evaluated syntax node
/// while dynamic coverage is tracing a decision.
///
/// Logical chains install their precise combined trace in
/// `eval_expression_node`. This fallback propagates that trace through
/// transparent grammar wrappers; genuinely atomic Boolean values (a variable,
/// literal, comparison, field access, or Boolean-returning call) become one
/// condition. Procedure-call internals are intentionally not exposed as
/// conditions of the caller's decision.
fn record_condition_trace(node: Node<'_>, result: &Eval, ctx: &mut DispatchCtx) {
    if !ctx.cov_condition_trace_active() || ctx.cov_expression_trace(node).is_some() {
        return;
    }
    let Eval::Normal(Value::Boolean(outcome)) = result else {
        return;
    };

    let transparent = match node.kind() {
        "expression"
        | "parenthesized_expression"
        | "primary_expression"
        | "case_label_expression" => true,
        "unary_expression" => true,
        "postfix_expression" => !crate::interpreter::eval_stmt::is_call_postfix(node),
        _ => false,
    };
    let inherited = transparent.then(|| {
        let mut cursor = node.walk();
        let trace = node
            .named_children(&mut cursor)
            .find_map(|child| ctx.cov_expression_trace(child));
        trace
    });
    ctx.cov_set_expression_trace(node, inherited.flatten().unwrap_or_else(|| vec![*outcome]));
}

fn eval_expr_inner(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match node.kind() {
        // Literal forms — the AL grammar uses `integer`, `decimal`, `string`
        // as the actual node kinds (not `integer_literal` etc.).
        "integer_literal" | "integer" => match utf8_text(node, source) {
            Some(t) => match int_literal_value(t) {
                Some(v) => Eval::Normal(v),
                None => Eval::Error(simple_error(&format!("malformed integer literal: {t}"))),
            },
            None => Eval::Error(simple_error("invalid integer literal text")),
        },
        "decimal_literal" | "decimal" => {
            match utf8_text(node, source).and_then(|t| t.parse::<Decimal>().ok()) {
                Some(n) => Eval::Normal(Value::Decimal(n)),
                None => Eval::Error(simple_error("malformed decimal literal")),
            }
        }
        "boolean_literal" => eval_literal(node, source),
        // Date / Time / DateTime literals — `20240701D`, `063030T`. The
        // lexer tokenises these; evaluation maps them onto the day/ms carriers.
        "date_literal" => eval_date_literal(node, source),
        "time_literal" => eval_time_literal(node, source),
        "string_literal" | "string" | "verbatim_string" => {
            let text = utf8_text(node, source).unwrap_or("");
            let trimmed = text.trim_start_matches('\'').trim_end_matches('\'');
            let unescaped = trimmed.replace("''", "'");
            Eval::Normal(Value::Text(unescaped))
        }
        // The AL grammar uses `expression` as the binary expression node:
        //   expression = unary_expression (binary_operator unary_expression)*
        // When there are 3+ named children, it's a binary (or assignment) expression.
        // When there is 1 named child, it's a transparent wrapper.
        "expression" => eval_expression_node(node, source, stack, ctx),
        // A postfix_expression is a call (`Foo(args)`, `Recv.Proc(args)`), a
        // scope-qualified enum access (`"Enum"::Member`), a record field read
        // (`Rec."Field"`), or a transparent wrapper around a primary_expression.
        // Calls and record reads need the dispatch context; scope access yields
        // an Option value; everything else unwraps.
        "postfix_expression" => eval_postfix(node, source, stack, ctx),
        "parenthesized_expression" | "primary_expression" | "case_label_expression" => {
            match named_child(node, 0) {
                Some(inner) => eval_expr(inner, source, stack, ctx),
                None => Eval::Error(simple_error("empty expression wrapper")),
            }
        }
        // In expression position the grammar's lossless generic bracket block
        // is an AL set literal (`[A, B, Low .. High]`). Evaluate each top-level
        // member as an ordinary expression so calls, enum scopes, variables,
        // and ranges use exactly the same semantics as expressions elsewhere.
        "bracketed_block" => eval_set_literal(node, source, stack, ctx),
        "identifier" | "variable_reference" | "name" => match utf8_text(node, source) {
            Some(name) => {
                // Boolean keywords may appear as identifiers in some grammar versions.
                match name.to_ascii_lowercase().as_str() {
                    "true" => return Eval::Normal(Value::Boolean(true)),
                    "false" => return Eval::Normal(Value::Boolean(false)),
                    _ => {}
                }
                match stack.lookup(name) {
                    Some(v) => Eval::Normal(v.clone()),
                    // Niladic clock builtins may appear without parentheses
                    // (`dt := CurrentDateTime`). Only treated as builtins when
                    // not shadowed by a bound variable of the same name.
                    None => match niladic_clock_builtin(name) {
                        Some(v) => Eval::Normal(v),
                        None => Eval::Error(simple_error(&format!("unbound identifier: {name}"))),
                    },
                }
            }
            None => Eval::Error(simple_error("invalid identifier text")),
        },
        "unary_expression" => eval_unary(node, source, stack, ctx),
        // Anything else: signal a clear error rather than silently
        // returning a default — failing loud is better than failing wrong.
        other => Eval::Error(simple_error(&format!(
            "unsupported expression kind: {other}"
        ))),
    }
}

fn simple_error(message: &str) -> ErrorInfo {
    ErrorInfo {
        message: message.to_string(),
        error_type: None,
        source: None,
    }
}

fn utf8_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

/// Type an integer literal. An `l`/`L` suffix — AL's `BigInteger` literal
/// marker — or a magnitude outside the 32-bit `Integer` range yields a
/// `BigInteger`; otherwise `Integer`.
fn int_literal_value(text: &str) -> Option<Value> {
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

fn named_child(node: Node<'_>, index: usize) -> Option<Node<'_>> {
    node.named_child(index)
}

fn eval_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("invalid literal text"));
    };
    match node.kind() {
        "integer_literal" => match int_literal_value(text) {
            Some(v) => Eval::Normal(v),
            None => Eval::Error(simple_error(&format!("malformed integer literal: {text}"))),
        },
        "decimal_literal" => match text.parse::<Decimal>() {
            Ok(n) => Eval::Normal(Value::Decimal(n)),
            Err(_) => Eval::Error(simple_error(&format!("malformed decimal literal: {text}"))),
        },
        "boolean_literal" => match text.eq_ignore_ascii_case("true") {
            true => Eval::Normal(Value::Boolean(true)),
            false => Eval::Normal(Value::Boolean(false)),
        },
        "string_literal" => {
            // Strip leading/trailing single-quote and unescape doubled
            // single-quotes (AL's escape mechanism).
            let trimmed = text.trim_start_matches('\'').trim_end_matches('\'');
            let unescaped = trimmed.replace("''", "'");
            Eval::Normal(Value::Text(unescaped))
        }
        other => Eval::Error(simple_error(&format!("unknown literal kind: {other}"))),
    }
}

fn eval_unary(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Grammar: unary_expression = (unary_operator unary_expression) | postfix_expression
    //   - 2 named children: [unary_operator, unary_expression]
    //   - 1 named child:    [postfix_expression] — transparent wrapper
    let named_count = node.named_child_count();
    if named_count <= 1 {
        return match named_child(node, 0) {
            Some(inner) => eval_expr(inner, source, stack, ctx),
            None => Eval::Error(simple_error("unary expression: empty node")),
        };
    }
    let op_node = match named_child(node, 0) {
        Some(n) => n,
        None => return Eval::Error(simple_error("unary expression missing operator")),
    };
    let operand_node = match named_child(node, 1) {
        Some(n) => n,
        None => return Eval::Error(simple_error("unary expression missing operand")),
    };
    let operator_text = utf8_text(op_node, source).unwrap_or("").trim();

    let value = match eval_expr(operand_node, source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    match (operator_text.to_ascii_lowercase().as_str(), value) {
        ("+", Value::Integer(n)) => Eval::Normal(Value::Integer(n)),
        ("+", Value::BigInteger(n)) => Eval::Normal(Value::BigInteger(n)),
        ("+", Value::Decimal(n)) => Eval::Normal(Value::Decimal(n)),
        ("+", Value::Option { ordinal, .. }) => Eval::Normal(Value::Integer(ordinal)),
        ("-", Value::Integer(n)) => checked_int(n.checked_neg(), false),
        ("-", Value::BigInteger(n)) => checked_int(n.checked_neg(), true),
        ("-", Value::Decimal(n)) => Eval::Normal(Value::Decimal(-n)),
        ("-", Value::Option { ordinal, .. }) => checked_int(ordinal.checked_neg(), false),
        ("not", Value::Boolean(b)) => Eval::Normal(Value::Boolean(!b)),
        (op, v) => Eval::Error(simple_error(&format!(
            "unary operator `{op}` not supported on {}",
            v.type_name()
        ))),
    }
}

/// Evaluate an AL set literal represented by the grammar's generic
/// `bracketed_block`.
fn eval_set_literal(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("set literal: invalid source text"));
    };
    let Some(inner) = text
        .strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
    else {
        return Eval::Error(simple_error("set literal: missing brackets"));
    };
    let members = match split_set_members(inner) {
        Ok(members) => members,
        Err(message) => return Eval::Error(simple_error(&message)),
    };
    let mut values = Vec::with_capacity(members.len());
    for member in members {
        match eval_expression_fragment(member, stack, ctx) {
            Eval::Normal(value) => values.push(value),
            other => return other,
        }
    }
    Eval::Normal(Value::List(values))
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
fn eval_expression_fragment(
    expression: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let wrapper = format!(
        "codeunit 0 __SetExpression {{ procedure __Eval(): Variant begin exit({expression}); end; }}"
    );
    let parsed = al_syntax::AlParser::parse_quick(&wrapper);
    if !parsed.errors.is_empty() {
        return Eval::Error(simple_error(&format!(
            "set literal member is not a valid expression: `{expression}`"
        )));
    }

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

    let Some(node) = find_expression(parsed.tree.root_node()) else {
        return Eval::Error(simple_error(
            "set literal member expression could not be recovered",
        ));
    };
    eval_expr(node, wrapper.as_bytes(), stack, ctx)
}

/// Evaluate a `postfix_expression`: a primary expression followed by zero or
/// more suffixes. Three shapes:
///   * call suffix (`Foo(args)`, `Recv.Proc(args)`, `Cu::Run(args)`) →
///     dispatched through `eval_stmt::eval_call` (needs `ctx`).
///   * scope suffix (`"Enum"::Member`, `Enum::"T"::"V"`) → an Option value.
///   * no suffix → transparent wrapper around the primary expression.
fn eval_postfix(
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
        if let Some(Value::Record(rv)) = stack.lookup(&recv) {
            let table_name = rv.table_name.clone();
            return records::field_get(&table_name, &field, ctx);
        }
    }

    // Plain wrapper — evaluate the primary expression.
    match named_child(node, 0) {
        Some(inner) => eval_expr(inner, source, stack, ctx),
        None => Eval::Error(simple_error("empty postfix expression")),
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
fn eval_scope_access(
    node: Node<'_>,
    scope_members: &[Node<'_>],
    source: &[u8],
    ctx: &DispatchCtx,
) -> Eval {
    let member_name = |n: Node<'_>| -> Option<String> {
        n.child_by_field_name("member")
            .or_else(|| n.named_child(0))
            .and_then(|m| m.utf8_text(source).ok())
            .map(|t| t.trim_matches('"').to_string())
    };

    let primary_text = node
        .named_child(0)
        .and_then(|p| p.utf8_text(source).ok())
        .map(|t| t.trim_matches('"').to_string())
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
            _ => return Eval::Error(simple_error("scope access: missing enum member")),
        }
    } else {
        // `"Type"::Member` — primary is the type, the suffix is the member.
        let Some(member) = scope_members.last().and_then(|n| member_name(*n)) else {
            return Eval::Error(simple_error("scope access: missing enum member"));
        };
        (primary_text, member)
    };

    let Some(ordinal) = resolve_workspace_enum_ordinal(ctx, &type_name, &member) else {
        return Eval::Error(simple_error(&format!(
            "enum member '{type_name}::{member}' has no workspace declaration; live BC execution is required"
        )));
    };
    Eval::Normal(Value::Option {
        type_name,
        member,
        ordinal,
    })
}

fn resolve_workspace_enum_ordinal(ctx: &DispatchCtx, type_name: &str, member: &str) -> Option<i64> {
    let path = ctx.source.find_by_object_name(type_name)?;
    let (text, tree) = ctx.source.get_cached_parse(&path)?;
    let bytes = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_value_declaration" {
            let name = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(bytes).ok())
                .map(|name| name.trim().trim_matches('"'));
            if name.is_some_and(|name| name.eq_ignore_ascii_case(member)) {
                return node
                    .child_by_field_name("id")
                    .and_then(|id| id.utf8_text(bytes).ok())
                    .and_then(|id| id.trim().parse::<i64>().ok());
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

/// Evaluate an AL date literal (`20240701D`, `0D`) into a `Value::Date`.
fn eval_date_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("invalid date literal text"));
    };
    let digits = text.trim().trim_end_matches(['d', 'D']);
    // `0D` is AL's undefined/zero date.
    if digits == "0" {
        return Eval::Normal(Value::Date(0));
    }
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Eval::Error(simple_error(&format!("malformed date literal: {text}")));
    }
    let parse_component = |digits: &str, component: &str| {
        digits.parse::<i64>().map_err(|error| {
            simple_error(&format!(
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
        return Eval::Error(simple_error(&format!("date literal out of range: {text}")));
    }
    Eval::Normal(Value::Date(value::al_days_from_ymd(year, month, day)))
}

/// Evaluate an AL time literal (`063030T`, `063030500T`, `0T`) into a
/// `Value::Time` (milliseconds since midnight). Digits are `HHMMSS` with an
/// optional trailing thousandths group.
fn eval_time_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("invalid time literal text"));
    };
    let digits = text.trim().trim_end_matches(['t', 'T']);
    if digits == "0" {
        return Eval::Normal(Value::Time(0));
    }
    if !(6..=9).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Eval::Error(simple_error(&format!("malformed time literal: {text}")));
    }
    let parse_component = |digits: &str, component: &str| {
        digits.parse::<i64>().map_err(|error| {
            simple_error(&format!(
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
        return Eval::Error(simple_error(&format!("time literal out of range: {text}")));
    }
    let ms = ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis;
    Eval::Normal(Value::Time(ms))
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Resolve a niladic clock builtin used without parentheses (`Today`,
/// `Time`, `CurrentDateTime`). Returns `None` for any other identifier.
fn niladic_clock_builtin(name: &str) -> Option<Value> {
    match name.to_ascii_lowercase().as_str() {
        "today" => Some(Value::Date(crate::interpreter::dispatch::clock_today())),
        "time" => Some(Value::Time(crate::interpreter::dispatch::clock_time())),
        "currentdatetime" => Some(Value::DateTime(
            crate::interpreter::dispatch::clock_current_datetime(),
        )),
        _ => None,
    }
}

/// Handle the AL `expression` node.
///
/// The grammar defines:
///   expression = unary_expression (binary_operator unary_expression)*
///
/// The tree is FLAT: all operators and operands are direct named children.
/// Named children alternate: operand, operator, operand, operator, operand, ...
///
/// We collect all named children into a list, then find the `:=` operator
/// (assignment, lowest precedence) and process accordingly. Pure computation
/// is reduced with AL's documented operator hierarchy; the grammar deliberately
/// keeps the source expression flat.
fn eval_expression_node(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let named_count = node.named_child_count();

    let children: Vec<Node<'_>> = (0..named_count)
        .filter_map(|i| node.named_child(i))
        .collect();

    if children.is_empty() {
        return Eval::Error(simple_error("expression: no children"));
    }
    if children.len() == 1 {
        // Transparent wrapper.
        return eval_expr(children[0], source, stack, ctx);
    }

    // Operators are at odd indices: [operand, op, operand, op, operand, ...]
    // Recognise the assignment operators — plain `:=` and the compound
    // arithmetic forms `+=`, `-=`, `*=`, `/=`. Each binds the LHS variable
    // and is the lowest-precedence operator in the chain.
    let assign = children.iter().enumerate().find_map(|(idx, c)| {
        if idx % 2 != 1 || c.kind() != "binary_operator" {
            return None;
        }
        let text = c.utf8_text(source).ok()?.trim();
        assignment_kind(text).map(|kind| (idx, kind))
    });

    if let Some((op_idx, kind)) = assign {
        let lhs_node = children[op_idx - 1];
        let rhs_children = &children[(op_idx + 1)..];

        let mut ignored_trace = None;
        let rhs_val =
            match eval_computation_chain(rhs_children, source, stack, ctx, &mut ignored_trace) {
                Eval::Normal(v) => v,
                other => return other,
            };

        // Record field assignment: `Rec."Field" := value`. Handled before the
        // plain-identifier path so the whole record isn't overwritten.
        if let Some(result) = records::try_field_assign(lhs_node, source, &rhs_val, stack, ctx) {
            return result;
        }

        let lhs_name = extract_identifier_name(lhs_node, source)
            .or_else(|| {
                lhs_node
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.trim_matches('"').to_ascii_lowercase())
            })
            .unwrap_or_default();

        if lhs_name.is_empty() {
            return Eval::Error(simple_error(
                "expression: cannot resolve LHS name for assignment",
            ));
        }

        // Compound assignment (`x += rhs`) is `x := x <op> rhs`: load the
        // current value, apply the base operator, then store. Behaviour is
        // intentionally identical to writing the expanded form by hand.
        let new_val = match kind {
            AssignKind::Plain => rhs_val,
            AssignKind::Compound(base_op) => {
                let Some(current) = stack.lookup(&lhs_name).cloned() else {
                    return Eval::Error(simple_error(&format!(
                        "compound assignment to unbound identifier: {lhs_name}"
                    )));
                };
                match apply_binary(base_op, current, rhs_val) {
                    Eval::Normal(v) => v,
                    other => return other,
                }
            }
        };

        if let Some(slot) = stack.lookup_mut(&lhs_name) {
            // Preserve the slot's declared type (Code caselessness / integer
            // width) rather than adopting the RHS's — see `coerce_into_slot`.
            *slot = Value::coerce_into_slot(slot, new_val);
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&lhs_name, new_val);
        } else {
            return Eval::Error(simple_error("expression: no active scope for assignment"));
        }
        return Eval::Normal(Value::Empty);
    }

    let mut condition_trace = None;
    let result = eval_computation_chain(&children, source, stack, ctx, &mut condition_trace);
    if condition_trace.is_some() {
        ctx.cov_set_expression_trace(node, condition_trace.unwrap_or_default());
    }
    result
}

/// The two flavours of AL assignment operator.
enum AssignKind {
    /// `:=` — store the RHS directly.
    Plain,
    /// `+=` / `-=` / `*=` / `/=` — apply the carried base operator to the
    /// current LHS value and the RHS, then store.
    Compound(&'static str),
}

/// Classify a binary-operator token as an assignment, if it is one.
fn assignment_kind(text: &str) -> Option<AssignKind> {
    match text {
        ":=" => Some(AssignKind::Plain),
        "+=" => Some(AssignKind::Compound("+")),
        "-=" => Some(AssignKind::Compound("-")),
        "*=" => Some(AssignKind::Compound("*")),
        "/=" => Some(AssignKind::Compound("/")),
        _ => None,
    }
}

/// Precedence of AL's flat binary-expression operators.
///
/// Postfix and unary operators are already represented by nested syntax nodes,
/// so this table starts with the multiplicative/logical tier. Larger values
/// bind more tightly. Every binary tier is left-associative.
fn binary_precedence(operator: &str) -> Option<u8> {
    match operator.to_ascii_lowercase().as_str() {
        "*" | "/" | "div" | "mod" | "and" | "xor" => Some(4),
        "+" | "-" | "or" => Some(3),
        ">" | ">=" | "<" | "<=" | "=" | "<>" | "in" => Some(2),
        ".." => Some(1),
        _ => None,
    }
}

/// One token in a reverse-Polish representation of a flat AL expression.
///
/// Building this representation before evaluation gives us the official
/// precedence without manufacturing a recursive tree. Operands retain source
/// order in RPN, and operators are applied as soon as their complete
/// precedence group is available, so an earlier runtime error still prevents
/// later source operands from running.
enum ChainToken<'tree> {
    Operand(Node<'tree>),
    Operator(String),
}

/// Evaluate a complete flat AL computation, including the conditional
/// `condition ? when_true : when_false` operator.
///
/// Conditional expressions bind less tightly than the ordinary binary
/// operators and associate to the right. They also evaluate exactly one
/// result branch. Keeping this dispatch above the RPN binary evaluator is
/// important: putting `?` and `:` into the RPN table would eagerly evaluate
/// both branches and make side effects and runtime errors observably wrong.
fn eval_computation_chain(
    children: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
    condition_trace: &mut Option<Vec<bool>>,
) -> Eval {
    let question_index = children.iter().enumerate().find_map(|(index, child)| {
        (index % 2 == 1
            && child.kind() == "binary_operator"
            && utf8_text(*child, source).is_some_and(|text| text.trim() == "?"))
        .then_some(index)
    });

    let Some(question_index) = question_index else {
        return eval_expr_chain(children, source, stack, ctx, condition_trace);
    };

    // Find the colon paired with the first question mark. Any conditional in
    // the true arm increments the nesting depth, so `a ? b ? c : d : e`
    // selects the final colon while `a ? b : c ? d : e` selects the first.
    let mut nested_questions = 0usize;
    let mut colon_index = None;
    for index in ((question_index + 2)..children.len()).step_by(2) {
        let child = children[index];
        if child.kind() != "binary_operator" {
            continue;
        }
        match utf8_text(child, source).unwrap_or("").trim() {
            "?" => nested_questions += 1,
            ":" if nested_questions == 0 => {
                colon_index = Some(index);
                break;
            }
            ":" => nested_questions -= 1,
            _ => {}
        }
    }
    let Some(colon_index) = colon_index else {
        return Eval::Error(simple_error(
            "conditional expression: `?` has no matching `:`",
        ));
    };

    let condition_children = &children[..question_index];
    let true_children = &children[(question_index + 1)..colon_index];
    let false_children = &children[(colon_index + 1)..];
    if condition_children.is_empty() || true_children.is_empty() || false_children.is_empty() {
        return Eval::Error(simple_error(
            "conditional expression: condition and both result branches are required",
        ));
    }

    let mut condition_conditions = None;
    let condition = match eval_computation_chain(
        condition_children,
        source,
        stack,
        ctx,
        &mut condition_conditions,
    ) {
        Eval::Normal(Value::Boolean(value)) => value,
        Eval::Normal(other) => {
            return Eval::Error(simple_error(&format!(
                "conditional expression requires Boolean condition, got {}",
                other.type_name()
            )));
        }
        other => return other,
    };

    let mut selected_trace = None;
    let result = if condition {
        eval_computation_chain(true_children, source, stack, ctx, &mut selected_trace)
    } else {
        eval_computation_chain(false_children, source, stack, ctx, &mut selected_trace)
    };
    if matches!(result, Eval::Normal(Value::Boolean(_))) {
        *condition_trace = selected_trace.or_else(|| match &result {
            Eval::Normal(Value::Boolean(value)) => Some(vec![*value]),
            _ => None,
        });
    }
    result
}

/// Evaluate a flat alternating chain [operand, op, operand, op, operand, ...]
/// using AL operator precedence and left associativity.
fn eval_expr_chain(
    children: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
    condition_trace: &mut Option<Vec<bool>>,
) -> Eval {
    if children.is_empty() {
        return Eval::Error(simple_error("expression chain: empty"));
    }
    if children.len() == 1 {
        return eval_expr(children[0], source, stack, ctx);
    }

    if children.len().is_multiple_of(2) {
        return Eval::Error(simple_error(
            "expression chain: expected alternating operands and operators",
        ));
    }

    // Shunting-yard conversion. Operators at equal precedence are popped,
    // making every tier left-associative as AL specifies.
    let mut output = Vec::with_capacity(children.len());
    let mut operators: Vec<(String, u8)> = Vec::with_capacity(children.len() / 2);
    for (index, child) in children.iter().copied().enumerate() {
        if index % 2 == 0 {
            output.push(ChainToken::Operand(child));
            continue;
        }

        let operator = utf8_text(child, source)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let Some(precedence) = binary_precedence(&operator) else {
            return Eval::Error(simple_error(&format!(
                "unsupported binary operator in expression: `{operator}`"
            )));
        };
        while operators
            .last()
            .is_some_and(|(_, stacked_precedence)| *stacked_precedence >= precedence)
        {
            let (stacked, _) = operators.pop().expect("last operator exists");
            output.push(ChainToken::Operator(stacked));
        }
        operators.push((operator, precedence));
    }
    while let Some((operator, _)) = operators.pop() {
        output.push(ChainToken::Operator(operator));
    }

    struct TracedValue {
        value: Value,
        conditions: Option<Vec<bool>>,
    }

    let mut values: Vec<TracedValue> = Vec::with_capacity(children.len().div_ceil(2));
    for token in output {
        match token {
            ChainToken::Operand(node) => match eval_expr(node, source, stack, ctx) {
                Eval::Normal(value) => values.push(TracedValue {
                    conditions: ctx.cov_expression_trace(node),
                    value,
                }),
                other => return other,
            },
            ChainToken::Operator(operator) => {
                let Some(right) = values.pop() else {
                    return Eval::Error(simple_error(
                        "expression chain: binary operator missing right operand",
                    ));
                };
                let Some(left) = values.pop() else {
                    return Eval::Error(simple_error(
                        "expression chain: binary operator missing left operand",
                    ));
                };
                let logical = matches!(operator.as_str(), "and" | "or" | "xor");
                let left_atomic = match &left.value {
                    Value::Boolean(value) => Some(*value),
                    _ => None,
                };
                let right_atomic = match &right.value {
                    Value::Boolean(value) => Some(*value),
                    _ => None,
                };
                match apply_binary(&operator, left.value, right.value) {
                    Eval::Normal(value) => {
                        let conditions = match &value {
                            Value::Boolean(_) if logical => {
                                let mut combined = left
                                    .conditions
                                    .or_else(|| left_atomic.map(|value| vec![value]))
                                    .unwrap_or_default();
                                combined.extend(
                                    right
                                        .conditions
                                        .or_else(|| right_atomic.map(|value| vec![value]))
                                        .unwrap_or_default(),
                                );
                                Some(combined)
                            }
                            // A comparison (including `in`) is one atomic
                            // condition even when its operands contain their
                            // own Boolean calculations.
                            Value::Boolean(outcome) => Some(vec![*outcome]),
                            _ => None,
                        };
                        values.push(TracedValue { value, conditions });
                    }
                    other => return other,
                }
            }
        }
    }

    match values.pop() {
        Some(value) if values.is_empty() => {
            *condition_trace = value.conditions;
            Eval::Normal(value.value)
        }
        _ => Eval::Error(simple_error(
            "expression chain: invalid operand/operator structure",
        )),
    }
}

fn extract_identifier_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "name" | "variable_reference" => {
                return child
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.trim_matches('"').to_ascii_lowercase());
            }
            "unary_expression" | "postfix_expression" | "primary_expression" => {
                if let Some(name) = extract_identifier_name(child, source) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// A numeric operand classified for arithmetic: an integer-like value (with a
/// flag for whether it is a `BigInteger`, which widens the overflow trap from
/// i32 to i64) or a `Decimal`.
enum Num {
    Int { val: i64, big: bool },
    Dec(Decimal),
}

impl Num {
    fn to_decimal(&self) -> Decimal {
        match self {
            Num::Int { val, .. } => Decimal::from(*val),
            Num::Dec(d) => *d,
        }
    }
}

fn classify_numeric(v: &Value) -> Option<Num> {
    match v {
        Value::Integer(n) => Some(Num::Int {
            val: *n,
            big: false,
        }),
        Value::BigInteger(n) => Some(Num::Int { val: *n, big: true }),
        Value::Char(value) => Some(Num::Int {
            val: *value as i64,
            big: false,
        }),
        Value::Option { ordinal, .. } => Some(Num::Int {
            val: *ordinal,
            big: false,
        }),
        Value::Decimal(d) => Some(Num::Dec(*d)),
        _ => None,
    }
}

/// Convert an AL integer-like/whole-decimal offset used by Date/Time
/// arithmetic. Fractional Decimal offsets are invalid for these operations.
fn whole_offset(value: &Value) -> Result<i64, ErrorInfo> {
    match classify_numeric(value) {
        Some(Num::Int { val, .. }) => Ok(val),
        Some(Num::Dec(decimal)) if decimal.fract().is_zero() => decimal
            .to_string()
            .parse::<i64>()
            .map_err(|_| simple_error("Date/Time arithmetic offset is out of range")),
        Some(Num::Dec(_)) => Err(simple_error(
            "Date/Time arithmetic requires a whole-number offset",
        )),
        None => Err(simple_error(&format!(
            "Date/Time arithmetic does not support {}",
            value.type_name()
        ))),
    }
}

fn apply_temporal_arithmetic(operator: &str, left: &Value, right: &Value) -> Option<Eval> {
    let op = operator.to_ascii_lowercase();
    match (op.as_str(), left, right) {
        ("+", Value::Date(date), offset) | ("+", offset, Value::Date(date)) => {
            if *date == 0 {
                return Some(Eval::Error(simple_error(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    date.checked_add(offset)
                        .ok_or_else(|| simple_error("Date arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Date(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("-", Value::Date(left), Value::Date(right)) => {
            if *left == 0 || *right == 0 {
                return Some(Eval::Error(simple_error(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(checked_int(left.checked_sub(*right), false))
        }
        ("-", Value::Date(date), offset) => {
            if *date == 0 {
                return Some(Eval::Error(simple_error(
                    "Date arithmetic is undefined for 0D",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    date.checked_sub(offset)
                        .ok_or_else(|| simple_error("Date arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Date(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("+", Value::Time(time), offset) | ("+", offset, Value::Time(time)) => {
            if *time == 0 {
                return Some(Eval::Error(simple_error(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    time.checked_add(offset)
                        .filter(|value| (0..value::MS_PER_DAY).contains(value))
                        .ok_or_else(|| simple_error("Time arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Time(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        ("-", Value::Time(left), Value::Time(right)) => {
            if *left == 0 || *right == 0 {
                return Some(Eval::Error(simple_error(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            Some(checked_int(left.checked_sub(*right), false))
        }
        ("-", Value::Time(time), offset) => {
            if *time == 0 {
                return Some(Eval::Error(simple_error(
                    "Time arithmetic is undefined for 0T",
                )));
            }
            Some(
                match whole_offset(offset).and_then(|offset| {
                    time.checked_sub(offset)
                        .filter(|value| (0..value::MS_PER_DAY).contains(value))
                        .ok_or_else(|| simple_error("Time arithmetic overflow"))
                }) {
                    Ok(value) => Eval::Normal(Value::Time(value)),
                    Err(error) => Eval::Error(error),
                },
            )
        }
        _ => None,
    }
}

/// Wrap an integer arithmetic result at the correct width. BC's `Integer` is
/// 32-bit and traps overflow at runtime; `BigInteger` is 64-bit. The i64
/// carrier means an i64 `checked_*` alone would let `2147483647 * 3` silently
/// produce a value no `Integer` can hold, so an `Integer` result is also
/// range-checked against i32. Either overflow is a runtime error, never a
/// silent wrap.
fn checked_int(result: Option<i64>, big: bool) -> Eval {
    match result {
        Some(n) if big => Eval::Normal(Value::BigInteger(n)),
        Some(n) if (i32::MIN as i64..=i32::MAX as i64).contains(&n) => {
            Eval::Normal(Value::Integer(n))
        }
        _ => Eval::Error(simple_error("integer overflow")),
    }
}

/// Wrap a Decimal result. `rust_decimal` has no NaN/infinity; the only failure
/// mode is overflow of the 96-bit range, which `checked_*` reports as `None`.
fn checked_decimal(d: Option<Decimal>) -> Eval {
    match d {
        Some(v) => Eval::Normal(Value::Decimal(v)),
        None => Eval::Error(simple_error("decimal arithmetic overflow")),
    }
}

/// Apply `+ - * / div mod` to two classified numeric operands.
///
/// * `/` is AL real division: always `Decimal` (e.g. `Avg := Total / Count`).
/// * `div`/`mod` are integer-only; on any `Decimal` operand they error.
/// * Integer/Integer stays `Integer` (i32 trap); if either side is a
///   `BigInteger` the result is `BigInteger` with an i64 overflow trap.
fn apply_numeric(op: &str, l: Num, r: Num) -> Eval {
    // Real division always promotes to Decimal.
    if op == "/" {
        let (a, b) = (l.to_decimal(), r.to_decimal());
        if b == Decimal::ZERO {
            return Eval::Error(simple_error("division by zero"));
        }
        return checked_decimal(a.checked_div(b));
    }
    match (l, r) {
        (Num::Int { val: a, big: ab }, Num::Int { val: b, big: bb }) => {
            let big = ab || bb;
            match op {
                "+" => checked_int(a.checked_add(b), big),
                "-" => checked_int(a.checked_sub(b), big),
                "*" => checked_int(a.checked_mul(b), big),
                "div" => {
                    if b == 0 {
                        Eval::Error(simple_error("division by zero"))
                    } else {
                        // i32::MIN / -1 (or i64::MIN / -1) overflows → error.
                        checked_int(a.checked_div(b), big)
                    }
                }
                "mod" => {
                    if b == 0 {
                        Eval::Error(simple_error("modulo by zero"))
                    } else {
                        checked_int(a.checked_rem(b), big)
                    }
                }
                _ => unreachable!("op pre-filtered by apply_binary"),
            }
        }
        (l, r) => {
            // At least one Decimal operand → Decimal arithmetic.
            let (a, b) = (l.to_decimal(), r.to_decimal());
            match op {
                "+" => checked_decimal(a.checked_add(b)),
                "-" => checked_decimal(a.checked_sub(b)),
                "*" => checked_decimal(a.checked_mul(b)),
                "div" | "mod" => Eval::Error(simple_error(&format!(
                    "binary operator `{op}` is integer-only (not supported on Decimal)"
                ))),
                _ => unreachable!("op pre-filtered by apply_binary"),
            }
        }
    }
}

/// Apply a binary operator to two values. Made `pub(crate)` so unit tests
/// so mock dispatch can reuse the operator semantics.
pub(crate) fn apply_binary(operator: &str, left: Value, right: Value) -> Eval {
    let op = operator.to_ascii_lowercase();

    if matches!(op.as_str(), "+" | "-") {
        if let Some(result) = apply_temporal_arithmetic(&op, &left, &right) {
            return result;
        }
    }

    // Numeric arithmetic (Integer, BigInteger, Decimal, and every mix) is
    // handled first, before the value-consuming match, so the operand-type
    // classification lives in one place. Non-arithmetic operations and
    // non-numeric operands fall through to the match below.
    if matches!(op.as_str(), "+" | "-" | "*" | "/" | "div" | "mod") {
        if let (Some(l), Some(r)) = (classify_numeric(&left), classify_numeric(&right)) {
            return apply_numeric(&op, l, r);
        }
    }

    match (&op[..], left, right) {
        ("+", Value::Text(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Text(a), Value::Code(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Code(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Code(a), Value::Code(b)) => Eval::Normal(Value::Code(format!("{a}{b}"))),

        ("=", a, b) => Eval::Normal(Value::Boolean(values_equal(&a, &b))),
        ("<>", a, b) => Eval::Normal(Value::Boolean(!values_equal(&a, &b))),
        ("<", a, b) => values_cmp(&a, &b, |o| o.is_lt()),
        ("<=", a, b) => values_cmp(&a, &b, |o| o.is_le()),
        (">", a, b) => values_cmp(&a, &b, |o| o.is_gt()),
        (">=", a, b) => values_cmp(&a, &b, |o| o.is_ge()),

        ("and", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a && b)),
        ("or", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a || b)),
        ("xor", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a ^ b)),
        ("..", start, end) => Eval::Normal(Value::Range {
            start: Box::new(start),
            end: Box::new(end),
        }),
        ("in", value, Value::List(members)) | ("in", value, Value::Array(members)) => {
            for member in members {
                let matched = match member {
                    Value::Range { start, end } => match value_in_range(&value, &start, &end) {
                        Ok(matched) => matched,
                        Err(error) => return Eval::Error(error),
                    },
                    member => values_equal(&value, &member),
                };
                if matched {
                    return Eval::Normal(Value::Boolean(true));
                }
            }
            Eval::Normal(Value::Boolean(false))
        }

        (op, a, b) => Eval::Error(simple_error(&format!(
            "binary operator `{op}` not supported on ({}, {})",
            a.type_name(),
            b.type_name()
        ))),
    }
}

/// AL value equality for `=`/`<>` and CASE matching:
/// - `Text = Text` is **case-sensitive**.
/// - `Code = Code` and `Code = Text` are **case-insensitive** (Code is an
///   uppercased, caseless type; a Text on the other side is coerced to Code).
/// - `Integer = Decimal` compares exactly: the Integer is promoted to an
///   exact `Decimal` (infallible — i64 fits the 96-bit range), so `5 = 5.0`
///   holds and `5 = 5.1` does not, with no floating-point rounding.
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    if let (Some(left), Some(right)) = (classify_numeric(a), classify_numeric(b)) {
        return left.to_decimal() == right.to_decimal();
    }
    use Value::*;
    match (a, b) {
        // Code is caseless in BC; a Text compared to a Code is coerced to Code.
        (Code(x), Code(y)) | (Text(x), Code(y)) | (Code(y), Text(x)) => x.eq_ignore_ascii_case(y),
        // Everything else uses structural equality (`Value`'s Eq): Text,
        // Boolean, Decimal, Date/Time/DateTime, Duration, Guid, Char, Option,
        // Null, Empty, … A cross-type non-numeric pair is unequal by variant.
        // (This is what lets `Assert.AreEqual` work on Duration/Guid/Char/Option,
        // which the previous explicit arm list silently treated as never-equal.)
        _ => a == b,
    }
}

fn value_ordering(a: &Value, b: &Value) -> Result<std::cmp::Ordering, ErrorInfo> {
    use Value::*;
    if let (Some(left), Some(right)) = (classify_numeric(a), classify_numeric(b)) {
        return Ok(match (left, right) {
            (Num::Int { val: left, .. }, Num::Int { val: right, .. }) => left.cmp(&right),
            (left, right) => left.to_decimal().cmp(&right.to_decimal()),
        });
    }
    Ok(match (a, b) {
        (Boolean(left), Boolean(right)) => left.cmp(right),
        (Text(x), Text(y)) => x.cmp(y),
        // `Code` is caseless in BC, so relational operators must compare it
        // case-insensitively too — matching `values_equal` and the record
        // filter. A Text/Code mix coerces to caseless Code.
        (Code(x), Code(y)) | (Text(x), Code(y)) | (Code(x), Text(y)) => {
            x.to_ascii_uppercase().cmp(&y.to_ascii_uppercase())
        }
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x.cmp(y),
        (l, r) => {
            return Err(simple_error(&format!(
                "cannot compare {} and {}",
                l.type_name(),
                r.type_name()
            )));
        }
    })
}

fn values_cmp(a: &Value, b: &Value, predicate: impl Fn(std::cmp::Ordering) -> bool) -> Eval {
    match value_ordering(a, b) {
        Ok(ordering) => Eval::Normal(Value::Boolean(predicate(ordering))),
        Err(error) => Eval::Error(error),
    }
}

/// Inclusive range membership shared by the `in` operator and CASE labels.
pub(crate) fn value_in_range(value: &Value, start: &Value, end: &Value) -> Result<bool, ErrorInfo> {
    Ok(value_ordering(value, start)?.is_ge() && value_ordering(value, end)?.is_le())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::scope::CallFrame;
    use crate::test_support::MockSource;
    use rust_decimal_macros::dec;
    use std::sync::Arc;

    fn test_ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(MockSource::new()))
    }

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) | Eval::Exit(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    fn err(eval: Eval) -> ErrorInfo {
        match eval {
            Eval::Error(e) => e,
            Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
            Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
            Eval::Break | Eval::Continue => panic!("expected error, got break/continue"),
        }
    }

    #[test]
    fn deep_expression_nesting_errors_instead_of_overflowing() {
        let depth = 400; // beyond the cap, far below crash territory
        let expr = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        let source = format!(
            "codeunit 50100 X\n{{\n    procedure P()\n    var\n        I: Integer;\n    begin\n        I := {expr};\n    end;\n}}\n"
        );
        let parsed = al_syntax::AlParser::parse_quick(&source);
        let mut nodes = vec![parsed.tree.root_node()];
        let mut target = None;
        while let Some(n) = nodes.pop() {
            if n.kind() == "parenthesized_expression" {
                target = Some(n);
                break;
            }
            let mut c = n.walk();
            nodes.extend(n.children(&mut c));
        }
        let node = target.expect("parenthesized expression must parse");
        let mut scope = ScopeStack::new();
        let mut ctx = test_ctx();
        match eval_expr(node, source.as_bytes(), &mut scope, &mut ctx) {
            Eval::Error(e) => assert!(
                e.message.to_lowercase().contains("depth"),
                "error must mention the depth cap: {}",
                e.message
            ),
            Eval::Normal(_) => panic!("expected a depth-cap error, got Normal"),
            Eval::Exit(_) => panic!("expected a depth-cap error, got Exit"),
            Eval::Break | Eval::Continue => {
                panic!("expected a depth-cap error, got break/continue")
            }
        }
    }

    #[test]
    fn workspace_enum_member_preserves_declared_ordinal() {
        let source = Arc::new(MockSource::new());
        source.file_index.add_file(
            std::path::PathBuf::from("/tmp/RunState.Enum.al"),
            r#"enum 50100 "Run State"
{
    value(0; Unknown) { }
    value(17; Running) { }
}"#
            .to_string(),
        );
        let ctx = DispatchCtx::new_pure(source);
        assert_eq!(
            resolve_workspace_enum_ordinal(&ctx, "Run State", "Running"),
            Some(17)
        );
    }

    #[test]
    fn integer_arithmetic_associativity() {
        let lhs = ok(apply_binary(
            "+",
            ok(apply_binary("+", Value::Integer(1), Value::Integer(2))),
            Value::Integer(3),
        ));
        let rhs = ok(apply_binary(
            "+",
            Value::Integer(1),
            ok(apply_binary("+", Value::Integer(2), Value::Integer(3))),
        ));
        assert_eq!(lhs, rhs);
    }

    #[test]
    fn integer_div_truncates() {
        assert_eq!(
            ok(apply_binary("div", Value::Integer(7), Value::Integer(2))),
            Value::Integer(3)
        );
    }

    #[test]
    fn option_and_char_follow_al_numeric_conversion_rules() {
        let option = Value::Option {
            type_name: "State".into(),
            member: "Ready".into(),
            ordinal: 2,
        };
        assert_eq!(
            ok(apply_binary("+", option.clone(), Value::Integer(3))),
            Value::Integer(5)
        );
        assert_eq!(
            ok(apply_binary("*", Value::Char('\u{0002}'), option.clone())),
            Value::Integer(4)
        );
        assert_eq!(
            ok(apply_binary("=", option, Value::Decimal(dec!(2.0)))),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary("<", Value::Char('A'), Value::Integer(66))),
            Value::Boolean(true)
        );
    }

    #[test]
    fn date_and_time_arithmetic_preserves_temporal_types() {
        assert_eq!(
            ok(apply_binary("+", Value::Date(100), Value::Integer(2))),
            Value::Date(102)
        );
        assert_eq!(
            ok(apply_binary("-", Value::Date(102), Value::Date(100))),
            Value::Integer(2)
        );
        assert_eq!(
            ok(apply_binary("+", Value::Time(1_000), Value::Integer(250))),
            Value::Time(1_250)
        );
        assert_eq!(
            ok(apply_binary("-", Value::Time(2_000), Value::Time(1_250))),
            Value::Integer(750)
        );
        assert!(err(apply_binary("+", Value::Date(0), Value::Integer(1)))
            .message
            .contains("0D"));
        assert!(err(apply_binary(
            "+",
            Value::Time(value::MS_PER_DAY - 1),
            Value::Integer(1)
        ))
        .message
        .contains("overflow"));
    }

    #[test]
    fn mixed_code_text_concatenation_preserves_operand_order() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Code("LEFT".into()),
                Value::Text("right".into())
            )),
            Value::Text("LEFTright".into())
        );
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Text("left".into()),
                Value::Code("RIGHT".into())
            )),
            Value::Text("leftRIGHT".into())
        );
    }

    #[test]
    fn slash_promotes_to_decimal() {
        assert_eq!(
            ok(apply_binary("/", Value::Integer(7), Value::Integer(2))),
            Value::Decimal(dec!(3.5))
        );
    }

    #[test]
    fn divide_by_zero_is_error() {
        let e = err(apply_binary("/", Value::Integer(1), Value::Integer(0)));
        assert!(e.message.contains("division by zero"));
        let e = err(apply_binary("mod", Value::Integer(1), Value::Integer(0)));
        assert!(e.message.contains("modulo by zero"));
    }

    #[test]
    fn integer_arithmetic_traps_i32_overflow() {
        let e = err(apply_binary(
            "*",
            Value::Integer(2_147_483_647),
            Value::Integer(3),
        ));
        assert!(e.message.contains("overflow"), "got {}", e.message);
        let e = err(apply_binary(
            "+",
            Value::Integer(i32::MAX as i64),
            Value::Integer(1),
        ));
        assert!(e.message.contains("overflow"));
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Integer(2_000_000_000),
                Value::Integer(100),
            )),
            Value::Integer(2_000_000_100)
        );
    }

    #[test]
    fn repeating_division_yields_value_not_error() {
        let r = ok(apply_binary(
            "/",
            Value::Decimal(dec!(10)),
            Value::Decimal(dec!(3)),
        ));
        match r {
            Value::Decimal(d) => {
                assert!(d > dec!(3.333) && d < dec!(3.334), "got {d}");
            }
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn decimal_overflow_on_add_and_mul_errors() {
        assert!(err(apply_binary(
            "*",
            Value::Decimal(Decimal::MAX),
            Value::Decimal(Decimal::MAX),
        ))
        .message
        .contains("overflow"));
        assert!(err(apply_binary(
            "+",
            Value::Decimal(Decimal::MAX),
            Value::Decimal(Decimal::MAX),
        ))
        .message
        .contains("overflow"));
    }

    #[test]
    fn large_integer_decimal_equality_is_exact() {
        let big = i64::MAX;
        let as_dec = Decimal::from(big);
        assert_eq!(
            ok(apply_binary(
                "=",
                Value::Integer(big),
                Value::Decimal(as_dec)
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "=",
                Value::Integer(big),
                Value::Decimal(Decimal::from(big - 1))
            )),
            Value::Boolean(false),
            "i64::MAX must not equal i64::MAX-1 (would collide under as-f64)"
        );
    }

    #[test]
    fn biginteger_arithmetic_uses_i64_width() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::BigInteger(5_000_000_000),
                Value::BigInteger(1)
            )),
            Value::BigInteger(5_000_000_001)
        );
        assert!(matches!(
            ok(apply_binary(
                "*",
                Value::BigInteger(3_000_000_000),
                Value::BigInteger(2)
            )),
            Value::BigInteger(6_000_000_000)
        ));
    }

    #[test]
    fn integer_arithmetic_still_traps_at_i32() {
        assert!(err(apply_binary(
            "*",
            Value::Integer(2_000_000_000),
            Value::Integer(2)
        ))
        .message
        .contains("overflow"));
    }

    #[test]
    fn mixed_integer_biginteger_promotes_to_biginteger() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Integer(2_000_000_000),
                Value::BigInteger(2_000_000_000)
            )),
            Value::BigInteger(4_000_000_000)
        );
    }

    #[test]
    fn biginteger_overflow_at_i64_traps() {
        assert!(err(apply_binary(
            "*",
            Value::BigInteger(i64::MAX),
            Value::BigInteger(2)
        ))
        .message
        .contains("overflow"));
    }

    #[test]
    fn integer_and_biginteger_compare_equal_by_value() {
        assert_eq!(
            ok(apply_binary("=", Value::Integer(5), Value::BigInteger(5))),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary("<", Value::Integer(5), Value::BigInteger(6))),
            Value::Boolean(true)
        );
        assert!(values_equal(&Value::BigInteger(5), &Value::Integer(5)));
    }

    #[test]
    fn biginteger_divided_by_integer_promotes_to_decimal() {
        assert_eq!(
            ok(apply_binary(
                "/",
                Value::BigInteger(5_000_000_000),
                Value::Integer(2)
            )),
            Value::Decimal(dec!(2500000000))
        );
    }

    #[test]
    fn code_relational_comparison_is_caseless() {
        assert_eq!(
            ok(apply_binary(
                "<=",
                Value::Code("abc".into()),
                Value::Code("ABC".into())
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "<",
                Value::Code("abc".into()),
                Value::Code("ABD".into())
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "=",
                Value::Code("abc".into()),
                Value::Code("ABC".into())
            )),
            Value::Boolean(true)
        );
    }

    #[test]
    fn values_equal_covers_non_numeric_variants() {
        assert!(values_equal(&Value::Duration(1000), &Value::Duration(1000)));
        assert!(!values_equal(
            &Value::Duration(1000),
            &Value::Duration(2000)
        ));
        assert!(values_equal(
            &Value::Guid("abc".into()),
            &Value::Guid("abc".into())
        ));
        assert!(values_equal(&Value::Char('x'), &Value::Char('x')));
        assert!(!values_equal(&Value::Char('x'), &Value::Char('y')));
        assert!(values_equal(&Value::BigInteger(5), &Value::Integer(5)));
        assert!(!values_equal(&Value::Integer(5), &Value::Text("5".into())));
    }

    #[test]
    fn mixed_integer_decimal_division_promotes() {
        assert_eq!(
            ok(apply_binary(
                "/",
                Value::Decimal(dec!(7.0)),
                Value::Integer(2)
            )),
            Value::Decimal(dec!(3.5))
        );
        assert_eq!(
            ok(apply_binary(
                "/",
                Value::Integer(7),
                Value::Decimal(dec!(2.0))
            )),
            Value::Decimal(dec!(3.5))
        );
    }

    #[test]
    fn mixed_integer_decimal_division_by_zero_is_error() {
        let e = err(apply_binary(
            "/",
            Value::Decimal(dec!(1.0)),
            Value::Integer(0),
        ));
        assert!(e.message.contains("division by zero"));
        let e = err(apply_binary(
            "/",
            Value::Integer(1),
            Value::Decimal(dec!(0.0)),
        ));
        assert!(e.message.contains("division by zero"));
    }

    #[test]
    fn all_four_operators_handle_mixed_types() {
        for op in ["+", "-", "*", "/"] {
            assert!(
                matches!(
                    apply_binary(op, Value::Integer(6), Value::Decimal(dec!(2.0))),
                    Eval::Normal(Value::Decimal(_))
                ),
                "op {op} Int,Dec should promote to Decimal"
            );
            assert!(
                matches!(
                    apply_binary(op, Value::Decimal(dec!(6.0)), Value::Integer(2)),
                    Eval::Normal(Value::Decimal(_))
                ),
                "op {op} Dec,Int should promote to Decimal"
            );
        }
    }

    #[test]
    fn string_concat_text_and_code() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Text("hello, ".into()),
                Value::Text("world".into())
            )),
            Value::Text("hello, world".into())
        );
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Text("a".into()),
                Value::Code("B".into())
            )),
            Value::Text("aB".into())
        );
    }

    #[test]
    fn equality_and_inequality() {
        assert_eq!(
            ok(apply_binary("=", Value::Integer(5), Value::Integer(5))),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary("<>", Value::Integer(5), Value::Integer(6))),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "=",
                Value::Integer(5),
                Value::Decimal(dec!(5.0))
            )),
            Value::Boolean(true)
        );
    }

    #[test]
    fn ordering_for_text_lex() {
        assert_eq!(
            ok(apply_binary(
                "<",
                Value::Text("apple".into()),
                Value::Text("banana".into())
            )),
            Value::Boolean(true)
        );
    }

    #[test]
    fn boolean_logic_works() {
        assert_eq!(
            ok(apply_binary(
                "and",
                Value::Boolean(true),
                Value::Boolean(false)
            )),
            Value::Boolean(false)
        );
        assert_eq!(
            ok(apply_binary(
                "or",
                Value::Boolean(false),
                Value::Boolean(true)
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "xor",
                Value::Boolean(true),
                Value::Boolean(true)
            )),
            Value::Boolean(false)
        );
    }

    #[test]
    fn unsupported_operator_yields_error() {
        let e = err(apply_binary("**", Value::Integer(2), Value::Integer(3)));
        assert!(
            e.message.contains("not supported"),
            "expected helpful error, got: {}",
            e.message
        );
    }

    #[test]
    fn comparing_incompatible_types_errors() {
        let e = err(apply_binary(
            "<",
            Value::Text("a".into()),
            Value::Integer(1),
        ));
        assert!(
            e.message.contains("cannot compare"),
            "expected compare error, got: {}",
            e.message
        );
    }

    fn parse_and_eval(source_str: &str) -> Eval {
        let wrapper = format!(
            "codeunit 50100 \"X\"\n{{\n    procedure Test(): Variant\n    begin\n        exit({source_str});\n    end;\n}}"
        );
        let mut parser = al_syntax::parser::AlParser::new();
        let result = parser.parse(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        if root.has_error() {
            return Eval::Error(simple_error("test harness wrapper contains a syntax error"));
        }

        fn find_expression<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "expression" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_expression(child) {
                    return Some(found);
                }
            }
            None
        }

        fn find_exit_arg<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "exit_statement" {
                let mut cursor = node.walk();
                return node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() == "argument_list")
                    .and_then(find_expression);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_exit_arg(child) {
                    return Some(found);
                }
            }
            None
        }

        let expr = match find_exit_arg(root) {
            Some(n) => n,
            None => {
                return Eval::Error(simple_error(
                    "test harness could not find the wrapped expression",
                ));
            }
        };

        let mut stack = ScopeStack::new();
        stack.push(CallFrame::new("X", "Test"));
        let mut ctx = test_ctx();
        eval_expr(expr, bytes, &mut stack, &mut ctx)
    }

    #[test]
    fn parsed_expression_uses_al_multiplicative_precedence() {
        assert_eq!(ok(parse_and_eval("2 + 3 * 4")), Value::Integer(14));
    }

    #[test]
    fn parsed_parentheses_override_al_precedence() {
        assert_eq!(ok(parse_and_eval("(2 + 3) * 4")), Value::Integer(20));
    }

    #[test]
    fn parsed_boolean_expression_uses_al_logical_precedence() {
        assert_eq!(
            ok(parse_and_eval("true or false and false")),
            Value::Boolean(true)
        );
    }

    #[test]
    fn parsed_equal_precedence_operators_are_left_associative() {
        assert_eq!(ok(parse_and_eval("20 div 5 * 2")), Value::Integer(8));
        assert_eq!(ok(parse_and_eval("20 - 5 - 2")), Value::Integer(13));
    }

    #[test]
    fn parsed_parenthesized_comparisons_compose_logically() {
        assert_eq!(
            ok(parse_and_eval("(1 < 2) and (3 < 4)")),
            Value::Boolean(true)
        );
    }

    #[test]
    fn parsed_conditional_expression_uses_low_precedence_and_one_branch() {
        assert_eq!(
            ok(parse_and_eval("1 + 2 = 3 ? 10 : 20")),
            Value::Integer(10)
        );
        assert_eq!(
            ok(parse_and_eval("1 + 2 = 4 ? 10 : 20")),
            Value::Integer(20)
        );

        // An unselected arm must not be evaluated: in real AL it may contain
        // a call with side effects or a runtime failure.
        assert_eq!(
            ok(parse_and_eval("true ? 7 : UnboundIdentifier")),
            Value::Integer(7)
        );
        assert_eq!(
            ok(parse_and_eval("false ? UnboundIdentifier : 8")),
            Value::Integer(8)
        );
    }

    #[test]
    fn parsed_conditional_expression_is_right_associative() {
        assert_eq!(
            ok(parse_and_eval("false ? 1 : true ? 2 : 3")),
            Value::Integer(2)
        );
        assert_eq!(
            ok(parse_and_eval("true ? false ? 1 : 2 : 3")),
            Value::Integer(2)
        );
    }

    #[test]
    fn parsed_conditional_expression_requires_boolean_condition() {
        let error = err(parse_and_eval("1 ? 2 : 3"));
        assert!(error.message.contains("requires Boolean condition"));
    }

    #[test]
    fn range_and_in_membership_are_inclusive() {
        let range = ok(apply_binary("..", Value::Integer(10), Value::Integer(20)));
        assert_eq!(
            ok(apply_binary(
                "in",
                Value::Integer(10),
                Value::List(vec![range.clone()])
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "in",
                Value::Integer(20),
                Value::List(vec![range.clone()])
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "in",
                Value::Integer(21),
                Value::List(vec![range])
            )),
            Value::Boolean(false)
        );
    }

    #[test]
    fn in_membership_supports_discrete_values_and_exact_numeric_coercion() {
        assert_eq!(
            ok(apply_binary(
                "in",
                Value::Integer(2),
                Value::List(vec![
                    Value::Integer(1),
                    Value::Decimal(dec!(2.0)),
                    Value::Integer(3),
                ])
            )),
            Value::Boolean(true)
        );
    }

    #[test]
    fn parsed_in_set_supports_values_ranges_and_nested_expressions() {
        assert_eq!(ok(parse_and_eval("2 in [1, 2, 3]")), Value::Boolean(true));
        assert_eq!(
            ok(parse_and_eval("2 in [0, (1 + 1), 4 .. 8]")),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(parse_and_eval("5 in [1, 4 .. 8, 10]")),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(parse_and_eval("9 in [1, 4 .. 8, 10]")),
            Value::Boolean(false)
        );
    }

    #[test]
    fn parsed_in_set_handles_quoted_commas_and_empty_sets() {
        assert_eq!(
            ok(parse_and_eval("'a,b' in ['x', 'a,b']")),
            Value::Boolean(true)
        );
        assert_eq!(ok(parse_and_eval("1 in []")), Value::Boolean(false));
        assert_eq!(
            ok(parse_and_eval(
                "2 in [1, // a comma in a comment is not a member: ,\n 2, 3]"
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(parse_and_eval("2 in [1, /* ignored, comma */ 2, 3]")),
            Value::Boolean(true)
        );
    }

    #[test]
    fn eval_integer_literal_via_parser() {
        if let Eval::Normal(v) = parse_and_eval("42") {
            assert_eq!(v, Value::Integer(42));
        }
    }

    #[test]
    fn impossible_calendar_date_is_rejected() {
        let result = parse_and_eval("20230229D");
        assert!(
            result.is_error(),
            "a non-leap-year February 29 must not be normalized into another date: {result:?}"
        );
    }

    #[test]
    fn overprecise_time_literal_is_rejected() {
        let result = parse_and_eval("1234561234T");
        assert!(
            result.is_error(),
            "time precision beyond milliseconds must not be silently truncated: {result:?}"
        );
    }

    #[test]
    fn eval_unbound_identifier_is_error() {
        let result = parse_and_eval("nope");
        match result {
            Eval::Error(e) => {
                assert!(e.message.contains("unbound") || e.message.contains("unsupported"));
            }
            Eval::Normal(_) | Eval::Exit(_) => {}
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    #[test]
    fn integer_add_overflow_returns_error() {
        let result = apply_binary("+", Value::Integer(i64::MAX), Value::Integer(1));
        assert!(
            result.is_error(),
            "Integer overflow must produce Eval::Error, not panic or wrap silently"
        );
    }

    #[test]
    fn integer_min_div_neg1_returns_error() {
        let result = apply_binary("div", Value::Integer(i64::MIN), Value::Integer(-1));
        assert!(
            result.is_error(),
            "i64::MIN div -1 must produce Eval::Error, not panic"
        );
    }

    #[test]
    fn decimal_arithmetic_is_exact_and_overflow_errors() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Decimal(dec!(0.1)),
                Value::Decimal(dec!(0.2))
            )),
            Value::Decimal(dec!(0.3)),
            "0.1 + 0.2 must equal 0.3 exactly"
        );
        let result = apply_binary(
            "*",
            Value::Decimal(Decimal::MAX),
            Value::Decimal(Decimal::MAX),
        );
        assert!(
            result.is_error(),
            "decimal overflow must produce Eval::Error, got: {result:?}"
        );
    }
}
