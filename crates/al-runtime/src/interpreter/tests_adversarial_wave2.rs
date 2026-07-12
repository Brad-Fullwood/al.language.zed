//! Adversarial wave 2 — BCApps corpus findings.
//!
//! Each test was derived by loading or inspecting real BCApps test source
//! from `tests/.repos/BCApps/` and running the snippet through the Phase 2
//! interpreter.  Tests are categorised:
//!
//!   PASS            — test runs, asserts hold (good)
//!   FAIL            — real assertion failure (expected BCApps behaviour)
//!   PANIC           — interpreter panics (OUR BUG, guarded #[ignore])
//!   TYPE_ERROR      — Eval::Error from a type mismatch (OUR BUG, guarded #[ignore])
//!   UNIMPLEMENTED   — Eval::Error "unsupported …" (known Phase-2 gap)
//!
//! All bug-repro tests use `#[ignore]` so CI does not break.
//! Source references are BCApps file paths relative to the repo root.
//!
//! # Findings summary (wave 2)
//!
//! | ID    | Category        | Synopsis                                          |
//! |-------|-----------------|---------------------------------------------------|
//! | W2-01 | UNIMPLEMENTED   | Date literal `20240701D` → unsupported expr kind  |
//! | W2-02 | UNIMPLEMENTED   | Time literal `063030T` → unsupported expr kind    |
//! | W2-03 | TYPE_ERROR      | Compound `+=` operator → "not supported on Int"   |
//! | W2-04 | TYPE_ERROR      | Compound `-=` operator → "not supported on Int"   |
//! | W2-05 | UNIMPLEMENTED   | Multi-var decl `a, b : Integer` — second var unbound |
//! | W2-06 | PANIC           | FOR downto with `i64::MIN` start → i -= 1 panics  |
//! | W2-07 | UNIMPLEMENTED   | `Enum::` scope-qualified enum member not evaluated |
//! | W2-08 | UNIMPLEMENTED   | `List of [T]` variable → member calls unresolved  |
//! | W2-09 | PASS            | BigInteger `L`-suffix stripped correctly           |
//! | W2-10 | TYPE_ERROR      | FOR loop body `x := x + i` — `operator` node kind not handled |
//! | W2-11 | UNIMPLEMENTED   | `MaxStrLen()` builtin not implemented              |
//! | W2-12 | UNIMPLEMENTED   | `CreateDateTime()` builtin not implemented         |
//! | W2-13 | PASS            | `StrSubstNo` used as argument to `Assert.AreEqual` |
//! | W2-14 | PASS            | `Format(integer)` used as argument to AreEqual     |
//! | W2-15 | TYPE_ERROR      | Compound `+=` on Text → "not supported on Text"   |
//! | W2-16 | PASS            | `asserterror` correctly catches "procedure not found" |
//! | W2-17 | TYPE_ERROR      | CASE on integer fails — arm value eval returns unsupported |
//! | W2-18 | PASS            | Nested if/else chains evaluate correctly           |
//! | W2-19 | UNIMPLEMENTED   | `CurrentDateTime` global fn not implemented        |
//! | W2-20 | PANIC           | FOR upward loop with `i64::MAX` end → i += 1 OOB  |

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
            "codeunit 50100 \"W2\"\n{{\n    procedure Test()\n    var\n        x: Integer;\n        s: Text;\n        b: Boolean;\n    begin\n        {source_snippet}\n    end;\n}}"
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();

        let body = find_proc_body(root, bytes).expect("could not find procedure body");

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-01  Date literal
    //
    // Source: src/System Application/Test/Date and Time/src/UnixTimestampTest.Codeunit.al
    //   GivenDateTime := CreateDateTime(20240701D, 063030T);
    //
    // The sub-expression `20240701D` is a `date_literal` node.  eval_expr has
    // no `"date_literal"` arm, so it hits the catch-all and returns
    //   Eval::Error("unsupported expression kind in Phase 2a: date_literal")
    //
    // Expected: Eval::Normal(Value::Date(...)) — a Date value representing 2024-07-01.
    // Observed: Eval::Error("unsupported expression kind in Phase 2a: date_literal")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_01_date_literal_unimplemented() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-02  Time literal
    //
    // Source: same file — `063030T` is a time literal.
    //
    // Expected: Eval::Normal(Value::Time(...)) — ms-since-midnight for 06:30:30.
    // Observed: Eval::Error("unsupported expression kind in Phase 2a: time_literal")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_02_time_literal_unimplemented() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-03  Compound assignment `+=`
    //
    // Source: src/System Application/Test/Filter Tokens/src/FilterTokensTest.Codeunit.al
    //   NoOfAttempts += 1;
    //   (also: src/Apps/W1/DataSearch/Test/TestDataSearch.codeunit.al, line 279)
    //
    // The grammar parses `x += 1` as an `expression` node with `binary_operator`
    // `+=`.  eval_expression_node looks only for `:=`; when `+=` is seen instead,
    // it falls through to eval_expr_chain which calls apply_binary("+=", ...).
    // apply_binary has no `+=` arm — it hits the catch-all error.
    //
    // Expected: Eval::Normal — x incremented by 1.
    // Observed: Eval::Error("binary operator `+=` not supported on (Integer, Integer)")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_03_compound_plus_equals_not_implemented() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-04  Compound assignment `-=`
    //
    // Source: BCApps DataArchiveExportToExcel.Codeunit.al, similar pattern.
    //
    // Same root cause as W2-03: apply_binary has no `-=` arm.
    //
    // Expected: Eval::Normal — x decremented.
    // Observed: Eval::Error("binary operator `-=` not supported on …")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_04_compound_minus_equals_not_implemented() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-05  Multi-variable declaration `a, b : Integer`
    //
    // Source: src/System Application/Test/Encoding/src/EncodingTest.Codeunit.al
    //   UTF8, ISO88591 : Integer;
    //   ConvertedText, ExpectedText : Text;
    //
    // The AL grammar allows multiple names on one `var` declaration line.
    // The interpreter's frame setup (run_stmt harness) only binds variables
    // declared with separate `var` entries.  If the grammar emits both names
    // as siblings of the same `parameter` / `var_section_entry` node, only
    // the first name gets bound and the second is unresolvable.
    //
    // Expected: both `utf8` and `iso88591` are bound to Integer(0) initially.
    // Observed: second identifier is unbound → Eval::Error("unbound identifier: iso88591")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_05_multi_var_declaration_second_var_unbound() {
        // We use a wrapper that declares two vars on one line, mirroring BCApps.
        let wrapper = r#"codeunit 50100 "W2"
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
        let frame = CallFrame::new("W2", "Test");
        // Both vars should be initialised to their default by the dispatcher;
        // for this test we start the frame empty and rely on auto-bind.
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-06  FOR downto loop with `i64::MIN` start — arithmetic panic
    //
    // Source: general BCApps pattern — FOR i := N DOWNTO 1 DO …
    //
    // eval_stmt::eval_for uses plain `i -= 1` (line 298) without overflow
    // guard.  When the start value is i64::MIN and the DOWNTO loop body
    // finishes, `i -= 1` panics with "attempt to subtract with overflow"
    // in debug builds.
    //
    // Expected: Eval::Error (overflow → runtime error, not panic).
    // Observed: thread panic "attempt to subtract with overflow"
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_06_for_downto_i64_min_overflow_panic() {
        // FOR i := -9223372036854775808 DOWNTO -9223372036854775809
        // — the DOWNTO condition `i < end` is never satisfied on the first
        // iteration (start == i64::MIN == end+1), so one body execution
        // runs and then `i -= 1` panics.
        //
        // In practice real tests never hit this, but the interpreter must
        // not panic — it should return Eval::Error.
        let wrapper = format!(
            "codeunit 50100 \"W2\"\n{{\n    procedure Test()\n    var\n        i: Integer;\n    begin\n        for i := {} downto {} do x := 1;\n    end;\n}}",
            i64::MIN,
            // end value: one less than MIN so the loop runs one iteration and
            // then hits the decrement
            i64::MIN
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("i", Value::Integer(0));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        // Must NOT panic; should return an Error or Normal (empty loop).
        assert!(
            !matches!(eval, Eval::Exit(_)),
            "FOR downto i64::MIN must not silently exit; got: {:?}",
            eval
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // W2-07  `Enum::` scope-qualified enum member
    //
    // Source: src/System Application/Test/Performance Profiler/src/ProfilingDataProcessorTest.Codeunit.al
    //   Assert.AreEqual('TestNode1App',
    //     PerfProfilerTestLibrary.GetUniqueIdentifierByAggregationType(
    //       ProfilingNode,
    //       Enum::"Test Prof. Aggregation Type"::"App Name"),
    //     'Incorrect app name.');
    //
    // Also: src/Apps/W1/Shopify/Test/Customers/ShpfyCountySourceTest.Codeunit.al
    //   ICounty := "Shpfy County Source"::Code;
    //
    // The interpreter attempts to dispatch this as a `scope_call_suffix` member
    // access.  The receiver text is `Enum::"Shpfy Risk Level"` and the procedure
    // name is `Low`.  dispatch_call routes this through workspace lookup and
    // returns "procedure not found".
    //
    // Expected: Eval::Normal(Value::Option { ... }) — the enum ordinal value.
    // Observed: Eval::Error("object 'Enum' not found in workspace") or
    //           Eval::Error("procedure not found: …::Low")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_07_enum_scope_qualifier_not_implemented() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("myoption", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after enum assignment, got: {:?}",
            eval
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // W2-08  `List of [T]` member access
    //
    // Source: src/System Application/Test/Performance Profiler/src/PerfProfilerChartTest.Codeunit.al
    //   ChartLabels: List of [Text];
    //   ChartLabels.Get(1)
    //
    // The interpreter has `Value::List(Vec<Value>)` but no dispatch path to
    // resolve member calls on a `List of [T]` variable (`.Add()`, `.Get()`,
    // `.Count()` etc.).  The dispatch falls through to workspace lookup for an
    // object named "ChartLabels" which does not exist.
    //
    // Expected: Eval::Normal — list operations execute correctly.
    // Observed: Eval::Error("object 'chartlabels' not found in workspace")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_08_list_member_calls_not_dispatched() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-09  BigInteger `L`-suffix — PASS (no bug)
    //
    // Source: src/System Application/Test/Date and Time/src/UnixTimestampTest.Codeunit.al
    //   LibraryAssert.AreEqual(ResultTimestamp, 1719815430L, '...');
    //
    // eval_expr strips trailing `l`/`L` from integer_literal text before
    // parsing as i64.  This already works correctly.
    //
    // Expected: Eval::Normal(Value::Integer(1719815430))
    // Observed: Eval::Normal(Value::Integer(1719815430))  ← PASS
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_09_biginteger_l_suffix_pass() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-10  FOR loop body `x := x + i` — `operator` node kind not handled
    //
    // Source: src/Business Foundation/Test/NoSeries/src/NoSeriesTests.Codeunit.al
    //   for i := 1 to 10 do
    //       LibraryAssert.AreEqual(Format(i), NoSeries.GetNextNo(NoSeriesCode), '...');
    //
    // When the FOR loop body contains an expression like `x := x + i`, the
    // grammar emits the `+` token as a bare `operator` node (not
    // `binary_operator`).  eval_expr's catch-all fires:
    //   Eval::Error("unsupported expression kind in Phase 2a: operator")
    //
    // Root cause: eval_expr handles `"binary_operator"` in eval_expression_node
    // but the FOR loop body context produces a raw `"operator"` node.
    // eval_expr has no `"operator"` arm.
    //
    // Expected: Eval::Normal, x == 55 (sum 1..10).
    // Observed: Eval::Error("unsupported expression kind in Phase 2a: operator")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_10_for_upward_loop_accumulates_pass() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-11  `MaxStrLen()` builtin
    //
    // Source: src/Business Foundation/Test/NoSeries/src/NoSeriesTests.Codeunit.al
    //   NoSeriesCode := CopyStr(UpperCase(Any.AlphabeticText(MaxStrLen(NoSeriesCode))), 1, MaxStrLen(NoSeriesCode));
    //
    // `MaxStrLen(var)` is a BC built-in that returns the declared maximum
    // length of a Text/Code variable.  It is not implemented as an inline
    // builtin in dispatch.rs.
    //
    // Expected: Eval::Normal(Value::Integer(N)) — the declared length.
    // Observed: Eval::Error("procedure not found: MaxStrLen")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    #[ignore = "W2-11 NOW IMPLEMENTED but repro assertion is wrong: MaxStrLen() is dispatched \
                (see b4_maxstrlen_* tests below), but this repro asserts the *statement* evaluates \
                to Normal(Integer). An assignment statement evaluates to Normal(Empty) — the integer \
                lands in `x`, not in the Eval. Kept ignored rather than rewrite an existing assertion."]
    fn w2_11_maxstrlen_not_implemented() {
        let (eval, _) = run_stmt("x := MaxStrLen(s);");
        assert!(
            matches!(eval, Eval::Normal(Value::Integer(_))),
            "Expected Normal(Integer) from MaxStrLen, got: {:?}",
            eval
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // W2-12  `CreateDateTime()` builtin
    //
    // Source: src/System Application/Test/Date and Time/src/UnixTimestampTest.Codeunit.al
    //   GivenDateTime := CreateDateTime(20240701D, 063030T);
    //
    // `CreateDateTime(date, time)` is a BC global that is not an inline
    // builtin in dispatch.rs.
    //
    // Expected: Eval::Normal(Value::DateTime(...)) — the combined datetime.
    // Observed: Eval::Error("procedure not found: CreateDateTime")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_12_createdatetime_not_implemented() {
        let (_eval, _) = run_stmt("x := 1;"); // placeholder — real test needs Date vars
                                              // The real failing pattern from BCApps:
                                              //   GivenDateTime := CreateDateTime(20240701D, 063030T);
                                              // Both W2-01 (date literal) and this builtin absence are needed.
                                              // This test documents the missing builtin separately.
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("dt", Value::DateTime(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(
            matches!(eval, Eval::Normal(_)),
            "Expected Normal after CreateDateTime, got: {:?}",
            eval
        );
        let _ = eval;
    }

    // ══════════════════════════════════════════════════════════════════════════
    // W2-13  `StrSubstNo` as argument to `Assert.AreEqual` — PASS
    //
    // Source: src/Business Foundation/Test/NoSeries/src/NoSeriesTests.Codeunit.al
    //   LibraryAssert.AreEqual(Format(i), NoSeries.GetNextNo(NoSeriesCode), '...');
    //
    // Tests that using a builtin call return value inline as an argument works.
    // StrSubstNo is already an inline builtin; passing its result to AreEqual
    // should work.
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_13_strsubstno_as_argument_pass() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-14  `Format(integer)` as argument to `AreEqual` — PASS
    //
    // Source: src/Business Foundation/Test/NoSeries/src/NoSeriesTests.Codeunit.al
    //   LibraryAssert.AreEqual(Format(i), result, 'Number was not as expected');
    //
    // The `Format` builtin already converts Integer → Text.  Tests that this
    // round-trips correctly when used inline.
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_14_format_integer_inline_argument_pass() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-15  Compound `+=` on Text — TYPE_ERROR
    //
    // Source: src/Apps/W1/DataSearch/Test/TestDataSearch.codeunit.al line 470:
    //   ExpectedCaption += ' - ' + 'lines';
    //
    // Same root cause as W2-03 but on a Text operand.  apply_binary has no
    // `+=` arm regardless of the operand type.
    //
    // Expected: Eval::Normal — s appended.
    // Observed: Eval::Error("binary operator `+=` not supported on (Text, Text)")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_15_compound_plus_equals_text_not_implemented() {
        // s starts as ''; after `s += 'hello'` it should be 'hello'.
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-16  `asserterror` on UNIMPLEMENTED call — PASS
    //
    // Source: multiple BCApps test files use patterns like:
    //   asserterror Encoding.Convert(NonExistingEncoding, ISO88591, TextToConvert);
    //   Assert.ExpectedError('Valid values are between 0 and 65535');
    //
    // When the inner call is unimplemented (procedure not found), the
    // interpreter returns Eval::Error.  `asserterror` catches it — this is
    // correct Phase-2 behaviour.  This test verifies that asserterror
    // correctly swallows a "procedure not found" error rather than re-raising.
    //
    // Note: `Assert.ExpectedError(msg)` is NOT a stub we implement, so the
    // full BCApps pattern can't be tested; we just verify asserterror absorbs.
    //
    // Expected: Eval::Normal — asserterror swallows the error.
    // Observed: Eval::Normal  ← PASS
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_16_asserterror_catches_unimplemented_call_pass() {
        // The procedure is not in the workspace, so dispatch returns Error.
        // asserterror should absorb it and return Normal.
        let (eval, _) = run_stmt("asserterror SomeUnimplementedCU.DoSomething();");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "asserterror should catch 'procedure not found' error, got: {:?}",
            eval
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // W2-17  CASE on integer literal — arm match fails
    //
    // Source: multiple BCApps test patterns use CASE on integer/option variables.
    //
    // eval_case finds the arm value nodes but eval_expr on the literal `2`
    // inside a `case_arm` returns an error (the arm value node kind may differ
    // from what eval_expr expects in that grammar context).  The arm is skipped,
    // no arm matches, and the ELSE is absent, so the result is Normal(Empty)
    // with `s` unchanged.
    //
    // Root cause: `eval_case` evaluates arm values with `eval_expr(val_node, …)`.
    // If `val_node` is not a recognised expression kind (e.g. it has a different
    // wrapping node kind in the case context), the Eval::Error silently causes
    // the arm to be skipped rather than propagated.  The comment in eval_case
    // (`// If we can't evaluate a value, skip this arm value`) makes all
    // mis-evaluations silent, masking bugs.
    //
    // Expected: s == "two" (arm 2 matched).
    // Observed: s == "" (no arm matched, ELSE absent, Normal(Empty) returned).
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    #[ignore = "W2-17 GRAMMAR: multi-arm CASE without begin/end parsed as one arm with statement_list body; second arm absorbed as expression with ':' operator — grammar fix needed in tree-sitter-al"]
    fn w2_17_case_on_integer_literal_pass() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-18  Nested if/else chains — PASS
    //
    // Source: BCApps tests universally use if/else chains as control flow.
    //
    // Verifies that deeply-nested if/else evaluates correctly with the correct
    // branch selected.
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_18_nested_if_else_chains_pass() {
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-19  `CurrentDateTime` global function
    //
    // Source: src/System Application/Test/Filter Tokens/src/FilterTokensTest.Codeunit.al
    //   ExpectedFilterText := STRSUBSTNO(ExpectedText, CURRENTDATETIME());
    //
    // `CurrentDateTime` is a BC global that returns the current date-time.
    // It is not implemented as an inline builtin in dispatch.rs.
    //
    // Expected: Eval::Normal(Value::DateTime(...)) — current datetime.
    // Observed: Eval::Error("procedure not found: CurrentDateTime")
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_19_currentdatetime_not_implemented() {
        let (_eval, _) = run_stmt("x := 1;"); // placeholder
                                              // The real failing pattern:
                                              //   dt := CurrentDateTime();
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame2 = CallFrame::new("W2", "Test");
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

    // ══════════════════════════════════════════════════════════════════════════
    // W2-20  FOR upward loop with `i64::MAX` end — arithmetic panic
    //
    // Source: general BCApps pattern — FOR i := 1 TO N DO …
    //
    // eval_stmt::eval_for uses plain `i += 1` (line 300) without overflow
    // guard.  When the end value is i64::MAX and the loop completes one
    // iteration, `i += 1` panics with "attempt to add with overflow" in
    // debug builds.
    //
    // Expected: Eval::Error (overflow → runtime error, not panic).
    // Observed: thread panic "attempt to add with overflow"
    //
    // Note: the guard condition checks `i > end_i` but uses plain `i += 1`
    // — on the last iteration i == i64::MAX, the body runs, then i += 1
    // overflows before the loop-exit check.
    // ══════════════════════════════════════════════════════════════════════════
    #[test]
    fn w2_20_for_upward_i64_max_overflow_panic() {
        let end_val = i64::MAX;
        let start_val = i64::MAX; // single-iteration loop: body runs once, then i += 1 panics
        let wrapper = format!(
            "codeunit 50100 \"W2\"\n{{\n    procedure Test()\n    var\n        i: Integer;\n    begin\n        for i := {} to {} do x := 1;\n    end;\n}}",
            start_val, end_val
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(root, bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("i", Value::Integer(0));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        // Must NOT panic; should return an Error or Normal.
        assert!(
            !matches!(eval, Eval::Exit(_)),
            "FOR i64::MAX must not silently exit; got: {:?}",
            eval
        );
    }

    // ══════════════════════════════════════════════════════════════════════════
    // B4 slice — focused tests for the newly-implemented interpreter features.
    // These assert on the *resulting values* (not just Eval::Normal), which the
    // wave-2 repro tests above mostly do not.
    // ══════════════════════════════════════════════════════════════════════════

    /// Compound `*=` multiplies in place.
    #[test]
    fn b4_compound_times_equals() {
        let (eval, stack) = run_stmt("x := 6; x *= 7;");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(42)));
    }

    /// Compound `/=` is `x := x / rhs`; integer `/` promotes to Decimal, exactly
    /// as the expanded form would. (Consistent with the interpreter's `:=`.)
    #[test]
    fn b4_compound_divide_equals_promotes_to_decimal() {
        let (eval, stack) = run_stmt("x := 10; x /= 4;");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Decimal(dec!(2.5))));
    }

    /// A multi-name local `var` line binds every name to its type default, so an
    /// unassigned variable reads as 0 rather than erroring "unbound".
    #[test]
    fn b4_multivar_local_defaults_bound_via_dispatch() {
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

    /// `"Enum Type"::Member` evaluates to a `Value::Option` carrying the type and
    /// member names (ordinal unresolved → 0, the interpreter is BC-free).
    #[test]
    fn b4_enum_scope_simple_produces_option() {
        let (eval, stack) = run_stmt("x := \"Risk Level\"::High;");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Option {
                type_name: "Risk Level".into(),
                member: "High".into(),
                ordinal: 0,
            })
        );
    }

    /// `Enum::"Type"::"Value"` — the `Enum` keyword prefix form. The first scope
    /// member is the type, the last is the value.
    #[test]
    fn b4_enum_scope_enum_prefix_form() {
        let (eval, stack) = run_stmt("x := Enum::\"Risk Level\"::\"App Name\";");
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Option {
                type_name: "Risk Level".into(),
                member: "App Name".into(),
                ordinal: 0,
            })
        );
    }

    /// `MaxStrLen` is dispatched as an inline builtin and lands in the LHS.
    /// (See the W2-11 note: this asserts the *stored value*, the correct thing.)
    #[test]
    fn b4_maxstrlen_dispatched_into_variable() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("s", Value::Text(String::new()));
        frame.bind("x", Value::Integer(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        // s == "hello" → 5 (current-content length; see builtin_maxstrlen note).
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(5)));
    }

    /// Date literal evaluates to the correct day carrier.
    #[test]
    fn b4_date_literal_value_correct() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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
    fn b4_time_literal_value_correct() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
        frame.bind("t", Value::Time(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = crate::interpreter::eval_stmt::eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(matches!(eval, Eval::Normal(_)), "got: {:?}", eval);
        // ((6*60 + 30)*60 + 30) * 1000
        assert_eq!(stack.lookup("t"), Some(&Value::Time(23_430_000)));
    }

    /// `CreateDateTime(date, time)` combines the day and ms carriers exactly.
    #[test]
    fn b4_createdatetime_value_correct() {
        let wrapper = r#"codeunit 50100 "W2"
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
        let mut frame = CallFrame::new("W2", "Test");
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
    fn b4_clock_builtins_dispatch() {
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
