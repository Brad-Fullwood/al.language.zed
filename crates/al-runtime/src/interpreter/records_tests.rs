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
    let mut ctx = DispatchCtx::new_with_records(ws, Default::default());
    dispatch_call(Some(object), proc, args, &mut ctx)
}

fn ok(eval: Eval) -> Value {
    match eval {
        Eval::Normal(v) | Eval::Exit(v) => v,
        Eval::Error(e) => panic!("unexpected error: {}", e.message),
        Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
    }
}

fn error_message(eval: Eval) -> String {
    match eval {
        Eval::Error(error) => error.message,
        other => panic!("expected an interpreter error, got {other:?}"),
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
fn same_codeunit_nested_call_preserves_object_globals() {
    let cu = r#"codeunit 50189 "Global State Tests"
{
    var
        Counter: Integer;

    procedure Run(): Integer
    begin
        Increment();
        Increment();
        exit(Counter);
    end;

    local procedure Increment()
    begin
        Counter := Counter + 1;
    end;
}
"#;
    let result = run(
        &[("/ws/GlobalStateTests.al", cu)],
        "Global State Tests",
        "Run",
        vec![],
    );
    assert_eq!(ok(result), Value::Integer(2));
}

#[test]
fn stateful_cross_codeunit_call_fails_closed() {
    let stateful = r#"codeunit 50187 "Stateful Helper"
{
    var
        Counter: Integer;

    procedure Next(): Integer
    begin
        Counter := Counter + 1;
        exit(Counter);
    end;
}
"#;
    let caller = r#"codeunit 50188 "Stateful Caller"
{
    procedure Run(): Integer
    var
        Helper: Codeunit "Stateful Helper";
    begin
        exit(Helper.Next());
    end;
}
"#;
    let result = run(
        &[
            ("/ws/StatefulHelper.al", stateful),
            ("/ws/StatefulCaller.al", caller),
        ],
        "Stateful Caller",
        "Run",
        vec![],
    );
    let Eval::Error(error) = result else {
        panic!("stateful helper must fail closed");
    };
    assert!(
        error.message.contains("requires live BC"),
        "{}",
        error.message
    );
}

#[test]
fn unqualified_call_resolves_only_within_current_object() {
    let caller = r#"codeunit 50185 "Scoped Caller"
{
    procedure Run(): Integer
    begin
        exit(Value());
    end;

    local procedure Value(): Integer
    begin
        exit(1);
    end;
}
"#;
    let collision = r#"codeunit 50186 "Scoped Collision"
{
    procedure Value(): Integer
    begin
        exit(99);
    end;
}
"#;
    let result = run(
        &[
            ("/ws/ScopedCollision.al", collision),
            ("/ws/ScopedCaller.al", caller),
        ],
        "Scoped Caller",
        "Run",
        vec![],
    );
    assert_eq!(ok(result), Value::Integer(1));
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
    assert_eq!(
        ok(r),
        Value::Decimal(dec!(19)),
        "an Integer literal stored into a Decimal field reads back as Decimal"
    );
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
    assert_eq!(ok(r), Value::Decimal(dec!(100)));
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
fn unknown_table_field_fails_instead_of_getting_a_synthetic_id() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure WriteUnknown()
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."Not A Field" := 'invented';
    end;
}
"#;
    let result = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "WriteUnknown",
        vec![],
    );
    let message = error_message(result);
    assert!(message.contains("is not declared"), "got: {message}");
}

#[test]
fn requested_record_trigger_fails_instead_of_running_as_a_noop() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure InsertWithTrigger()
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'X';
        Item.Insert(true);
    end;
}
"#;
    let result = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "InsertWithTrigger",
        vec![],
    );
    let message = error_message(result);
    assert!(
        message.contains("requires live Business Central"),
        "got: {message}"
    );
}

#[test]
fn table_without_primary_key_is_rejected() {
    let table = r#"table 50140 "No Key"
{
    fields
    {
        field(1; Value; Integer) { }
    }
}
"#;
    let cu = r#"codeunit 50141 "No Key Tests"
{
    procedure Touch()
    var
        Rec: Record "No Key";
    begin
        Rec.Init();
    end;
}
"#;
    let result = run(
        &[("/ws/NoKey.al", table), ("/ws/NoKeyTests.al", cu)],
        "No Key Tests",
        "Touch",
        vec![],
    );
    let message = error_message(result);
    assert!(
        message.contains("keys section is missing"),
        "got: {message}"
    );
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
    assert_eq!(ok(run_flow("SumViaCalcFields")), Value::Decimal(dec!(60)));
}

#[test]
fn flowfield_count_on_read() {
    assert_eq!(ok(run_flow("CountOnRead")), Value::Integer(3));
}

#[test]
fn flowfield_filtered_const() {
    assert_eq!(ok(run_flow("FilteredConst")), Value::Decimal(dec!(40)));
}

#[test]
fn flowfield_filtered_expr() {
    assert_eq!(ok(run_flow("FilteredExpr")), Value::Decimal(dec!(50)));
}

