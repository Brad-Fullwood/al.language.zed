//! End-to-end interpreter tests for workspace procedure dispatch, records,
//! FlowFields, and list values.
//!
//! Each test builds a tiny workspace (`MockSource`) containing real AL table
//! and codeunit objects, then drives a test procedure through `dispatch_call`
//! — the same path the test engine uses. Assertions are on the **returned
//! value** (via `exit(...)`), never just on `Eval::Normal`.

#![cfg(test)]

use std::sync::Arc;

use crate::interpreter::dispatch::{dispatch_call, DispatchCtx};
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;
use crate::test_support::MockSource as Workspace;
use rust_decimal_macros::dec;

/// A workspace table with a `Code` primary key and a couple of data fields.
const ITEM_TABLE: &str = r#"table 50100 "Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Description; Text[100]) { }
        field(3; "Unit Price"; Decimal) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}
"#;

/// A workspace table with an Integer primary key (clean for range filters).
const NUM_TABLE: &str = r#"table 50102 "Num"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Amount; Decimal) { }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

fn run(files: &[(&str, &str)], object: &str, proc: &str, args: Vec<Value>) -> Eval {
    let ws = Arc::new(Workspace::new());
    for (path, src) in files {
        ws.file_index
            .add_file(std::path::PathBuf::from(path), src.to_string());
    }
    let mut ctx = DispatchCtx::new_pure(ws);
    dispatch_call(Some(object), proc, args, &mut ctx)
}

fn ok(eval: Eval) -> Value {
    match eval {
        Eval::Normal(v) | Eval::Exit(v) => v,
        Eval::Error(e) => panic!("unexpected error: {}", e.message),
        Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
    }
}

#[test]
fn var_param_mutation_propagates_to_caller() {
    let cu = r#"codeunit 50190 "VarParam Tests"
{
    procedure Bump(var n: Integer)
    begin
        n := n + 1;
    end;

    procedure Run(): Integer
    var
        x: Integer;
    begin
        x := 10;
        Bump(x);
        exit(x);
    end;
}
"#;
    let r = run(&[("/ws/VarParam.al", cu)], "VarParam Tests", "Run", vec![]);
    assert_eq!(ok(r), Value::Integer(11));
}

#[test]
fn decimal_arithmetic_is_exact_end_to_end() {
    let cu = r#"codeunit 50191 "Decimal Tests"
{
    procedure SumIsExact(): Boolean
    var
        d: Decimal;
    begin
        d := 0.1 + 0.2;
        exit(d = 0.3);
    end;

    procedure Accumulate(): Decimal
    var
        total: Decimal;
        i: Integer;
    begin
        total := 0;
        for i := 1 to 10 do
            total := total + 0.1;
        exit(total);
    end;
}
"#;
    let r = run(
        &[("/ws/Decimal.al", cu)],
        "Decimal Tests",
        "SumIsExact",
        vec![],
    );
    assert_eq!(
        ok(r),
        Value::Boolean(true),
        "0.1 + 0.2 must equal 0.3 exactly through the interpreter"
    );

    let r = run(
        &[("/ws/Decimal.al", cu)],
        "Decimal Tests",
        "Accumulate",
        vec![],
    );
    assert_eq!(ok(r), Value::Decimal(dec!(1.0)));
}

#[test]
fn value_param_does_not_propagate() {
    let cu = r#"codeunit 50191 "ByVal Tests"
{
    procedure Bump(n: Integer)
    begin
        n := n + 1;
    end;

    procedure Run(): Integer
    var
        x: Integer;
    begin
        x := 10;
        Bump(x);
        exit(x);
    end;
}
"#;
    let r = run(&[("/ws/ByVal.al", cu)], "ByVal Tests", "Run", vec![]);
    assert_eq!(ok(r), Value::Integer(10));
}

#[test]
fn var_param_non_lvalue_arg_is_not_written_back() {
    let cu = r#"codeunit 50192 "VarLit Tests"
{
    procedure Bump(var n: Integer): Integer
    begin
        n := n + 5;
        exit(n);
    end;

    procedure Run(): Integer
    begin
        exit(Bump(10));
    end;
}
"#;
    let r = run(&[("/ws/VarLit.al", cu)], "VarLit Tests", "Run", vec![]);
    assert_eq!(ok(r), Value::Integer(15));
}

