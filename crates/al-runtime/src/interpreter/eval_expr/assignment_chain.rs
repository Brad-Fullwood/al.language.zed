//! The flat AL `expression` node: assignment (`:=`, `+=`, `-=`, `*=`, `/=`)
//! on the one hand, and the operator-precedence walk over a plain
//! `[operand, op, operand, op, ...]` chain (including the `? :` conditional)
//! on the other.

use tree_sitter::Node;

use al_syntax::IdentifierText;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::indexing;
use crate::interpreter::records;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;

use super::entry::eval_expr;
use super::helpers::utf8_text;
use super::operators::apply_binary;

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
pub(super) fn eval_expression_node(
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
        return Eval::Error(error_info("expression: no children"));
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

        // `Slots[i] := value` / `Name[1] := 'x'`: one element of an array or
        // one character of a Text.
        if let Some((name, suffix)) = indexing::indexed_variable(lhs_node, source) {
            let combine = |current: Value, rhs: Value| match kind {
                AssignKind::Plain => Eval::Normal(rhs),
                AssignKind::Compound(base_op) => apply_binary(base_op, current, rhs),
            };
            return indexing::write_element(&name, suffix, source, rhs_val, &combine, stack, ctx);
        }

        // Record field assignment: `Rec."Field" := value` / `Rec.Amount += 5`.
        // Handled before the plain-identifier path so the whole record isn't
        // overwritten. Compound forms load the field's current value and apply
        // the base operator FIRST — `Rec.Amount += 5` stores `Amount + 5`,
        // not the raw RHS.
        if let Some((recv, field)) = records::record_field_access(lhs_node, source) {
            if matches!(stack.lookup(&recv), Some(Value::Record(_))) {
                let new_val = match kind {
                    AssignKind::Plain => rhs_val,
                    AssignKind::Compound(base_op) => {
                        let Some((table, handle)) = records::record_binding(&recv, stack, ctx)
                        else {
                            return Eval::Error(error_info(format!(
                                "record variable '{recv}' is not bound"
                            )));
                        };
                        let current = match records::field_get(&table, handle, &field, ctx) {
                            Eval::Normal(v) => v,
                            other => return other,
                        };
                        match apply_binary(base_op, current, rhs_val) {
                            Eval::Normal(v) => v,
                            other => return other,
                        }
                    }
                };
                return records::try_field_assign(lhs_node, source, &new_val, stack, ctx)
                    .unwrap_or_else(|| {
                        Eval::Error(error_info(format!(
                            "record field assignment failed for '{recv}.{field}'"
                        )))
                    });
            }
        }

        let lhs_name = extract_identifier_name(lhs_node, source)
            .or_else(|| {
                lhs_node
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.unquote_identifier().to_ascii_lowercase())
            })
            .unwrap_or_default();

        if lhs_name.is_empty() {
            return Eval::Error(error_info(
                "expression: cannot resolve LHS name for assignment",
            ));
        }

        // In table code a bare field name that is not a variable is a field
        // of the implicit record.
        if stack.lookup(&lhs_name).is_none() {
            let field_value = match kind {
                AssignKind::Plain => Some(rhs_val.clone()),
                AssignKind::Compound(base_op) => {
                    match records::implicit_field_get(&lhs_name, stack, ctx) {
                        Some(Eval::Normal(current)) => {
                            match apply_binary(base_op, current, rhs_val.clone()) {
                                Eval::Normal(value) => Some(value),
                                other => return other,
                            }
                        }
                        Some(other) => return other,
                        None => None,
                    }
                }
            };
            if let Some(value) = field_value {
                if let Some(result) = records::implicit_field_set(&lhs_name, &value, stack, ctx) {
                    return result;
                }
            }
        }

        // Compound assignment (`x += rhs`) is `x := x <op> rhs`: load the
        // current value, apply the base operator, then store. Behaviour is
        // intentionally identical to writing the expanded form by hand.
        let new_val = match kind {
            AssignKind::Plain => rhs_val,
            AssignKind::Compound(base_op) => {
                let Some(current) = stack.lookup(&lhs_name).cloned() else {
                    return Eval::Error(error_info(format!(
                        "compound assignment to unbound identifier: {lhs_name}"
                    )));
                };
                match apply_binary(base_op, current, rhs_val) {
                    Eval::Normal(v) => v,
                    other => return other,
                }
            }
        };

        let capacity = stack.declared_text_length(&lhs_name);
        if let Some(slot) = stack.lookup_mut(&lhs_name) {
            // Preserve the slot's declared type (Code caselessness / integer
            // width) rather than adopting the RHS's — see `coerce_into_slot`.
            match Value::coerce_into_slot(slot, new_val, capacity) {
                Ok(value) => *slot = value,
                Err(message) => return Eval::Error(error_info(&message)),
            }
        } else {
            // AL has no implicit declaration: a typo'd LHS must fail loudly
            // instead of silently creating a fresh variable.
            return Eval::Error(error_info(format!(
                "assignment to unbound identifier '{lhs_name}' — variables must be declared"
            )));
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
        return Eval::Error(error_info(
            "conditional expression: `?` has no matching `:`",
        ));
    };

    let condition_children = &children[..question_index];
    let true_children = &children[(question_index + 1)..colon_index];
    let false_children = &children[(colon_index + 1)..];
    if condition_children.is_empty() || true_children.is_empty() || false_children.is_empty() {
        return Eval::Error(error_info(
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
            return Eval::Error(error_info(format!(
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
        return Eval::Error(error_info("expression chain: empty"));
    }
    if children.len() == 1 {
        return eval_expr(children[0], source, stack, ctx);
    }

    if children.len().is_multiple_of(2) {
        return Eval::Error(error_info(
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
            return Eval::Error(error_info(format!(
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
                    return Eval::Error(error_info(
                        "expression chain: binary operator missing right operand",
                    ));
                };
                let Some(left) = values.pop() else {
                    return Eval::Error(error_info(
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
        _ => Eval::Error(error_info(
            "expression chain: invalid operand/operator structure",
        )),
    }
}

fn extract_identifier_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "name" => {
                return child
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.unquote_identifier().to_ascii_lowercase());
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