#[test]
fn flowfield_max() {
    assert_eq!(ok(run_flow("MaxOnRead")), Value::Decimal(dec!(30)));
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

#[test]
fn list_method_wrong_arity_is_rejected() {
    let cu = r#"codeunit 50300 "List Tests"
{
    procedure BadCount()
    var
        items: List of [Integer];
    begin
        items.Count(1);
    end;
}
"#;
    let result = run(
        &[("/ws/ListTests.al", cu)],
        "List Tests",
        "BadCount",
        vec![],
    );
    let message = error_message(result);
    assert!(message.contains("expects no arguments"), "got: {message}");
}

#[test]
fn compound_assignment_to_record_field_accumulates() {
    // Regression: `Rec.Amount += 5` must store Amount + 5, not the raw RHS.
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure Compound(): Decimal
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'A';
        Item."Unit Price" := 19;
        Item."Unit Price" += 5;
        exit(Item."Unit Price");
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "Compound",
        vec![],
    );
    assert_eq!(ok(r), Value::Decimal(dec!(24)));
}

#[test]
fn statement_position_get_miss_errors() {
    // BC raises "The record does not exist" when a statement-position Get
    // misses; only `if Rec.Get(...) then` yields false.
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure GetMiss()
    var
        Item: Record "Item";
    begin
        Item.Get('NOPE');
    end;
}
"#;
    let result = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "GetMiss",
        vec![],
    );
    let message = error_message(result);
    assert!(message.contains("does not exist"), "got: {message}");
}

#[test]
fn expression_position_get_miss_returns_false() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure GetMissExpr(): Integer
    var
        Item: Record "Item";
    begin
        if Item.Get('NOPE') then
            exit(1);
        exit(0);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "GetMissExpr",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(0));
}

#[test]
fn statement_position_findfirst_miss_errors() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure FindMiss()
    var
        Num: Record "Num";
    begin
        Num.FindFirst();
    end;
}
"#;
    let result = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "FindMiss",
        vec![],
    );
    let message = error_message(result);
    assert!(
        message.contains("no 'Num' record matches"),
        "got: {message}"
    );
}

#[test]
fn expression_position_insert_duplicate_returns_false() {
    // `if Rec.Insert() then` must take the false branch on a duplicate key
    // instead of raising.
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure DupInsertExpr(): Integer
    var
        Item: Record "Item";
    begin
        Item."No." := 'X';
        Item.Insert();
        Item."No." := 'X';
        if Item.Insert() then
            exit(1);
        exit(0);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "DupInsertExpr",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(0));
}

#[test]
fn record_variables_have_independent_filters_and_cursors() {
    // Two record variables of one table share the physical rows but each has
    // its own filter set and iteration cursor (BC semantics).
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure IndependentViews(): Integer
    var
        A: Record "Num";
        B: Record "Num";
        i: Integer;
    begin
        for i := 1 to 5 do begin
            A.Init();
            A."Entry No." := i;
            A.Insert();
        end;
        A.SetRange("Entry No.", 1, 2);
        B.SetRange("Entry No.", 4, 5);
        if A.Count() <> 2 then
            Error('A sees %1 rows after its own filter', A.Count());
        if B.Count() <> 2 then
            Error('B sees %1 rows after its own filter', B.Count());
        A.FindFirst();
        B.FindFirst();
        if A."Entry No." <> 1 then
            Error('A cursor moved by B: %1', A."Entry No.");
        if B."Entry No." <> 4 then
            Error('B cursor is wrong: %1', B."Entry No.");
        if A.Next() <> 1 then
            Error('A cursor lost its position');
        if A."Entry No." <> 2 then
            Error('A next row wrong: %1', A."Entry No.");
        exit(A.Count() + B.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "IndependentViews",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(4));
}

#[test]
fn unset_field_reads_typed_zero_and_matches_zero_filter() {
    // A never-assigned field is the field type's zero value in BC: reading it
    // participates in arithmetic and `SetRange(F, 0)` matches the row.
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure UnsetDefaults(): Decimal
    var
        Num: Record "Num";
    begin
        Num.Init();
        Num."Entry No." := 1;
        Num.Insert();
        Num.SetRange(Amount, 0);
        if Num.Count() <> 1 then
            Error('SetRange(Amount, 0) missed the unset row: %1', Num.Count());
        Num.FindFirst();
        exit(Num.Amount + 1);
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "UnsetDefaults",
        vec![],
    );
    assert_eq!(ok(r), Value::Decimal(dec!(1)));
}

#[test]
fn get_on_code_primary_key_is_caseless() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure CaselessGet(): Boolean
    var
        Item: Record "Item";
    begin
        Item."No." := 'abc';
        Item.Insert();
        exit(Item.Get('ABC'));
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "CaselessGet",
        vec![],
    );
    assert_eq!(ok(r), Value::Boolean(true));
}

