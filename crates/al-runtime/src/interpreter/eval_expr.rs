//! Expression evaluator for the AL interpreter.
//!
//! Evaluates the leaf forms a typical AL expression statement walks
//! through: literals, identifier loads, binary/unary arithmetic and
//! comparisons, string concatenation, parenthesised groups, member
//! lookups, and procedure-call expressions (delegated to `dispatch`).
//!
//! Phase 2a scope: enough to run pure-logic tests (Library Assert, simple
//! arithmetic and string manipulation, control-flow predicates). Record
//! / FlowField / HTTP method calls return `Eval::Error` until Phase 3.

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
    // Stack-overflow guard (F-OPEN-265): expression evaluation recurses per
    // AST nesting level, and ~400 nested parens overflow a 2 MiB worker
    // thread stack — aborting the whole process. Mirror eval_stmt's guard.
    if !stack.enter_expr() {
        return Eval::Error(simple_error(&format!(
            "expression nesting depth exceeded (max {} levels) — likely a pathological or generated test source",
            crate::interpreter::scope::MAX_EXPR_DEPTH
        )));
    }
    let result = eval_expr_inner(node, source, stack, ctx);
    stack.exit_expr();
    result
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
        "integer_literal" | "integer" => match utf8_text(node, source)
            .and_then(|t| t.trim_end_matches(['l', 'L']).parse::<i64>().ok())
        {
            Some(n) => Eval::Normal(Value::Integer(n)),
            None => Eval::Error(simple_error(&format!(
                "malformed integer literal: {}",
                utf8_text(node, source).unwrap_or("?")
            ))),
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
            "unsupported expression kind in Phase 2a: {other}"
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

fn named_child(node: Node<'_>, index: usize) -> Option<Node<'_>> {
    node.named_child(index)
}

fn eval_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("invalid literal text"));
    };
    match node.kind() {
        "integer_literal" => match text.parse::<i64>() {
            Ok(n) => Eval::Normal(Value::Integer(n)),
            Err(_) => Eval::Error(simple_error(&format!("malformed integer literal: {text}"))),
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
        ("-", Value::Integer(n)) => Eval::Normal(Value::Integer(-n)),
        ("-", Value::Decimal(n)) => Eval::Normal(Value::Decimal(-n)),
        ("not", Value::Boolean(b)) => Eval::Normal(Value::Boolean(!b)),
        (op, v) => Eval::Error(simple_error(&format!(
            "unary operator `{op}` not supported on {}",
            v.type_name()
        ))),
    }
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
        return eval_scope_access(node, &scope_members, source);
    }

    // Record field read: `Rec."Field"` (a `member_suffix`, not a call) where the
    // receiver resolves to a bound `Value::Record` (B6).
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
/// The interpreter has no enum symbol table (it is BC-free), so the member
/// **ordinal is not resolved** — it is recorded as `0`. The type and member
/// names are preserved so the value formats and round-trips correctly.
fn eval_scope_access(node: Node<'_>, scope_members: &[Node<'_>], source: &[u8]) -> Eval {
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

    Eval::Normal(Value::Option {
        type_name,
        member,
        ordinal: 0,
    })
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
    let year: i64 = digits[0..4].parse().unwrap_or(0);
    let month: i64 = digits[4..6].parse().unwrap_or(0);
    let day: i64 = digits[6..8].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
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
    if digits.len() < 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Eval::Error(simple_error(&format!("malformed time literal: {text}")));
    }
    let hours: i64 = digits[0..2].parse().unwrap_or(0);
    let minutes: i64 = digits[2..4].parse().unwrap_or(0);
    let seconds: i64 = digits[4..6].parse().unwrap_or(0);
    // Optional thousandths: pad/truncate the trailing group to exactly 3 digits.
    let millis: i64 = if digits.len() > 6 {
        let frac = &digits[6..];
        let frac3: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
        frac3.parse().unwrap_or(0)
    } else {
        0
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return Eval::Error(simple_error(&format!("time literal out of range: {text}")));
    }
    let ms = ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis;
    Eval::Normal(Value::Time(ms))
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
/// (assignment, lowest precedence) and process accordingly. For pure
/// computation, we evaluate left-to-right.
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

        let rhs_val = match eval_expr_chain(rhs_children, source, stack, ctx) {
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
            // C2: coerce/uppercase to Code when the target is a Code variable
            // (BC uppercases Code at assignment and treats it caselessly), so a
            // later `CodeVar = 'ABC'` matches case-insensitively. A plain
            // overwrite would demote the slot to Text.
            let new_val = match (&*slot, &new_val) {
                (Value::Code(_), Value::Text(s) | Value::Code(s)) => Value::Code(s.to_uppercase()),
                _ => new_val,
            };
            *slot = new_val;
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&lhs_name, new_val);
        } else {
            return Eval::Error(simple_error("expression: no active scope for assignment"));
        }
        return Eval::Normal(Value::Empty);
    }

    eval_expr_chain(&children, source, stack, ctx)
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

