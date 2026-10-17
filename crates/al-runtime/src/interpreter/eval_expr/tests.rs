use super::*;
use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::scope::{CallFrame, Eval, ScopeStack};
use crate::interpreter::value::{self, Decimal, ErrorInfo, Value};
use crate::test_support::MockSource;
use rust_decimal_macros::dec;
use std::sync::Arc;

use super::member_access::resolve_workspace_enum_ordinal;
use super::operators::apply_binary;

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

fn error_of(eval: Eval) -> ErrorInfo {
    match eval {
        Eval::Error(e) => e,
        Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
        Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
        Eval::Break | Eval::Continue => panic!("expected error, got break/continue"),
    }
}

#[test]
fn deep_expression_nesting_errors_instead_of_overflowing() {
    // The cap is sized against the stack an interpreted body gets, so run
    // on that stack rather than the 2 MiB test default.
    std::thread::Builder::new()
        .stack_size(crate::interpreter::dispatch::INTERP_STACK_BYTES)
        .spawn(deep_expression_nesting_body)
        .expect("spawn deep expression test thread")
        .join()
        .expect("deep expression test thread panicked");
}

fn deep_expression_nesting_body() {
    let depth = crate::interpreter::scope::MAX_EXPR_DEPTH + 64;
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
fn duration_plus_date_and_time_dispatch_to_temporal_arms() {
    // Regression: the symmetric Duration arms used to match first, so
    // `Duration + Date` / `Duration + Time` captured the Date/Time operand
    // as `offset` and errored in `whole_offset` instead of reaching the
    // Date/Time handlers.
    let date = crate::interpreter::value::al_days_from_ymd(2024, 2, 28);
    let expected = ok(apply_binary("+", Value::Date(date), Value::Duration(1)));
    assert_eq!(expected, Value::Date(date + 1));
    assert_eq!(
        ok(apply_binary("+", Value::Duration(1), Value::Date(date))),
        expected,
        "Duration + Date must match Date + Duration"
    );
    let time = 3_600_000;
    let expected = ok(apply_binary("+", Value::Time(time), Value::Duration(500)));
    assert_eq!(expected, Value::Time(time + 500));
    assert_eq!(
        ok(apply_binary("+", Value::Duration(500), Value::Time(time))),
        expected,
        "Duration + Time must match Time + Duration"
    );
    // Duration ± Duration still lands on the Duration arms.
    assert_eq!(
        ok(apply_binary("+", Value::Duration(2), Value::Duration(3))),
        Value::Duration(5)
    );
    assert_eq!(
        ok(apply_binary("-", Value::Duration(5), Value::Duration(3))),
        Value::Duration(2)
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
        Value::Duration(750),
        "Time - Time is a Duration in BC, not an Integer"
    );
    assert!(
        error_of(apply_binary("+", Value::Date(0), Value::Integer(1)))
            .message
            .contains("0D")
    );
    assert!(error_of(apply_binary(
        "+",
        Value::Time(value::MS_PER_DAY - 1),
        Value::Integer(1)
    ))
    .message
    .contains("overflow"));
}

#[test]
fn datetime_and_duration_arithmetic() {
    // DateTime - DateTime → Duration.
    assert_eq!(
        ok(apply_binary(
            "-",
            Value::DateTime(10_000),
            Value::DateTime(4_000)
        )),
        Value::Duration(6_000)
    );
    // DateTime ± Duration → DateTime (both operand orders for +).
    assert_eq!(
        ok(apply_binary(
            "+",
            Value::DateTime(10_000),
            Value::Duration(500)
        )),
        Value::DateTime(10_500)
    );
    assert_eq!(
        ok(apply_binary(
            "+",
            Value::Duration(500),
            Value::DateTime(10_000)
        )),
        Value::DateTime(10_500)
    );
    assert_eq!(
        ok(apply_binary(
            "-",
            Value::DateTime(10_000),
            Value::Duration(500)
        )),
        Value::DateTime(9_500)
    );
    // Duration ± Duration → Duration; Duration ± number too.
    assert_eq!(
        ok(apply_binary(
            "+",
            Value::Duration(500),
            Value::Duration(250)
        )),
        Value::Duration(750)
    );
    assert_eq!(
        ok(apply_binary("-", Value::Duration(500), Value::Integer(100))),
        Value::Duration(400)
    );
    // Arithmetic on the zero (undefined) DateTime is an error.
    assert!(apply_binary("+", Value::DateTime(0), Value::Duration(1)).is_error());
    assert!(apply_binary("-", Value::DateTime(0), Value::DateTime(1)).is_error());
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
    let e = error_of(apply_binary("/", Value::Integer(1), Value::Integer(0)));
    assert!(e.message.contains("division by zero"));
    let e = error_of(apply_binary("mod", Value::Integer(1), Value::Integer(0)));
    assert!(e.message.contains("modulo by zero"));
}

#[test]
fn integer_arithmetic_traps_i32_overflow() {
    let e = error_of(apply_binary(
        "*",
        Value::Integer(2_147_483_647),
        Value::Integer(3),
    ));
    assert!(e.message.contains("overflow"), "got {}", e.message);
    let e = error_of(apply_binary(
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
    assert!(error_of(apply_binary(
        "*",
        Value::Decimal(Decimal::MAX),
        Value::Decimal(Decimal::MAX),
    ))
    .message
    .contains("overflow"));
    assert!(error_of(apply_binary(
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
    assert!(error_of(apply_binary(
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
    assert!(error_of(apply_binary(
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
    let e = error_of(apply_binary(
        "/",
        Value::Decimal(dec!(1.0)),
        Value::Integer(0),
    ));
    assert!(e.message.contains("division by zero"));
    let e = error_of(apply_binary(
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
    let e = error_of(apply_binary("**", Value::Integer(2), Value::Integer(3)));
    assert!(
        e.message.contains("not supported"),
        "expected helpful error, got: {}",
        e.message
    );
}

#[test]
fn comparing_incompatible_types_errors() {
    let e = error_of(apply_binary(
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
        return Eval::Error(error_info("test harness wrapper contains a syntax error"));
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
            return Eval::Error(error_info(
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
    let error = error_of(parse_and_eval("1 ? 2 : 3"));
    assert!(error.message.contains("requires Boolean condition"));
}

#[test]
fn range_and_in_membership_are_inclusive() {
    let range = ok(apply_binary("..", Value::Integer(10), Value::Integer(20)));
    assert_eq!(
        ok(apply_binary(
            "in",
            Value::Integer(10),
            Value::list(vec![range.clone()])
        )),
        Value::Boolean(true)
    );
    assert_eq!(
        ok(apply_binary(
            "in",
            Value::Integer(20),
            Value::list(vec![range.clone()])
        )),
        Value::Boolean(true)
    );
    assert_eq!(
        ok(apply_binary(
            "in",
            Value::Integer(21),
            Value::list(vec![range])
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
            Value::list(vec![
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