#[test]
fn setfilter_placeholder_ten_is_not_corrupted_by_placeholder_one() {
    // `%10` must substitute the tenth value; ascending substitution used to
    // rewrite the `%1` prefix of `%10` first.
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure ManyPlaceholders(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 50 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetFilter("Entry No.", '%1|%2|%3|%4|%5|%6|%7|%8|%9|%10', 1, 2, 3, 4, 5, 6, 7, 8, 9, 42);
        if Num.Count() <> 10 then
            Error('placeholder filter matched %1 rows', Num.Count());
        Num.FindLast();
        exit(Num."Entry No.");
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "ManyPlaceholders",
        vec![],
    );
    assert_eq!(
        ok(r),
        Value::Integer(42),
        "%10 must map to the tenth value (42), not be corrupted into 10"
    );
}

#[test]
fn setfilter_value_containing_percent_one_stays_literal() {
    // Regression: substituted text must never be re-scanned. A `%2` value
    // whose text contains '%1' used to be corrupted by the later `%1` pass
    // (descending replace order), turning 'A%1B' into 'AXB'.
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure PercentLiteralValue(): Text
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'K1';
        Item.Description := 'A%1B';
        Item.Insert();
        Item.Init();
        Item."No." := 'K2';
        Item.Description := 'AXB';
        Item.Insert();
        Item.SetFilter(Description, '%2', 'X', 'A%1B');
        if Item.Count() <> 1 then
            Error('filter matched %1 rows', Item.Count());
        Item.FindFirst();
        exit(Item.Description);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "PercentLiteralValue",
        vec![],
    );
    assert_eq!(
        ok(r),
        Value::Text("A%1B".into()),
        "a substituted value containing '%1' must stay literal, not be rewritten by a later pass"
    );
}

#[test]
fn by_value_record_argument_does_not_share_the_caller_view() {
    // Regression: a by-value record parameter used to keep the caller's view
    // handle (RecordValue is Clone), so the callee's SetRange corrupted the
    // caller's filters. The callee must get its own fresh view.
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure CalleeFilters(Num: Record "Num")
    begin
        Num.SetRange("Entry No.", 9, 9);
        if Num.Count() <> 1 then
            Error('callee view expected 1 row, got %1', Num.Count());
    end;

    procedure ByValueKeepsCallerView(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 10 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetRange("Entry No.", 1, 4);
        CalleeFilters(Num);
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "ByValueKeepsCallerView",
        vec![],
    );
    assert_eq!(
        ok(r),
        Value::Integer(4),
        "the caller's SetRange(1,4) must survive a by-value call that sets its own filter"
    );
}

#[test]
fn by_value_record_argument_carries_the_callers_buffer() {
    // Regression: giving the callee a *fresh* view fixed the shared-filter bug
    // but handed it an EMPTY buffer. BC passes a record by value as a copy, so
    // unsaved field assignments made by the caller (no Insert) are visible in
    // the callee — while its filters/cursor stay independent.
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure ReadCopiedBuffer(Num: Record "Num"): Decimal
    begin
        // The callee's own filter must not leak back to the caller.
        Num.SetRange("Entry No.", 9, 9);
        exit(Num."Entry No." + Num.Amount);
    end;

    procedure ByValuePassesBuffer(): Integer
    var
        Num: Record "Num";
        Copied: Decimal;
        i: Integer;
    begin
        for i := 1 to 10 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetRange("Entry No.", 1, 4);
        // Buffer values that were never written to the table.
        Num.Init();
        Num."Entry No." := 42;
        Num.Amount := 8;
        Copied := ReadCopiedBuffer(Num);
        if Copied <> 50 then
            Error('callee saw buffer %1, expected 50', Copied);
        if Num."Entry No." <> 42 then
            Error('the caller buffer must be untouched, got %1', Num."Entry No.");
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "ByValuePassesBuffer",
        vec![],
    );
    assert_eq!(
        ok(r),
        Value::Integer(4),
        "the by-value copy carries the caller's buffer, and its SetRange must not touch the \
         caller's filters"
    );
}