#[test]
fn code_equality_is_case_insensitive() {
    let cu = r#"codeunit 50193 "Code Eq"
{
    procedure Run(): Boolean
    var
        c: Code[10];
    begin
        c := 'abc';
        exit(c = 'ABC');
    end;
}
"#;
    let r = run(&[("/ws/CodeEq.al", cu)], "Code Eq", "Run", vec![]);
    assert_eq!(ok(r), Value::Boolean(true));
}

#[test]
fn text_equality_is_case_sensitive() {
    let cu = r#"codeunit 50194 "Text Eq"
{
    procedure Run(): Boolean
    var
        t: Text;
    begin
        t := 'abc';
        exit(t = 'ABC');
    end;
}
"#;
    let r = run(&[("/ws/TextEq.al", cu)], "Text Eq", "Run", vec![]);
    assert_eq!(ok(r), Value::Boolean(false));
}

#[test]
fn code_case_matching_is_case_insensitive() {
    let cu = r#"codeunit 50195 "Code Case"
{
    procedure Run(): Integer
    var
        c: Code[10];
        r: Integer;
    begin
        c := 'abc';
        case c of
            'ABC':
                r := 5;
            else
                r := 1;
        end;
        exit(r);
    end;
}
"#;
    let r = run(&[("/ws/CodeCase.al", cu)], "Code Case", "Run", vec![]);
    assert_eq!(ok(r), Value::Integer(5));
}

#[test]
fn init_set_insert_get_field_get() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure InsertAndGet(): Text
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := '1000';
        Item.Description := 'Bike';
        Item.Insert();
        Item.Get('1000');
        exit(Item.Description);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "InsertAndGet",
        vec![],
    );
    assert_eq!(ok(r), Value::Text("Bike".into()));
}

#[test]
fn get_missing_returns_false() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure GetMissing(): Boolean
    var
        Item: Record "Item";
    begin
        exit(Item.Get('NOPE'));
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "GetMissing",
        vec![],
    );
    assert_eq!(ok(r), Value::Boolean(false));
}

#[test]
fn field_assignment_then_readback() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure SetThenRead(): Decimal
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'A';
        Item."Unit Price" := 19;
        Item.Insert();
        Item.Get('A');
        exit(Item."Unit Price");
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "SetThenRead",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(19));
}

#[test]
fn insert_duplicate_key_errors() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure DupInsert()
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := '1';
        Item.Insert();
        Item.Init();
        Item."No." := '1';
        Item.Insert();
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "DupInsert",
        vec![],
    );
    match r {
        Eval::Error(e) => assert!(
            e.message.to_lowercase().contains("duplicate"),
            "expected duplicate-key error, got: {}",
            e.message
        ),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn asserterror_catches_duplicate_insert() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure DupCaught(): Integer
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := '1';
        Item.Insert();
        Item.Init();
        Item."No." := '1';
        asserterror Item.Insert();
        exit(42);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "DupCaught",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(42));
}

#[test]
fn setrange_then_count() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure CountInRange(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 10 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetRange("Entry No.", 3, 7);
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "CountInRange",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(5)); // 3,4,5,6,7
}

#[test]
fn setrange_single_value_exact() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure CountExact(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 5 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetRange("Entry No.", 4);
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "CountExact",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(1));
}

#[test]
fn setfilter_then_count() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure CountFiltered(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 6 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetFilter("Entry No.", '>=4');
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "CountFiltered",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(3)); // 4,5,6
}

#[test]
fn findset_next_iteration_sums_amount() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure SumAmounts(): Decimal
    var
        Num: Record "Num";
        i: Integer;
        total: Decimal;
    begin
        for i := 1 to 4 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Amount := i * 10;
            Num.Insert();
        end;
        total := 0;
        if Num.FindSet() then
            repeat
                total := total + Num.Amount;
            until Num.Next() = 0;
        exit(total);
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "SumAmounts",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(100));
}