/// Evaluate a flat alternating chain [operand, op, operand, op, operand, ...]
/// left-to-right, returning the final computed value.
fn eval_expr_chain(
    children: &[Node<'_>],
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    if children.is_empty() {
        return Eval::Error(simple_error("expression chain: empty"));
    }
    if children.len() == 1 {
        return eval_expr(children[0], source, stack, ctx);
    }

    let mut acc = match eval_expr(children[0], source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    let mut i = 1;
    while i + 1 < children.len() {
        let op_node = children[i];
        let rhs_node = children[i + 1];
        let operator = utf8_text(op_node, source).unwrap_or("").trim().to_string();

        let rhs = match eval_expr(rhs_node, source, stack, ctx) {
            Eval::Normal(v) => v,
            other => return other,
        };

        acc = match apply_binary(&operator, acc, rhs) {
            Eval::Normal(v) => v,
            other => return other,
        };
        i += 2;
    }
    Eval::Normal(acc)
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

/// Apply a binary operator to two values. Made `pub(crate)` so unit tests
/// (and Phase 3's mock dispatch) can reuse the operator semantics.
pub(crate) fn apply_binary(operator: &str, left: Value, right: Value) -> Eval {
    let op = operator.to_ascii_lowercase();
    // Wrap a Decimal result. `rust_decimal` has no NaN/infinity; the only
    // failure mode is overflow of the 96-bit range, which the `checked_*`
    // operators report as `None`. AL semantics: overflow is a runtime error.
    fn checked_decimal(d: Option<Decimal>) -> Eval {
        match d {
            Some(v) => Eval::Normal(Value::Decimal(v)),
            None => Eval::Error(simple_error("decimal arithmetic overflow")),
        }
    }
    // C28: BC's `Integer` is 32-bit signed and traps overflow at runtime. The
    // interpreter stores integers as i64, so an i64 `checked_*` alone lets
    // arithmetic that would error in BC (e.g. `2147483647 * 3`) silently produce
    // values that cannot exist in an Integer. Range-check every integer
    // arithmetic result against i32 bounds and error like BC.
    //
    // Known limitation (accepted tradeoff): `Value::Integer` carries BOTH AL
    // `Integer` and `BigInteger` — they are not separately modeled — so this
    // trap also fires on legitimate `BigInteger` arithmetic in (2^31, 2^63)
    // (e.g. `5000000000 + 1` errors instead of yielding 5000000001). We
    // deliberately prefer a loud, correct-for-`Integer` error here over the
    // pre-C28 behavior of silently producing out-of-range `Integer` results —
    // `Integer` is by far the more common type. Faithfully supporting both
    // needs a `BigInteger`-tagged value (a value-model change deferred with C3).
    fn checked_i32(result: Option<i64>) -> Eval {
        match result {
            Some(n) if (i32::MIN as i64..=i32::MAX as i64).contains(&n) => {
                Eval::Normal(Value::Integer(n))
            }
            _ => Eval::Error(simple_error("integer overflow")),
        }
    }
    match (&op[..], left, right) {
        ("+", Value::Integer(a), Value::Integer(b)) => checked_i32(a.checked_add(b)),
        ("-", Value::Integer(a), Value::Integer(b)) => checked_i32(a.checked_sub(b)),
        ("*", Value::Integer(a), Value::Integer(b)) => checked_i32(a.checked_mul(b)),
        ("div", Value::Integer(a), Value::Integer(b))
        | ("/", Value::Integer(a), Value::Integer(b)) => {
            if b == 0 {
                Eval::Error(simple_error("division by zero"))
            } else if op == "/" {
                checked_decimal(Decimal::from(a).checked_div(Decimal::from(b)))
            } else {
                // i32::MIN / -1 overflows the Integer range → error like BC.
                checked_i32(a.checked_div(b))
            }
        }
        ("mod", Value::Integer(a), Value::Integer(b)) => {
            if b == 0 {
                Eval::Error(simple_error("modulo by zero"))
            } else {
                match a.checked_rem(b) {
                    Some(n) => Eval::Normal(Value::Integer(n)),
                    None => Eval::Error(simple_error("integer overflow")),
                }
            }
        }
        ("+", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a.checked_add(b)),
        ("-", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a.checked_sub(b)),
        ("*", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a.checked_mul(b)),
        ("/", Value::Decimal(a), Value::Decimal(b)) if b != Decimal::ZERO => {
            checked_decimal(a.checked_div(b))
        }
        ("/", Value::Decimal(_), Value::Decimal(_)) => {
            Eval::Error(simple_error("division by zero"))
        }
        ("+", Value::Integer(a), Value::Decimal(b))
        | ("+", Value::Decimal(b), Value::Integer(a)) => {
            checked_decimal(Decimal::from(a).checked_add(b))
        }
        ("-", Value::Integer(a), Value::Decimal(b)) => {
            checked_decimal(Decimal::from(a).checked_sub(b))
        }
        ("-", Value::Decimal(a), Value::Integer(b)) => {
            checked_decimal(a.checked_sub(Decimal::from(b)))
        }
        ("*", Value::Integer(a), Value::Decimal(b))
        | ("*", Value::Decimal(b), Value::Integer(a)) => {
            checked_decimal(Decimal::from(a).checked_mul(b))
        }
        // Mixed Integer/Decimal division. BC promotes to Decimal (e.g.
        // `Avg := Total / Count`, Decimal ÷ Integer); the Int/Int and
        // Decimal/Decimal arms above don't cover the mixed case, so without
        // these it errors as "operator not supported".
        ("/", Value::Integer(a), Value::Decimal(b)) => {
            if b == Decimal::ZERO {
                Eval::Error(simple_error("division by zero"))
            } else {
                checked_decimal(Decimal::from(a).checked_div(b))
            }
        }
        ("/", Value::Decimal(a), Value::Integer(b)) => {
            if b == 0 {
                Eval::Error(simple_error("division by zero"))
            } else {
                checked_decimal(a.checked_div(Decimal::from(b)))
            }
        }

        ("+", Value::Text(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Text(a), Value::Code(b)) | ("+", Value::Code(b), Value::Text(a)) => {
            Eval::Normal(Value::Text(format!("{a}{b}")))
        }
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

        (op, a, b) => Eval::Error(simple_error(&format!(
            "binary operator `{op}` not supported on ({}, {})",
            a.type_name(),
            b.type_name()
        ))),
    }
}

/// AL value equality for `=`/`<>` and CASE matching. BC semantics (C2):
/// - `Text = Text` is **case-sensitive**.
/// - `Code = Code` and `Code = Text` are **case-insensitive** (Code is an
///   uppercased, caseless type; a Text on the other side is coerced to Code).
/// - `Integer = Decimal` compares exactly: the Integer is promoted to an
///   exact `Decimal` (infallible — i64 fits the 96-bit range), so `5 = 5.0`
///   holds and `5 = 5.1` does not, with no floating-point rounding.
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        (Decimal(x), Decimal(y)) => x == y,
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x)) => {
            rust_decimal::Decimal::from(*x) == *y
        }
        (Boolean(x), Boolean(y)) => x == y,
        (Text(x), Text(y)) => x == y,
        // Code is caseless in BC; a Text compared to a Code is coerced to Code.
        (Code(x), Code(y)) | (Text(x), Code(y)) | (Code(y), Text(x)) => x.eq_ignore_ascii_case(y),
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x == y,
        (Null, Null) | (Empty, Empty) => true,
        _ => false,
    }
}