#[test]
fn deleteall_removes_only_filtered_rows() {
    let cu = r#"codeunit 50103 "Num Tests"
{
    procedure DeleteFiltered(): Integer
    var
        Num: Record "Num";
        i: Integer;
    begin
        for i := 1 to 10 do begin
            Num.Init();
            Num."Entry No." := i;
            Num.Insert();
        end;
        Num.SetRange("Entry No.", 1, 4);
        Num.DeleteAll();
        Num.Reset();
        exit(Num.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Num.al", NUM_TABLE), ("/ws/NumTests.al", cu)],
        "Num Tests",
        "DeleteFiltered",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(6));
}

const FLAGGED_TABLE: &str = r#"table 50130 "Flagged"
{
    fields
    {
        field(1; "Id"; Integer) { }
        field(2; Flag; Boolean) { }
    }
    keys
    {
        key(PK; "Id") { }
    }
}
"#;

const FLAG_HDR_TABLE: &str = r#"table 50131 "Flag Hdr"
{
    fields
    {
        field(1; "Id"; Integer) { }
        field(2; FlagCount; Integer)
        {
            FieldClass = FlowField;
            CalcFormula = Count("Flagged" WHERE (Flag = CONST(true)));
        }
    }
    keys
    {
        key(PK; "Id") { }
    }
}
"#;

#[test]
fn flowfield_boolean_const_matches_boolean_cells() {
    // Regression: `WHERE(Flag = CONST(true))` used to compare Text("true")
    // against Boolean cells and match nothing.
    let cu = r#"codeunit 50132 "Flag Tests"
{
    procedure CountFlagged(): Integer
    var
        Row: Record "Flagged";
        Hdr: Record "Flag Hdr";
    begin
        Row.Init(); Row."Id" := 1; Row.Flag := true; Row.Insert();
        Row.Init(); Row."Id" := 2; Row.Flag := false; Row.Insert();
        Row.Init(); Row."Id" := 3; Row.Flag := true; Row.Insert();
        exit(Hdr.FlagCount);
    end;
}
"#;
    let r = run(
        &[
            ("/ws/Flagged.al", FLAGGED_TABLE),
            ("/ws/FlagHdr.al", FLAG_HDR_TABLE),
            ("/ws/FlagTests.al", cu),
        ],
        "Flag Tests",
        "CountFlagged",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(2));
}

#[test]
fn temporary_record_uses_the_table_name_without_the_temporary_keyword() {
    let cu = r#"codeunit 50133 "Temp Name Tests"
{
    procedure CountRows(): Integer
    var
        TempItem: Record "Item" temporary;
    begin
        TempItem.Init();
        TempItem."No." := 'A';
        TempItem.Insert();
        exit(TempItem.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/TempName.al", cu)],
        "Temp Name Tests",
        "CountRows",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(1));
}

#[test]
fn each_temporary_record_variable_has_its_own_rows() {
    // BC gives every temporary record variable a private in-memory table: rows
    // in one are invisible to a second temporary variable over the same table
    // and to the persistent table.
    let cu = r#"codeunit 50134 "Temp Isolation Tests"
{
    procedure Expected(): Integer
    var
        TempA: Record "Item" temporary;
        TempB: Record "Item" temporary;
        Persistent: Record "Item";
    begin
        TempA.Init(); TempA."No." := 'A'; TempA.Insert();
        TempA.Init(); TempA."No." := 'B'; TempA.Insert();
        TempB.Init(); TempB."No." := 'C'; TempB.Insert();
        Persistent.Init(); Persistent."No." := 'D'; Persistent.Insert();
        exit(TempA.Count() * 100 + TempB.Count() * 10 + Persistent.Count());
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/TempIsolation.al", cu)],
        "Temp Isolation Tests",
        "Expected",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(211));
}

const STATE_ENUM: &str = r#"enum 50110 "My State"
{
    value(0; Open) { }
    value(1; Released) { }
    value(2; Closed) { }
}
"#;

/// A table with an Enum field and an Option field, neither with an InitValue.
const TICKET_TABLE: &str = r#"table 50111 "Ticket"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Status; Enum "My State") { }
        field(3; Priority; Option)
        {
            OptionMembers = Low,High;
        }
        field(4; "Posting Date"; Date) { }
    }
    keys
    {
        key(PK; "No.") { }
    }
}
"#;

#[test]
fn unassigned_enum_field_reads_as_its_ordinal_zero_member() {
    // BC zero-initialises an Enum field, so a row inserted without assigning
    // Status is Open, matches SetRange(Status, Status::Open), and compares
    // equal to Status::Open.
    let cu = r#"codeunit 50135 "Enum Zero Tests"
{
    procedure OpenRowsAndEquality(): Integer
    var
        Ticket: Record "Ticket";
        Found: Integer;
    begin
        Ticket.Init();
        Ticket."No." := 'A';
        Ticket.Insert();
        Ticket.Reset();
        Ticket.SetRange(Status, "My State"::Open);
        Found := Ticket.Count() * 10;
        Ticket.Reset();
        Ticket.FindFirst();
        if Ticket.Status = "My State"::Open then
            Found := Found + 1;
        exit(Found);
    end;
}
"#;
    let r = run(
        &[
            ("/ws/MyState.al", STATE_ENUM),
            ("/ws/Ticket.al", TICKET_TABLE),
            ("/ws/EnumZero.al", cu),
        ],
        "Enum Zero Tests",
        "OpenRowsAndEquality",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(11));
}

#[test]
fn unassigned_option_field_reads_as_its_first_member() {
    let cu = r#"codeunit 50136 "Option Zero Tests"
{
    procedure PriorityOrdinal(): Integer
    var
        Ticket: Record "Ticket";
    begin
        Ticket.Init();
        Ticket."No." := 'A';
        Ticket.Insert();
        Ticket.Reset();
        Ticket.FindFirst();
        exit(Ticket.Priority + 0);
    end;
}
"#;
    let r = run(
        &[
            ("/ws/MyState.al", STATE_ENUM),
            ("/ws/Ticket.al", TICKET_TABLE),
            ("/ws/OptionZero.al", cu),
        ],
        "Option Zero Tests",
        "PriorityOrdinal",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(0));
}