#[test]
fn findfirst_reads_lowest_key() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure FirstKey(): Integer
    var
        Num: Record "Num";
    begin
        Num.Init(); Num."Entry No." := 30; Num.Insert();
        Num.Init(); Num."Entry No." := 10; Num.Insert();
        Num.Init(); Num."Entry No." := 20; Num.Insert();
        if Num.FindFirst() then
            exit(Num."Entry No.");
        exit(-1);
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "FirstKey",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(10));
}

#[test]
fn isempty_true_then_false() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure EmptyThenNot(): Boolean
    var
        Num: Record "Num";
        wasEmpty: Boolean;
    begin
        wasEmpty := Num.IsEmpty();
        Num.Init(); Num."Entry No." := 1; Num.Insert();
        exit(wasEmpty and (not Num.IsEmpty()));
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "EmptyThenNot",
        vec![],
    );
    assert_eq!(ok(r), Value::Boolean(true));
}

#[test]
fn delete_then_count() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure DeleteOne(): Integer
    var
        Num: Record "Num";
    begin
        Num.Init(); Num."Entry No." := 1; Num.Insert();
        Num.Init(); Num."Entry No." := 2; Num.Insert();
        Num.Get(1);
        Num.Delete();
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "DeleteOne",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(1));
}

#[test]
fn deleteall_empties_table() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure WipeAll(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 5 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.DeleteAll();
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "WipeAll",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(0));
}

#[test]
fn modify_updates_row() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure ModifyDesc(): Text
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'X';
        Item.Description := 'Old';
        Item.Insert();
        Item.Get('X');
        Item.Description := 'New';
        Item.Modify();
        Item.Get('X');
        exit(Item.Description);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "ModifyDesc",
        vec![],
    );
    assert_eq!(ok(r), Value::Text("New".into()));
}

#[test]
fn unknown_table_errors_gracefully() {
    let cu = r#"codeunit 50104 "Cust Tests"
{
    procedure UseCustomer()
    var
        Cust: Record "Customer";
    begin
        Cust.Init();
        Cust."No." := '1';
        Cust.Insert();
    end;
}
"#;
    let r = run(
        &[("/ws/CustTests.al", cu)],
        "Cust Tests",
        "UseCustomer",
        vec![],
    );
    match r {
        Eval::Error(e) => assert!(
            e.message.to_lowercase().contains("not found in workspace"),
            "expected a graceful 'not found in workspace' error, got: {}",
            e.message
        ),
        other => panic!("expected Error for unknown table, got {other:?}"),
    }
}

#[test]
fn two_record_vars_share_physical_table() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure SharedTable(): Text
    var
        Writer: Record "Item";
        Reader: Record "Item";
    begin
        Writer.Init();
        Writer."No." := 'SH';
        Writer.Description := 'Shared';
        Writer.Insert();
        Reader.Get('SH');
        exit(Reader.Description);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "SharedTable",
        vec![],
    );
    assert_eq!(ok(r), Value::Text("Shared".into()));
}

/// A header table with several FlowFields over the detail table below, plus a
/// detail table with a composite primary key.
const SALES_DOC_TABLE: &str = r#"table 50120 "Sales Doc"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Balance; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Sum("Sales Detail".Amount WHERE ("Doc No." = FIELD("No.")));
        }
        field(3; "Line Count"; Integer)
        {
            FieldClass = FlowField;
            CalcFormula = Count("Sales Detail" WHERE ("Doc No." = FIELD("No.")));
        }
        field(4; "Item Total"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Sum("Sales Detail".Amount WHERE ("Doc No." = FIELD("No."), Type = CONST(Item)));
        }
        field(5; "Big Total"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Sum("Sales Detail".Amount WHERE ("Doc No." = FIELD("No."), Amount = FILTER(>15)));
        }
        field(6; "Has Lines"; Boolean)
        {
            FieldClass = FlowField;
            CalcFormula = Exist("Sales Detail" WHERE ("Doc No." = FIELD("No.")));
        }
        field(7; "Max Amount"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Max("Sales Detail".Amount WHERE ("Doc No." = FIELD("No.")));
        }
        field(8; "Avg Amount"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Average("Sales Detail".Amount WHERE ("Doc No." = FIELD("No.")));
        }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}
