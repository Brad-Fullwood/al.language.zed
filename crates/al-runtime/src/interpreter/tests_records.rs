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