#[test]
fn setfilter_accepts_date_placeholders() {
    // The range a Date SetFilter selects must be the range SetRange selects.
    let cu = r#"codeunit 50137 "Date Filter Tests"
{
    procedure FilterAndRange(): Integer
    var
        Ticket: Record "Ticket";
        Filtered: Integer;
    begin
        Ticket.Init(); Ticket."No." := 'A'; Ticket."Posting Date" := 20240101D; Ticket.Insert();
        Ticket.Init(); Ticket."No." := 'B'; Ticket."Posting Date" := 20240615D; Ticket.Insert();
        Ticket.Init(); Ticket."No." := 'C'; Ticket."Posting Date" := 20241231D; Ticket.Insert();
        Ticket.Reset();
        Ticket.SetFilter("Posting Date", '%1..%2', 20240101D, 20240630D);
        Filtered := Ticket.Count();
        Ticket.Reset();
        Ticket.SetRange("Posting Date", 20240101D, 20240630D);
        exit(Filtered * 10 + Ticket.Count());
    end;
}
"#;
    let r = run(
        &[
            ("/ws/MyState.al", STATE_ENUM),
            ("/ws/Ticket.al", TICKET_TABLE),
            ("/ws/DateFilter.al", cu),
        ],
        "Date Filter Tests",
        "FilterAndRange",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(22));
}

#[test]
fn setfilter_accepts_option_placeholders() {
    // BC filters an option field by ordinal, so '%1|%2' over two enum members
    // selects exactly the rows holding those two ordinals.
    let cu = r#"codeunit 50138 "Option Filter Tests"
{
    procedure OpenOrReleased(): Integer
    var
        Ticket: Record "Ticket";
    begin
        Ticket.Init(); Ticket."No." := 'A'; Ticket.Status := "My State"::Open; Ticket.Insert();
        Ticket.Init(); Ticket."No." := 'B'; Ticket.Status := "My State"::Released; Ticket.Insert();
        Ticket.Init(); Ticket."No." := 'C'; Ticket.Status := "My State"::Closed; Ticket.Insert();
        Ticket.Reset();
        Ticket.SetFilter(Status, '%1|%2', "My State"::Open, "My State"::Released);
        exit(Ticket.Count());
    end;
}
"#;
    let r = run(
        &[
            ("/ws/MyState.al", STATE_ENUM),
            ("/ws/Ticket.al", TICKET_TABLE),
            ("/ws/OptionFilter.al", cu),
        ],
        "Option Filter Tests",
        "OpenOrReleased",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(2));
}

#[test]
fn assigning_past_a_code_field_capacity_is_an_error() {
    // "No." is Code[20]. BC traps the overflow at the assignment.
    let cu = r#"codeunit 50139 "Field Length Tests"
{
    procedure Overflow(): Integer
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'THIS-CODE-IS-WAY-LONGER-THAN-TWENTY';
        exit(1);
    end;
}
"#;
    let message = error_message(run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/FieldLength.al", cu)],
        "Field Length Tests",
        "Overflow",
        vec![],
    ));
    assert!(
        message.contains("35") && message.contains("20"),
        "expected a length overflow naming both lengths, got: {message}"
    );
}

#[test]
fn assigning_past_a_local_text_capacity_is_an_error() {
    let cu = r#"codeunit 50140 "Local Length Tests"
{
    procedure Overflow(): Integer
    var
        Short: Text[5];
    begin
        Short := 'abcdefgh';
        exit(1);
    end;
}
"#;
    let message = error_message(run(
        &[("/ws/LocalLength.al", cu)],
        "Local Length Tests",
        "Overflow",
        vec![],
    ));
    assert!(
        message.contains('8') && message.contains('5'),
        "expected a length overflow naming both lengths, got: {message}"
    );
}

#[test]
fn a_code_value_is_trimmed_and_fits_its_capacity() {
    // A Code variable's length is the text without leading or trailing spaces,
    // so '  ABCDE  ' is five characters and fits Code[5].
    let cu = r#"codeunit 50141 "Code Trim Tests"
{
    procedure Trimmed(): Text
    var
        Short: Code[5];
    begin
        Short := '  abcde  ';
        exit(Short);
    end;
}
"#;
    let r = run(
        &[("/ws/CodeTrim.al", cu)],
        "Code Trim Tests",
        "Trimmed",
        vec![],
    );
    assert_eq!(ok(r), Value::Code("ABCDE".to_string()));
}

#[test]
fn insert_without_a_primary_key_assignment_stores_the_blank_key() {
    // BC inserts a row whose Code key is '' and only rejects the second such
    // insert as a duplicate.
    let cu = r#"codeunit 50142 "Blank Key Tests"
{
    procedure BlankThenDuplicate(): Integer
    var
        Item: Record "Item";
        Found: Integer;
    begin
        Item.Init();
        Item.Description := 'blank key row';
        Item.Insert();
        Item.Reset();
        Found := Item.Count() * 10;
        Item.Init();
        if not Item.Insert(false) then
            Found := Found + 1;
        exit(Found);
    end;
}
"#;
    let r = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/BlankKey.al", cu)],
        "Blank Key Tests",
        "BlankThenDuplicate",
        vec![],
    );
    assert_eq!(ok(r), Value::Integer(11));
}

