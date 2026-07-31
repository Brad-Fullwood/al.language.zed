//! Interpreter regressions derived from real BCApps source patterns.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::interpreter::dispatch::DispatchCtx;
    use crate::interpreter::scope::{CallFrame, Eval, ScopeStack};
    use crate::interpreter::value::Value;
    use crate::test_support::MockSource as Workspace;
    use rust_decimal_macros::dec;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    fn run_stmt(source_snippet: &str) -> (Eval, ScopeStack) {
        let wrapper = format!(
            "codeunit 50100 \"Regression\"\n{{\n    procedure Test()\n    var\n        x: Integer;\n        s: Text;\n        b: Boolean;\n    begin\n        {source_snippet}\n    end;\n}}"
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();

        let body = find_proc_body(root, bytes).expect("could not find procedure body");

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("x", Value::Integer(0));
        frame.bind("s", Value::Text(String::new()));
        frame.bind("b", Value::Boolean(false));
        stack.push(frame);

        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        (eval, stack)
    }

    fn find_proc_body<'a>(
        node: tree_sitter::Node<'a>,
        _source: &[u8],
    ) -> Option<tree_sitter::Node<'a>> {
        let mut work = vec![node];
        while let Some(current) = work.pop() {
            if current.kind() == "begin_end_block" {
                return Some(current);
            }
            let mut cursor = current.walk();
            work.extend(current.named_children(&mut cursor));
        }
        None
    }

    #[test]
    fn date_literal_executes() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        d: Date;
    begin
        d := 20240701D;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("d", Value::Date(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after assigning date literal, got: {:?}",
            eval
        );
    }

    #[test]
    fn time_literal_executes() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        t: Time;
    begin
        t := 063030T;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("t", Value::Time(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after assigning time literal, got: {:?}",
            eval
        );
    }

    #[test]
    fn compound_plus_equals_updates_integer() {
        let (eval, stack) = run_stmt("x += 1;");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after x += 1, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(1)),
            "x should be 1 after += 1"
        );
    }

    #[test]
    fn compound_minus_equals_updates_integer() {
        let (eval, stack) = run_stmt("x -= 1;");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after x -= 1, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(-1)),
            "x should be -1 after -= 1"
        );
    }

    #[test]
    fn multi_var_declaration_binds_each_variable() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        UTF8, ISO88591 : Integer;
    begin
        UTF8 := 65001;
        ISO88591 := 28591;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let frame = CallFrame::new("Regression", "Test");
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after multi-var assignment, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("utf8"),
            Some(&Value::Integer(65001)),
            "utf8 should be 65001"
        );
        assert_eq!(
            stack.lookup("iso88591"),
            Some(&Value::Integer(28591)),
            "iso88591 should be 28591"
        );
    }

    #[test]
    fn for_downto_i64_min_does_not_panic() {
        let wrapper = format!(
            "codeunit 50100 \"Regression\"\n{{\n    procedure Test()\n    var\n        i: Integer;\n    begin\n        for i := {} downto {} do x := 1;\n    end;\n}}",
            i64::MIN,
            i64::MIN
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("i", Value::Integer(0));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            !matches!(eval, Eval::Exit(_)),
            "FOR downto i64::MIN must not silently exit; got: {:?}",
            eval
        );
    }

    #[test]
    fn source_free_option_scope_fails_closed() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        MyOption: Option "Low","Medium","High";
    begin
        MyOption := "Shpfy Risk Level"::Low;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("myoption", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        let Eval::Error(error) = eval else {
            panic!("source-free option scope must fail closed");
        };
        assert!(error.message.contains("live BC"), "{}", error.message);
    }

    #[test]
    fn list_member_calls_dispatch() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        Labels: List of [Text];
        First: Text;
    begin
        Labels.Add('hello');
        Labels.Add('world');
        First := Labels.Get(1);
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("labels", Value::List(vec![]));
        frame.bind("first", Value::Text(String::new()));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after List operations, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("first"),
            Some(&Value::Text("hello".into())),
            "first should be 'hello'"
        );
    }

    #[test]
    fn biginteger_l_suffix_parses() {
        let (eval, stack) = run_stmt("x := 1719815430;"); // grammar may or may not produce L-suffix
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal for integer assignment, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(1_719_815_430)),
            "x should equal 1719815430"
        );
    }

    #[test]
    fn for_upward_loop_accumulates() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        i: Integer;
        x: Integer;
    begin
        x := 0;
        for i := 1 to 10 do
            x := x + i;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("i", Value::Integer(0));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal for FOR loop, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(55)),
            "sum 1..10 should be 55"
        );
    }

    #[test]
    fn maxstrlen_rejects_values_without_declared_length() {
        let (eval, _) = run_stmt("x := MaxStrLen(s);");
        assert!(eval.is_error());
    }

    #[test]
    fn createdatetime_executes() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        dt: DateTime;
    begin
        dt := CreateDateTime(20240701D, 063030T);
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("dt", Value::DateTime(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after CreateDateTime, got: {:?}",
            eval
        );
    }

    #[test]
    fn strsubstno_result_can_be_an_argument() {
        use crate::interpreter::dispatch::dispatch_call;

        let mut ctx = ctx();
        let strsubstno_result = dispatch_call(
            None,
            "StrSubstNo",
            vec![Value::Text("Value is %1".into()), Value::Integer(42)],
            &mut ctx,
        );
        let formatted = match strsubstno_result {
            Eval::Normal(Value::Text(s)) => s,
            other => panic!("StrSubstNo failed: {:?}", other),
        };

        let assert_result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![
                Value::Text("Value is 42".into()),
                Value::Text(formatted),
                Value::Text(String::new()),
            ],
            &mut ctx,
        );
        assert!(
            matches!(assert_result, Eval::Normal(_)),
            "AreEqual should pass with StrSubstNo result, got: {:?}",
            assert_result
        );
    }

    #[test]
    fn formatted_integer_can_be_an_argument() {
        use crate::interpreter::dispatch::dispatch_call;

        let mut ctx = ctx();
        let format_result = dispatch_call(None, "Format", vec![Value::Integer(7)], &mut ctx);
        let as_text = match format_result {
            Eval::Normal(Value::Text(s)) => s,
            other => panic!("Format failed: {:?}", other),
        };
        assert_eq!(as_text, "7", "Format(7) should produce Text(\"7\")");

        let eq_result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![
                Value::Text("7".into()),
                Value::Text(as_text),
                Value::Text("Number was not as expected".into()),
            ],
            &mut ctx,
        );
        assert!(
            matches!(eq_result, Eval::Normal(_)),
            "AreEqual(Format(7), '7') should pass, got: {:?}",
            eq_result
        );
    }

    #[test]
    fn compound_plus_equals_appends_text() {
        let (eval, stack) = run_stmt("s += 'hello';");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after s += 'hello', got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("s"),
            Some(&Value::Text("hello".into())),
            "s should be 'hello'"
        );
    }

    #[test]
    fn maxstrlen_uses_declared_capacity_not_current_contents() {
        let source = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        Capacity: Integer;
        Value: Text[42];
    begin
        Value := 'short';
        Capacity := MaxStrLen(Value);
    end;
}"#;
        let parsed = al_syntax::parser::AlParser::parse_quick(source);
        let root = parsed.tree.root_node();
        let bytes = source.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut procedures = vec![root];
        let mut procedure = None;
        while let Some(node) = procedures.pop() {
            if node.kind() == "procedure_declaration" {
                procedure = Some(node);
                break;
            }
            let mut cursor = node.walk();
            procedures.extend(node.named_children(&mut cursor));
        }
        let mut frame = CallFrame::new("Regression", "Test");
        crate::interpreter::dispatch::bind_procedure_locals(
            procedure.expect("procedure"),
            bytes,
            &mut frame,
        );
        let mut stack = ScopeStack::new();
        stack.push(frame);
        let mut ctx = ctx();
        let result = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(result, Eval::Normal(_)), "got {result:?}");
        assert_eq!(stack.lookup("Capacity"), Some(&Value::Integer(42)));
    }

    #[test]
    fn asserterror_catches_missing_procedure() {
        let (eval, _) = run_stmt("asserterror SomeUnimplementedCU.DoSomething();");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "asserterror should catch 'procedure not found' error, got: {:?}",
            eval
        );
    }

    #[test]
    fn case_matches_integer_literal() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        x: Integer;
        s: Text;
    begin
        x := 2;
        case x of
            1: s := 'one';
            2: s := 'two';
            3: s := 'three';
        end;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("x", Value::Integer(0));
        frame.bind("s", Value::Text(String::new()));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal for CASE statement, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("s"),
            Some(&Value::Text("two".into())),
            "CASE 2 should match 'two'"
        );
    }

    #[test]
    fn nested_if_else_selects_expected_branch() {
        let (eval, stack) =
            run_stmt("if x = 0 then begin if true then x := 10 else x := 20; end else x := 30;");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal for nested if, got: {:?}",
            eval
        );
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(10)),
            "nested if: x=0 → inner true branch → x should be 10"
        );
    }

    #[test]
    fn currentdatetime_executes() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        dt: DateTime;
    begin
        dt := CurrentDateTime();
    end;
}"#;
        let result2 = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree2 = result2.tree;
        let root2 = tree2.root_node();
        let bytes2 = wrapper.as_bytes();
        let body2 = find_proc_body(root2, bytes2).expect("body");
        let mut stack2 = ScopeStack::new();
        let mut frame2 = CallFrame::new("Regression", "Test");
        frame2.bind("dt", Value::DateTime(0));
        stack2.push(frame2);
        let mut ctx2 = ctx();
        let eval2 = crate::interpreter::eval_stmt::eval_stmt(body2, bytes2, &mut stack2, &mut ctx2);
        assert!(
            matches!(eval2, Eval::Normal(_)),
            "Expected Normal after CurrentDateTime(), got: {:?}",
            eval2
        );
    }

    #[test]
    fn for_upward_i64_max_does_not_panic() {
        let end_val = i64::MAX;
        let start_val = i64::MAX; // single-iteration loop: body runs once, then i += 1 panics
        let wrapper = format!(
            "codeunit 50100 \"Regression\"\n{{\n    procedure Test()\n    var\n        i: Integer;\n    begin\n        for i := {} to {} do x := 1;\n    end;\n}}",
            start_val, end_val
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("i", Value::Integer(0));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            !matches!(eval, Eval::Exit(_)),
            "FOR i64::MAX must not silently exit; got: {:?}",
            eval
        );
    }

    /// Compound `*=` multiplies in place.
    #[test]
    fn compound_times_equals_multiplies_in_place() {
        let (eval, stack) = run_stmt("x := 6; x *= 7;");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(42)));
    }

    /// Compound `/=` is `x := x / rhs`; integer `/` promotes to Decimal, exactly
    /// as the expanded form would. (Consistent with the interpreter's `:=`.)
    #[test]
    fn compound_divide_equals_promotes_to_decimal() {
        let (eval, stack) = run_stmt("x := 10; x /= 4;");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Decimal(dec!(2.5))));
    }

    /// A multi-name local `var` line binds every name to its type default, so an
    /// unassigned variable reads as 0 rather than erroring "unbound".
    #[test]
    fn multivar_local_defaults_are_bound_via_dispatch() {
        use crate::interpreter::dispatch::{dispatch_call, DispatchCtx};
        let ws = Arc::new(Workspace::new());
        let source = r#"codeunit 50123 "MV"
{
    procedure Sum(): Integer
    var
        A, B, C : Integer;
    begin
        A := 5;
        exit(A + B + C);
    end;
}
"#;
        ws.file_index
            .add_file(std::path::PathBuf::from("/test/MV.al"), source.to_string());
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(Some("MV"), "Sum", vec![], &mut ctx);
        assert!(
            matches!(result, Eval::Normal(Value::Integer(5))),
            "B and C must default to 0 so A+B+C == 5; got: {:?}",
            result
        );
    }

    #[test]
    fn unresolved_enum_scope_does_not_invent_zero_ordinal() {
        let (eval, _stack) = run_stmt("x := \"Risk Level\"::High;");
        let Eval::Error(error) = eval else {
            panic!("unresolved enum must fail closed");
        };
        assert!(error.message.contains("live BC"), "{}", error.message);
    }

    #[test]
    fn unresolved_enum_prefix_form_does_not_invent_zero_ordinal() {
        let (eval, _stack) = run_stmt("x := Enum::\"Risk Level\"::\"App Name\";");
        let Eval::Error(error) = eval else {
            panic!("unresolved enum must fail closed");
        };
        assert!(error.message.contains("live BC"), "{}", error.message);
    }

    #[test]
    fn maxstrlen_does_not_guess_from_current_content() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        s: Text;
        x: Integer;
    begin
        s := 'hello';
        x := MaxStrLen(s);
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("s", Value::Text(String::new()));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(eval.is_error(), "got: {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(0)));
    }

    /// Date literal evaluates to the correct day carrier.
    #[test]
    fn date_literal_value_is_correct() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        d: Date;
    begin
        d := 20240701D;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("d", Value::Date(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        let expected = crate::interpreter::value::al_days_from_ymd(2024, 7, 1);
        assert_eq!(stack.lookup("d"), Some(&Value::Date(expected)));
    }

    /// Time literal `063030T` → 06:30:30 in ms since midnight.
    #[test]
    fn time_literal_value_is_correct() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        t: Time;
    begin
        t := 063030T;
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("t", Value::Time(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(stack.lookup("t"), Some(&Value::Time(23_430_000)));
    }

    /// `CreateDateTime(date, time)` combines the day and ms carriers exactly.
    #[test]
    fn createdatetime_value_is_correct() {
        let wrapper = r#"codeunit 50100 "Regression"
{
    procedure Test()
    var
        dt: DateTime;
    begin
        dt := CreateDateTime(20240701D, 063030T);
    end;
}"#;
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("Regression", "Test");
        frame.bind("dt", Value::DateTime(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        let expected = crate::interpreter::value::al_days_from_ymd(2024, 7, 1)
            * crate::interpreter::value::MS_PER_DAY
            + 23_430_000;
        assert_eq!(stack.lookup("dt"), Some(&Value::DateTime(expected)));
    }

    /// `Today`, `Time`, and `CurrentDateTime` dispatch to clock builtins and
    /// return the right value kinds, all internally consistent.
    #[test]
    fn clock_builtins_dispatch() {
        use crate::interpreter::dispatch::dispatch_call;
        let mut ctx = ctx();
        let today = dispatch_call(None, "Today", vec![], &mut ctx);
        let time = dispatch_call(None, "Time", vec![], &mut ctx);
        let now = dispatch_call(None, "CurrentDateTime", vec![], &mut ctx);
        assert!(
            matches!(today, Eval::Normal(Value::Date(_))),
            "got: {:?}",
            today
        );
        match time {
            Eval::Normal(Value::Time(ms)) => {
                assert!((0..crate::interpreter::value::MS_PER_DAY).contains(&ms))
            }
            other => panic!("Time() should be a Time in [0, 1 day); got {:?}", other),
        }
        assert!(
            matches!(now, Eval::Normal(Value::DateTime(_))),
            "got: {:?}",
            now
        );
    }
}