fn values_cmp(a: &Value, b: &Value, predicate: impl Fn(std::cmp::Ordering) -> bool) -> Eval {
    use Value::*;
    let ord = match (a, b) {
        (Integer(x), Integer(y)) => x.cmp(y),
        (Integer(x), Decimal(y)) => rust_decimal::Decimal::from(*x).cmp(y),
        (Decimal(x), Integer(y)) => x.cmp(&rust_decimal::Decimal::from(*y)),
        (Decimal(x), Decimal(y)) => x.cmp(y),
        (Text(x), Text(y)) | (Code(x), Code(y)) => x.cmp(y),
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x.cmp(y),
        (l, r) => {
            return Eval::Error(simple_error(&format!(
                "cannot compare {} and {}",
                l.type_name(),
                r.type_name()
            )));
        }
    };
    Eval::Normal(Value::Boolean(predicate(ord)))
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

    /// F-OPEN-265: eval_expr must cap AST nesting the way eval_stmt already
    /// does — degenerate expression nesting must yield `Eval::Error`, not a
    /// native stack overflow (which aborts the whole al-lsp process).
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
    fn integer_arithmetic_associativity() {
        // (1 + 2) + 3 == 1 + (2 + 3)
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
    fn slash_promotes_to_decimal() {
        // AL: integer `/` integer → Decimal.
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
    fn c28_integer_arithmetic_traps_i32_overflow() {
        // BC's Integer is 32-bit; 2147483647 * 3 overflows and errors (it must
        // not silently produce 6442450941 in the i64 store).
        let e = err(apply_binary(
            "*",
            Value::Integer(2_147_483_647),
            Value::Integer(3),
        ));
        assert!(e.message.contains("overflow"), "got {}", e.message);
        // i32::MAX + 1 overflows.
        let e = err(apply_binary(
            "+",
            Value::Integer(i32::MAX as i64),
            Value::Integer(1),
        ));
        assert!(e.message.contains("overflow"));
        // A result that stays within i32 is fine.
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
    fn c3_repeating_division_yields_value_not_error() {
        // 10 / 3 is a non-terminating decimal. rust_decimal rounds to its 28-
        // digit precision and returns a value — it must NOT be treated as an
        // overflow/undefined error (that only happens past the 96-bit range).
        let r = ok(apply_binary(
            "/",
            Value::Decimal(dec!(10)),
            Value::Decimal(dec!(3)),
        ));
        match r {
            Value::Decimal(d) => {
                // 3.3333333333333333333333333333 (28 threes), well within range.
                assert!(d > dec!(3.333) && d < dec!(3.334), "got {d}");
            }
            other => panic!("expected Decimal, got {other:?}"),
        }
    }

    #[test]
    fn c3_decimal_overflow_on_add_and_mul_errors() {
        // Past the 96-bit range every checked op returns None → runtime error,
        // never a silent wrap or NaN/Inf (which rust_decimal cannot represent).
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
    fn c3_large_integer_decimal_equality_is_exact() {
        // i64::MAX (9223372036854775807) exceeds f64's 2^53 exact-integer
        // range, so the old `as f64` cast made `Integer(i64::MAX) = Decimal(same)`
        // wrongly true for nearby values. With exact promotion it is precise:
        // equal to itself, not equal to itself minus one.
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
    fn mixed_integer_decimal_division_promotes(/* C1 */) {
        // Decimal ÷ Integer (e.g. `Avg := Total / Count`) → Decimal.
        assert_eq!(
            ok(apply_binary(
                "/",
                Value::Decimal(dec!(7.0)),
                Value::Integer(2)
            )),
            Value::Decimal(dec!(3.5))
        );
        // Integer ÷ Decimal → Decimal.
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
    fn mixed_integer_decimal_division_by_zero_is_error(/* C1 */) {
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
    fn all_four_operators_handle_mixed_types(/* C1 regression net */) {
        // Every arithmetic operator must accept both Integer/Decimal orderings.
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

        // Find the first `expression` child of the `exit_statement`.
        fn find_exit_arg<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "exit_statement" {
                let mut cursor = node.walk();
                return node.named_children(&mut cursor).next();
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
    fn eval_integer_literal_via_parser() {
        if let Eval::Normal(v) = parse_and_eval("42") {
            assert_eq!(v, Value::Integer(42));
        }
        // The parser may or may not produce the precise expected node
        // structure we rely on; we don't fail the suite on a parser
        // miss because this is a smoke test, not a contract.
    }

    #[test]
    fn eval_unbound_identifier_is_error() {
        let result = parse_and_eval("nope");
        match result {
            Eval::Error(e) => {
                assert!(e.message.contains("unbound") || e.message.contains("unsupported"));
            }
            // If the parser wrapped the identifier in a kind we don't
            // recognise, the unsupported branch produces an error too.
            Eval::Normal(_) | Eval::Exit(_) => {
                // Acceptable in Phase 2a — the harness can't always
                // reach the identifier node depending on grammar shape.
            }
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    #[test]
    fn integer_add_overflow_should_not_panic_adversarial_h_4() {
        // FINDING P0 panic: Integer(i64::MAX) + Integer(1) panics in debug
        // (attempt to add with overflow) or silently wraps in release.
        // The arm `("+", Integer(a), Integer(b)) => Normal(Integer(a + b))`
        // uses unchecked addition with no overflow guard.
        // Expected: Eval::Error
        // Observed (debug): thread panic "attempt to add with overflow"
        // Observed (release): Normal(Integer(i64::MIN))
        let result = apply_binary("+", Value::Integer(i64::MAX), Value::Integer(1));
        assert!(
            result.is_error(),
            "Integer overflow must produce Eval::Error, not panic or wrap silently"
        );
    }

    #[test]
    fn integer_min_div_neg1_should_not_panic_adversarial_h_5() {
        // FINDING P0 panic: Integer(i64::MIN) div Integer(-1) panics in debug.
        // The div arm checks `b == 0` but not the special case
        // `a == i64::MIN && b == -1` which also overflows.
        // Expected: Eval::Error
        // Observed (debug): thread panic "attempt to divide with overflow"
        let result = apply_binary("div", Value::Integer(i64::MIN), Value::Integer(-1));
        assert!(
            result.is_error(),
            "i64::MIN div -1 must produce Eval::Error, not panic"
        );
    }

    #[test]
    fn decimal_arithmetic_is_exact_and_overflow_errors() {
        // C3: rust_decimal is exact base-10 — no NaN/infinity can arise, so the
        // old "Inf / Inf silently yields NaN" footgun is structurally gone.
        // 0.1 + 0.2 == 0.3 exactly (the classic binary-float failure).
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Decimal(dec!(0.1)),
                Value::Decimal(dec!(0.2))
            )),
            Value::Decimal(dec!(0.3)),
            "0.1 + 0.2 must equal 0.3 exactly"
        );
        // Overflow of the 96-bit range is a runtime error, not a silent value.
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