const AMT_LINE_TABLE: &str = r#"table 50143 "Amt Line"
{
    fields
    {
        field(1; "Id"; Integer) { }
        field(2; Amount; Decimal) { }
    }
    keys
    {
        key(PK; "Id") { }
    }
}
"#;

const AMT_HDR_TABLE: &str = r#"table 50144 "Amt Hdr"
{
    fields
    {
        field(1; "Id"; Integer) { }
        field(2; MinAmount; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Min("Amt Line".Amount);
        }
    }
    keys
    {
        key(PK; "Id") { }
    }
}
"#;

#[test]
fn flowfield_min_counts_rows_that_never_assigned_the_field() {
    // A row inserted without assigning Amount holds the field's zero, so Min
    // over rows with Amount = 5 and Amount unassigned is 0, not 5.
    let cu = r#"codeunit 50145 "Min Zero Tests"
{
    procedure MinAmount(): Decimal
    var
        Line: Record "Amt Line";
        Hdr: Record "Amt Hdr";
    begin
        Line.Init(); Line."Id" := 1; Line.Amount := 5; Line.Insert();
        Line.Init(); Line."Id" := 2; Line.Insert();
        exit(Hdr.MinAmount);
    end;
}
"#;
    let r = run(
        &[
            ("/ws/AmtLine.al", AMT_LINE_TABLE),
            ("/ws/AmtHdr.al", AMT_HDR_TABLE),
            ("/ws/MinZero.al", cu),
        ],
        "Min Zero Tests",
        "MinAmount",
        vec![],
    );
    assert_eq!(ok(r), Value::Decimal(dec!(0)));
}

const EXPECTED_ERROR_CODEUNIT: &str = r#"codeunit 50146 "Expected Error Tests"
{
    var
        Assert: Codeunit "Library Assert";

    procedure Boom()
    begin
        Error('The order must have a customer');
    end;

    procedure Matching(): Integer
    begin
        asserterror Boom();
        Assert.ExpectedError('must have a customer');
        exit(1);
    end;

    procedure Mismatched(): Integer
    begin
        asserterror Boom();
        Assert.ExpectedError('a completely different message');
        exit(1);
    end;

    procedure NoErrorAtAll(): Integer
    begin
        Assert.ExpectedError('anything');
        exit(1);
    end;
}
"#;

#[test]
fn assert_expected_error_matches_a_substring_of_the_caught_error() {
    let files = [("/ws/ExpectedError.al", EXPECTED_ERROR_CODEUNIT)];
    assert_eq!(
        ok(run(&files, "Expected Error Tests", "Matching", vec![])),
        Value::Integer(1)
    );

    let mismatch = error_message(run(&files, "Expected Error Tests", "Mismatched", vec![]));
    assert!(
        mismatch.contains("Assert.ExpectedError failed")
            && mismatch.contains("The order must have a customer"),
        "expected the BC failure message naming both texts, got: {mismatch}"
    );

    let none = error_message(run(&files, "Expected Error Tests", "NoErrorAtAll", vec![]));
    assert!(
        none.contains("has not been thrown") || none.contains("Assert.ExpectedError failed"),
        "expected a thrown-nothing failure, got: {none}"
    );
}

const POINT_LEDGER: &str = r#"table 50150 "Point Ledger"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Member; Code[20]) { }
        field(3; Points; Decimal) { }
    }
    keys
    {
        key(PK; "Entry No.") { Clustered = true; }
    }
}
"#;

const POINT_CALC: &str = r#"codeunit 50151 "Point Calc"
{
    procedure Add(EntryNo: Integer; MemberCode: Code[20]; Amount: Decimal)
    var
        Ledger: Record "Point Ledger";
    begin
        Ledger.Init();
        Ledger."Entry No." := EntryNo;
        Ledger.Member := MemberCode;
        Ledger.Points := Amount;
        Ledger.Insert();
    end;

    procedure BalanceOfA(): Decimal
    var
        Ledger: Record "Point Ledger";
    begin
        Add(1, 'A', 10);
        Add(2, 'B', 5);
        Add(3, 'A', 32.5);
        Ledger.SetRange(Member, 'A');
        Ledger.CalcSums(Points);
        exit(Ledger.Points);
    end;

    procedure NothingMatches(): Decimal
    var
        Ledger: Record "Point Ledger";
    begin
        Add(1, 'A', 10);
        Ledger.SetRange(Member, 'Z');
        Ledger.CalcSums(Points);
        exit(Ledger.Points);
    end;
}
"#;

/// CalcSums was unsupported, so any test using it was routed to live BC.
#[test]
fn calcsums_totals_the_filtered_rows() {
    let run_calc = |proc: &str| {
        run(
            &[("/ws/Ledger.al", POINT_LEDGER), ("/ws/Calc.al", POINT_CALC)],
            "Point Calc",
            proc,
            vec![],
        )
    };
    assert_eq!(ok(run_calc("BalanceOfA")), Value::Decimal(dec!(42.5)));
    assert_eq!(ok(run_calc("NothingMatches")), Value::Decimal(dec!(0)));
}