"#;

const SALES_DETAIL_TABLE: &str = r#"table 50121 "Sales Detail"
{
    fields
    {
        field(1; "Doc No."; Code[20]) { }
        field(2; "Line No."; Integer) { }
        field(3; Amount; Decimal) { }
        field(4; Type; Code[10]) { }
    }
    keys
    {
        key(PK; "Doc No.", "Line No.") { }
    }
}
"#;

/// Codeunit that seeds ORD1 (3 lines: 10/Item, 20/Service, 30/Item), an
/// unrelated ORD2 line (999), and an empty document EMPTY, then exposes each
/// FlowField read so a test can assert the computed value.
const FLOW_TESTS: &str = r#"codeunit 50122 "Flow Tests"
{
    procedure Seed()
    var
        Hdr: Record "Sales Doc";
        Det: Record "Sales Detail";
    begin
        Hdr.Init(); Hdr."No." := 'ORD1'; Hdr.Insert();
        Hdr.Init(); Hdr."No." := 'EMPTY'; Hdr.Insert();

        Det.Init(); Det."Doc No." := 'ORD1'; Det."Line No." := 1; Det.Amount := 10; Det.Type := 'Item'; Det.Insert();
        Det.Init(); Det."Doc No." := 'ORD1'; Det."Line No." := 2; Det.Amount := 20; Det.Type := 'Service'; Det.Insert();
        Det.Init(); Det."Doc No." := 'ORD1'; Det."Line No." := 3; Det.Amount := 30; Det.Type := 'Item'; Det.Insert();
        Det.Init(); Det."Doc No." := 'ORD2'; Det."Line No." := 1; Det.Amount := 999; Det.Type := 'Item'; Det.Insert();
    end;

    procedure SumViaCalcFields(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        Hdr.CalcFields(Balance);
        exit(Hdr.Balance);
    end;

    procedure CountOnRead(): Integer
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Line Count");
    end;

    procedure FilteredConst(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Item Total");
    end;

    procedure FilteredExpr(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Big Total");
    end;

    procedure MaxOnRead(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Max Amount");
    end;

    procedure AvgOnRead(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Avg Amount");
    end;

    procedure ExistTrue(): Boolean
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('ORD1');
        exit(Hdr."Has Lines");
    end;

    procedure EmptySum(): Decimal
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('EMPTY');
        exit(Hdr.Balance);
    end;

    procedure EmptyExist(): Boolean
    var
        Hdr: Record "Sales Doc";
    begin
        Seed();
        Hdr.Get('EMPTY');
        exit(Hdr."Has Lines");
    end;
}
"#;

fn run_flow(proc: &str) -> Eval {
    run(
        &[
            ("/ws/SalesDoc.al", SALES_DOC_TABLE),
            ("/ws/SalesDetail.al", SALES_DETAIL_TABLE),
            ("/ws/FlowTests.al", FLOW_TESTS),
        ],
        "Flow Tests",
        proc,
        vec![],
    )
}

#[test]
fn flowfield_sum_via_calcfields() {
    assert_eq!(ok(run_flow("SumViaCalcFields")), Value::Integer(60));
}

#[test]
fn flowfield_count_on_read() {
    assert_eq!(ok(run_flow("CountOnRead")), Value::Integer(3));
}

#[test]
fn flowfield_filtered_const() {
    assert_eq!(ok(run_flow("FilteredConst")), Value::Integer(40));
}

#[test]
fn flowfield_filtered_expr() {
    assert_eq!(ok(run_flow("FilteredExpr")), Value::Integer(50));
}

#[test]
fn flowfield_max() {
    assert_eq!(ok(run_flow("MaxOnRead")), Value::Integer(30));
}

#[test]
fn flowfield_average() {
    assert_eq!(ok(run_flow("AvgOnRead")), Value::Decimal(dec!(20.0)));
}

#[test]
fn flowfield_exist_true() {
    assert_eq!(ok(run_flow("ExistTrue")), Value::Boolean(true));
}

#[test]
fn flowfield_empty_sum_is_zero() {
    assert_eq!(ok(run_flow("EmptySum")), Value::Integer(0));
}

#[test]
fn flowfield_empty_exist_is_false() {
    assert_eq!(ok(run_flow("EmptyExist")), Value::Boolean(false));
}

const MATH_LIB: &str = r#"codeunit 50200 "Math Lib"
{
    procedure Add(a: Integer; b: Integer): Integer
    begin
        exit(a + b);
    end;

    procedure Double(n: Integer): Integer
    begin
        exit(n * 2);
    end;
}
"#;

#[test]
fn codeunit_variable_dispatch_executes_real_body() {
    let caller = r#"codeunit 50201 "Caller"
{
    procedure Compute(): Integer
    var
        lib: Codeunit "Math Lib";
    begin
        exit(lib.Add(2, 3));
    end;
}
"#;
    let r = run(
        &[("/ws/MathLib.al", MATH_LIB), ("/ws/Caller.al", caller)],
        "Caller",
        "Compute",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(5));
}

#[test]
fn codeunit_variable_nested_calls() {
    let caller = r#"codeunit 50201 "Caller"
{
    procedure Compute(): Integer
    var
        lib: Codeunit "Math Lib";
    begin
        exit(lib.Double(lib.Add(2, 3)));
    end;
}
"#;
    let r = run(
        &[("/ws/MathLib.al", MATH_LIB), ("/ws/Caller.al", caller)],
        "Caller",
        "Compute",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(10)); // (2+3)*2
}

#[test]
fn rhs_expression_position_cross_object_dispatch() {
    let caller = r#"codeunit 50202 "Direct"
{
    procedure Compute(): Integer
    begin
        exit("Math Lib".Add(10, 20));
    end;
}
"#;
    let r = run(
        &[("/ws/MathLib.al", MATH_LIB), ("/ws/Direct.al", caller)],
        "Direct",
        "Compute",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(30));
}

#[test]
fn unresolved_codeunit_object_is_graceful_error() {
    let caller = r#"codeunit 50201 "Caller"
{
    procedure Compute(): Integer
    var
        lib: Codeunit "No Such Lib";
    begin
        exit(lib.Add(1, 2));
    end;
}
"#;
    let r = run(&[("/ws/Caller.al", caller)], "Caller", "Compute", vec![]);
    assert!(r.is_error(), "expected a graceful error, got {r:?}");
}

#[test]
fn list_add_get_count_via_dispatch() {
    let cu = r#"codeunit 50300 "List Tests"
{
    procedure Build(): Integer
    var
        items: List of [Integer];
    begin
        items.Add(10);
        items.Add(20);
        items.Add(30);
        exit(items.Count());
    end;
}
"#;
    let r = run(&[("/ws/ListTests.al", cu)], "List Tests", "Build", vec![]);
    assert_eq!(ok(r), Value::Integer(3));
}

#[test]
fn list_get_returns_element() {
    let cu = r#"codeunit 50300 "List Tests"
{
    procedure Second(): Text
    var
        items: List of [Text];
    begin
        items.Add('a');
        items.Add('b');
        items.Add('c');
        exit(items.Get(2));
    end;
}
"#;
    let r = run(&[("/ws/ListTests.al", cu)], "List Tests", "Second", vec![]);
    assert_eq!(ok(r), Value::Text("b".into()));
}

#[test]
fn list_contains() {
    let cu = r#"codeunit 50300 "List Tests"
{
    procedure HasIt(): Boolean
    var
        items: List of [Integer];
    begin
        items.Add(1);
        items.Add(2);
        exit(items.Contains(2));
    end;
}
"#;
    let r = run(&[("/ws/ListTests.al", cu)], "List Tests", "HasIt", vec![]);
    assert_eq!(ok(r), Value::Boolean(true));
}