/// `case true of Points >= 1000:` evaluated only `Points` of each label and
/// compared it with `true`, so every value fell through to `else`.
#[test]
fn case_true_of_compares_each_label_expression() {
    let src = r#"codeunit 50160 Probe
{
    procedure Tier(Points: Decimal): Text
    begin
        case true of
            Points >= 1000:
                exit('GOLD');
            Points >= 100:
                exit('SILVER');
            else
                exit('NONE');
        end;
    end;
}
"#;
    let tier = |points: Value| ok(run(&[("/ws/P.al", src)], "Probe", "Tier", vec![points]));
    assert_eq!(tier(Value::Decimal(dec!(1000))), Value::Text("GOLD".into()));
    assert_eq!(
        tier(Value::Decimal(dec!(150))),
        Value::Text("SILVER".into())
    );
    assert_eq!(tier(Value::Integer(99)), Value::Text("NONE".into()));
}

const BUILTIN_PROBE: &str = r#"codeunit 50170 "Builtin Probe"
{
    procedure DelStrOf(): Text
    begin
        exit(DelStr('ABCDEF', 2, 3) + '|' + DelStr('ABCDEF', 4));
    end;

    procedure Extremes(): Decimal
    begin
        exit(Maximum(3, 7.5) - Minimum(4, -2));
    end;

    procedure ArrayLength(): Integer
    var
        Slots: array[4] of Integer;
    begin
        exit(ArrayLen(Slots));
    end;

    procedure ArrayElements(): Integer
    var
        Slots: array[4] of Integer;
        i: Integer;
        Total: Integer;
    begin
        for i := 1 to ArrayLen(Slots) do
            Slots[i] := i * 10;
        Slots[i - 1] += 5;
        for i := 1 to 4 do
            Total += Slots[i];
        exit(Total + Slots[i - 2 + 1]);
    end;

    procedure ArrayOutOfBounds(): Integer
    var
        Slots: array[2] of Integer;
    begin
        exit(Slots[3]);
    end;

    procedure TextCharacters(): Text
    var
        Word: Text;
        Tag: Code[10];
    begin
        Word := 'cat';
        Word[1] := 'b';
        Tag := 'AB';
        Tag[2] := 'z';
        exit(Word + Format(Word[3]) + Tag);
    end;

    procedure CalcDateOf(Formula: Text; Day: Integer; Month: Integer; Year: Integer): Date
    begin
        exit(CalcDate(Formula, DMY2Date(Day, Month, Year)));
    end;

    procedure Dwy(Day: Integer; Month: Integer; Year: Integer; What: Integer): Integer
    begin
        exit(Date2DWY(DMY2Date(Day, Month, Year), What));
    end;

    procedure EvaluateInteger(Input: Text): Integer
    var
        Parsed: Integer;
    begin
        Parsed := -1;
        if not Evaluate(Parsed, Input) then
            exit(-99);
        exit(Parsed);
    end;

    procedure EvaluateAsStatement()
    var
        Parsed: Decimal;
    begin
        Evaluate(Parsed, 'not a number');
    end;

    procedure ListInsert(): Text
    var
        Names: List of [Text];
        Name: Text;
        Joined: Text;
    begin
        Names.Add('a');
        Names.Add('c');
        Names.Insert(2, 'b');
        Names.Insert(4, 'd');
        foreach Name in Names do
            Joined += Name;
        exit(Joined);
    end;

    procedure DictGet(): Text
    var
        Prices: Dictionary of [Code[20], Decimal];
        Price: Decimal;
        Found: Text;
    begin
        Prices.Add('A', 12.5);
        if Prices.Get('A', Price) then
            Found := Format(Price);
        if not Prices.Get('Z', Price) then
            Found += '|missing';
        exit(Found);
    end;
}
"#;

fn probe(proc: &str, args: Vec<Value>) -> Eval {
    run(
        &[("/ws/Probe.al", BUILTIN_PROBE)],
        "Builtin Probe",
        proc,
        args,
    )
}

/// DelStr, Maximum/Minimum and ArrayLen were unsupported, so any test using
/// them was routed to live BC.
#[test]
fn text_and_numeric_builtins_run_locally() {
    assert_eq!(ok(probe("DelStrOf", vec![])), Value::Text("AEF|ABC".into()));
    assert_eq!(ok(probe("Extremes", vec![])), Value::Decimal(dec!(9.5)));
    assert_eq!(ok(probe("ArrayLength", vec![])), Value::Integer(4));
}

/// Array variables were never bound, so `Slots[i]` failed as an unbound
/// identifier.
#[test]
fn array_elements_and_text_characters_read_and_write() {
    // 10 + 20 + 35 + 40 = 105, plus Slots[3] = 35.
    assert_eq!(ok(probe("ArrayElements", vec![])), Value::Integer(140));
    let out_of_bounds = error_message(probe("ArrayOutOfBounds", vec![]));
    assert!(
        out_of_bounds.contains("index 3 is outside"),
        "{out_of_bounds}"
    );
    assert_eq!(
        ok(probe("TextCharacters", vec![])),
        Value::Text("battAZ".into())
    );
}

#[test]
fn calcdate_applies_each_term_and_clamps_month_ends() {
    use crate::interpreter::value::al_days_from_ymd;
    let calc = |formula: &str, (d, m, y): (i64, i64, i64)| {
        ok(probe(
            "CalcDateOf",
            vec![
                Value::Text(formula.into()),
                Value::Integer(d),
                Value::Integer(m),
                Value::Integer(y),
            ],
        ))
    };
    let date = |d, m, y| Value::Date(al_days_from_ymd(y, m, d));
    assert_eq!(calc("<+1M>", (31, 1, 2026)), date(28, 2, 2026));
    assert_eq!(calc("<CM>", (10, 2, 2028)), date(29, 2, 2028));
    assert_eq!(calc("<-CM>", (10, 2, 2026)), date(1, 2, 2026));
    assert_eq!(calc("<CM+1D>", (10, 12, 2026)), date(1, 1, 2027));
    assert_eq!(calc("<-1W>", (7, 1, 2026)), date(31, 12, 2025));
    assert_eq!(calc("<CQ>", (5, 8, 2026)), date(30, 9, 2026));
    assert_eq!(calc("<-CY+1Y>", (5, 8, 2026)), date(1, 1, 2027));
    // 24 Sep 2026 is a Thursday: the week ends on Sunday the 27th.
    assert_eq!(calc("<CW>", (24, 9, 2026)), date(27, 9, 2026));
    assert!(error_message(calc_err("<1X>")).contains("unsupported unit"));
}

fn calc_err(formula: &str) -> Eval {
    probe(
        "CalcDateOf",
        vec![
            Value::Text(formula.into()),
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(2026),
        ],
    )
}

#[test]
fn date2dwy_gives_weekday_iso_week_and_its_year() {
    let dwy = |d, m, y, what| {
        ok(probe(
            "Dwy",
            vec![
                Value::Integer(d),
                Value::Integer(m),
                Value::Integer(y),
                Value::Integer(what),
            ],
        ))
    };
    // Thursday 24 Sep 2026, ISO week 39.
    assert_eq!(dwy(24, 9, 2026, 1), Value::Integer(4));
    assert_eq!(dwy(24, 9, 2026, 2), Value::Integer(39));
    // Friday 1 Jan 2027 belongs to week 53 of 2026.
    assert_eq!(dwy(1, 1, 2027, 1), Value::Integer(5));
    assert_eq!(dwy(1, 1, 2027, 2), Value::Integer(53));
    assert_eq!(dwy(1, 1, 2027, 3), Value::Integer(2026));
    // Monday 29 Dec 2025 is in week 1 of 2026.
    assert_eq!(dwy(29, 12, 2025, 2), Value::Integer(1));
    assert_eq!(dwy(29, 12, 2025, 3), Value::Integer(2026));
}

/// Evaluate writes the parsed value to its var argument, answers false in an
/// expression and raises as a statement, as BC does.
#[test]
fn evaluate_writes_back_or_fails_by_position() {
    let evaluate = |input: &str| ok(probe("EvaluateInteger", vec![Value::Text(input.into())]));
    assert_eq!(evaluate(" 42 "), Value::Integer(42));
    assert_eq!(evaluate("4x2"), Value::Integer(-99));
    let raised = error_message(probe("EvaluateAsStatement", vec![]));
    assert!(
        raised.contains("'not a number' is not a valid Decimal"),
        "{raised}"
    );
}

#[test]
fn list_insert_and_dictionary_get_with_var_run_locally() {
    assert_eq!(ok(probe("ListInsert", vec![])), Value::Text("abcd".into()));
    assert_eq!(
        ok(probe("DictGet", vec![])),
        Value::Text("12.5|missing".into())
    );
}

const POINT_RESET: &str = r#"codeunit 50152 "Point Reset"
{
    procedure ZeroMemberA(): Decimal
    var
        Calc: Codeunit "Point Calc";
        Ledger: Record "Point Ledger";
    begin
        Calc.Add(1, 'A', 10);
        Calc.Add(2, 'B', 5);
        Calc.Add(3, 'A', 7);
        Ledger.SetRange(Member, 'A');
        Ledger.ModifyAll(Points, 0);
        Ledger.Reset();
        Ledger.CalcSums(Points);
        exit(Ledger.Points);
    end;

    procedure ModifyKey()
    var
        Ledger: Record "Point Ledger";
    begin
        Ledger.ModifyAll("Entry No.", 9);
    end;
}
"#;

/// ModifyAll was unsupported, so any test using it was routed to live BC.
#[test]
fn modifyall_sets_the_field_on_filtered_rows_only() {
    let files = [
        ("/ws/Ledger.al", POINT_LEDGER),
        ("/ws/Calc.al", POINT_CALC),
        ("/ws/Reset.al", POINT_RESET),
    ];
    assert_eq!(
        ok(run(&files, "Point Reset", "ZeroMemberA", vec![])),
        Value::Decimal(dec!(5))
    );
    let refused = error_message(run(&files, "Point Reset", "ModifyKey", vec![]));
    assert!(refused.contains("part of the primary key"), "{refused}");
}
