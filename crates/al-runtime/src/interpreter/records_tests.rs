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

/// A codeunit with globals called through a variable failed closed
/// ("stateful codeunit requires live BC"): each variable is an instance
/// whose globals persist between its calls, as in BC.
#[test]
fn codeunit_variables_keep_their_own_globals() {
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
    let single = r#"codeunit 50189 "Session Counter"
{
    SingleInstance = true;

    var
        Counter: Integer;

    procedure Next(): Integer
    begin
        Counter += 1;
        exit(Counter);
    end;
}
"#;
    let caller = r#"codeunit 50188 "Stateful Caller"
{
    procedure Run(): Text
    var
        First: Codeunit "Stateful Helper";
        Second: Codeunit "Stateful Helper";
        Copy: Codeunit "Stateful Helper";
        SessionA: Codeunit "Session Counter";
        SessionB: Codeunit "Session Counter";
    begin
        First.Next();
        First.Next();
        Second.Next();
        Copy := First;
        SessionA.Next();
        exit(Format(First.Next()) + Format(Second.Next()) + Format(Copy.Next()) + Format(SessionB.Next()));
    end;
}
"#;
    let result = run(
        &[
            ("/ws/StatefulHelper.al", stateful),
            ("/ws/SessionCounter.al", single),
            ("/ws/StatefulCaller.al", caller),
        ],
        "Stateful Caller",
        "Run",
        vec![],
    );
    // First: 3; Second: 2; Copy shares First: 4; the SingleInstance
    // codeunit is one instance whichever variable calls it: 2.
    assert_eq!(ok(result), Value::Text("3242".into()));
}

/// A label is a constant, so a helper codeunit whose only globals are labels
/// runs from another codeunit with its labels bound.
#[test]
fn label_only_helper_codeunit_runs_from_another_codeunit() {
    let helper = r#"codeunit 50183 "Label Helper"
{
    var
        HelloLbl: Label 'Hello %1';

    procedure Hello(Name: Text): Text
    begin
        exit(StrSubstNo(HelloLbl, Name));
    end;
}
"#;
    let caller = r#"codeunit 50184 "Label Caller"
{
    procedure Run(): Text
    var
        Helper: Codeunit "Label Helper";
    begin
        exit(Helper.Hello('Ann'));
    end;
}
"#;
    let result = run(
        &[
            ("/ws/LabelHelper.al", helper),
            ("/ws/LabelCaller.al", caller),
        ],
        "Label Caller",
        "Run",
        vec![],
    );
    assert_eq!(ok(result), Value::Text("Hello Ann".into()));
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

/// Insert(true) runs the table's OnInsert trigger; a table that declares
/// none inserts exactly as Insert() does.
#[test]
fn insert_true_without_an_oninsert_trigger_inserts() {
    let cu = r#"codeunit 50101 "Item Tests"
{
    procedure InsertWithTrigger(): Boolean
    var
        Item: Record "Item";
    begin
        Item.Init();
        Item."No." := 'X';
        Item.Insert(true);
        exit(Item.Get('X'));
    end;
}
"#;
    let result = run(
        &[("/ws/Item.al", ITEM_TABLE), ("/ws/ItemTests.al", cu)],
        "Item Tests",
        "InsertWithTrigger",
        vec![],
    );
    assert_eq!(ok(result), Value::Boolean(true));
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

const SHARED_COLLECTIONS: &str = r#"codeunit 50301 "Shared Collections"
{
    procedure AssignedListIsShared(): Integer
    var
        First: List of [Integer];
        Second: List of [Integer];
    begin
        First.Add(1);
        Second := First;
        Second.Add(2);
        exit(First.Count());
    end;

    procedure ValueParameterShares(): Integer
    var
        Items: List of [Integer];
    begin
        AddTwo(Items);
        exit(Items.Count());
    end;

    local procedure AddTwo(Items: List of [Integer])
    begin
        Items.Add(1);
        Items.Add(2);
    end;

    procedure GetRangeCopies(): Text
    var
        First: List of [Integer];
        Copy: List of [Integer];
    begin
        First.AddRange(1, 2, 3);
        Copy := First.GetRange(1, First.Count());
        Copy.Add(4);
        exit(Format(First.Count()) + '/' + Format(Copy.Count()));
    end;

    procedure EmptyGetRange(): Integer
    var
        First: List of [Integer];
        Copy: List of [Integer];
    begin
        Copy := First.GetRange(1, First.Count());
        exit(Copy.Count());
    end;

    procedure RangeMethods(): Text
    var
        Items: List of [Text];
        Other: List of [Text];
        Item: Text;
        Result: Text;
    begin
        Items.AddRange('a', 'b', 'c', 'b');
        Other.Add('z');
        Items.AddRange(Other);
        Items.AddRange(Items);
        Items.RemoveRange(2, 5);
        Items.Reverse();
        foreach Item in Items do
            Result += Item;
        exit(Result + Format(Items.LastIndexOf('b')));
    end;

    procedure DictionaryIsShared(): Integer
    var
        First: Dictionary of [Code[10], Integer];
        Second: Dictionary of [Code[10], Integer];
    begin
        First.Add('A', 1);
        Second := First;
        Second.Add('B', 2);
        exit(First.Count());
    end;

    procedure KeysKeepTypeAndOrder(): Integer
    var
        Totals: Dictionary of [Integer, Integer];
        Key: Integer;
        Result: Integer;
    begin
        Totals.Add(10, 1);
        Totals.Add(2, 1);
        Totals.Add(7, 1);
        Totals.Remove(2);
        Totals.Add(2, 1);
        foreach Key in Totals.Keys() do
            Result := Result * 100 + Key;
        exit(Result);
    end;
}
"#;

/// A List or Dictionary was copied on assignment and by-value parameter
/// passing, where BC shares one instance: both are reference types.
#[test]
fn lists_and_dictionaries_are_reference_types() {
    let probe = |proc: &str| {
        ok(run(
            &[("/ws/Shared.al", SHARED_COLLECTIONS)],
            "Shared Collections",
            proc,
            vec![],
        ))
    };
    assert_eq!(probe("AssignedListIsShared"), Value::Integer(2));
    assert_eq!(probe("ValueParameterShares"), Value::Integer(2));
    assert_eq!(probe("GetRangeCopies"), Value::Text("3/4".into()));
    assert_eq!(probe("EmptyGetRange"), Value::Integer(0));
    assert_eq!(probe("DictionaryIsShared"), Value::Integer(2));
}

/// AddRange, GetRange, RemoveRange, Reverse and LastIndexOf, and Dictionary
/// keys that keep their type and insertion order.
#[test]
fn list_range_methods_and_typed_dictionary_keys() {
    let probe = |proc: &str| {
        ok(run(
            &[("/ws/Shared.al", SHARED_COLLECTIONS)],
            "Shared Collections",
            proc,
            vec![],
        ))
    };
    // a b c b z a b c b z -> remove 5 from 2 -> a b c b z -> reversed.
    assert_eq!(probe("RangeMethods"), Value::Text("zbcba4".into()));
    assert_eq!(probe("KeysKeepTypeAndOrder"), Value::Integer(100702));
}

const SEPARATE_DEFAULTS: &str = r#"codeunit 50305 "Separate Defaults"
{
    var
        GA, GB: List of [Integer];

    procedure MultiNameLocalLists(): Integer
    var
        A, B: List of [Integer];
    begin
        A.Add(1);
        exit(B.Count());
    end;

    procedure MultiNameLocalDicts(): Integer
    var
        A, B: Dictionary of [Integer, Integer];
    begin
        A.Add(1, 1);
        exit(B.Count());
    end;

    procedure MultiNameGlobalLists(): Integer
    begin
        GA.Add(1);
        exit(GB.Count());
    end;

    procedure MultiNameJson(): Text
    var
        A, B: JsonObject;
        T: Text;
    begin
        A.Add('x', 1);
        B.WriteTo(T);
        exit(T);
    end;

    procedure ArrayOfJson(): Text
    var
        A: array[2] of JsonObject;
        T: Text;
    begin
        A[1].Add('x', 1);
        A[2].WriteTo(T);
        exit(T);
    end;
}
"#;

#[test]
fn each_declared_name_and_array_element_gets_its_own_list_dictionary_or_json_value() {
    let probe = |proc: &str| {
        ok(run(
            &[("/ws/SeparateDefaults.al", SEPARATE_DEFAULTS)],
            "Separate Defaults",
            proc,
            vec![],
        ))
    };
    assert_eq!(probe("MultiNameLocalLists"), Value::Integer(0));
    assert_eq!(probe("MultiNameLocalDicts"), Value::Integer(0));
    assert_eq!(probe("MultiNameGlobalLists"), Value::Integer(0));
    assert_eq!(probe("MultiNameJson"), Value::Text("{}".into()));
    assert_eq!(probe("ArrayOfJson"), Value::Text("{}".into()));
}

const SHARED_TEXTBUILDERS: &str = r#"codeunit 50306 "Shared TextBuilders"
{
    procedure TextBuilderAssigned(): Text
    var
        A: TextBuilder;
        B: TextBuilder;
    begin
        A.Append('x');
        B := A;
        B.Append('y');
        exit(A.ToText());
    end;

    procedure TextBuilderByValue(): Text
    var
        A: TextBuilder;
    begin
        A.Append('x');
        AppendY(A);
        exit(A.ToText());
    end;

    local procedure AppendY(B: TextBuilder)
    begin
        B.Append('y');
    end;

    procedure TextBuilderMultiName(): Text
    var
        A, B: TextBuilder;
    begin
        A.Append('x');
        exit(B.ToText());
    end;
}
"#;

/// A TextBuilder is a reference type: assigning it, or passing it without
/// `var`, shares one builder. Each declared name still gets its own.
#[test]
fn textbuilders_are_shared_by_assignment_and_by_value_parameters() {
    let probe = |proc: &str| {
        ok(run(
            &[("/ws/SharedTextBuilders.al", SHARED_TEXTBUILDERS)],
            "Shared TextBuilders",
            proc,
            vec![],
        ))
    };
    assert_eq!(probe("TextBuilderAssigned"), Value::Text("xy".into()));
    assert_eq!(probe("TextBuilderByValue"), Value::Text("xy".into()));
    assert_eq!(probe("TextBuilderMultiName"), Value::Text(String::new()));
}

const TYPED_DICTIONARY_KEYS: &str = r#"codeunit 50307 "Typed Dictionary Keys"
{
    procedure CodeKeyFromText(): Text
    var
        D: Dictionary of [Code[20], Integer];
        K: Code[20];
    begin
        D.Add('abc', 1);
        if not D.ContainsKey('ABC') then
            exit('missing');
        foreach K in D.Keys() do
            exit(K);
    end;

    procedure CodeKeyFromCodeThenText(): Text
    var
        D: Dictionary of [Code[20], Integer];
        C: Code[20];
    begin
        C := 'xyz';
        D.Add(C, 1);
        if not D.ContainsKey('xyz') then
            exit('missing');
        exit('found');
    end;

    procedure ClearedKeepsKeyType(): Text
    var
        D: Dictionary of [Code[20], Integer];
    begin
        D.Add('x', 1);
        Clear(D);
        D.Add('abc', 1);
        if not D.ContainsKey('ABC') then
            exit('missing');
        exit('found');
    end;

    procedure LearnCharCounter(): Integer
    var
        counter: Dictionary of [Char, Integer];
    begin
        CountCharactersInCustomerName('abca', counter);
        exit(counter.Get('a'));
    end;

    procedure LearnCharCounterCount(): Text
    var
        counter: Dictionary of [Char, Integer];
        k: Char;
        r: Text;
    begin
        CountCharactersInCustomerName('abca', counter);
        foreach k in counter.Keys() do
            r += Format(k) + '=' + Format(counter.Get(k)) + ';';
        exit(Format(counter.Count()) + ':' + r);
    end;

    procedure CountCharactersInCustomerName(customerName: Text; counter: Dictionary of [Char, Integer])
    var
        i: Integer;
        c: Integer;
    begin
        for i := 1 to StrLen(customerName) do
            if counter.Get(customerName[i], c) then
                counter.Set(customerName[i], c + 1)
            else
                counter.Add(customerName[i], 1);
    end;

    procedure CharKeyFromIndex(): Text
    var
        counter: Dictionary of [Char, Integer];
        s: Text;
    begin
        s := 'abc';
        counter.Add(s[1], 1);
        if counter.ContainsKey('a') then
            exit('found');
        exit('missing');
    end;

    procedure TextKeyFromChar(): Text
    var
        D: Dictionary of [Text, Integer];
        s: Text;
    begin
        s := 'abc';
        D.Add(s[2], 1);
        if D.ContainsKey('b') then
            exit('found');
        exit('missing');
    end;
}
"#;

/// A key argument takes the dictionary's declared key type, as BC converts
/// any argument to its parameter type: Text to Code, one character of Text to
/// Char, and Char to Text.
#[test]
fn dictionary_keys_are_converted_to_the_declared_key_type() {
    let probe = |proc: &str| {
        ok(run(
            &[("/ws/TypedDictionaryKeys.al", TYPED_DICTIONARY_KEYS)],
            "Typed Dictionary Keys",
            proc,
            vec![],
        ))
    };
    assert_eq!(probe("CodeKeyFromText"), Value::Code("ABC".into()));
    assert_eq!(
        probe("CodeKeyFromCodeThenText"),
        Value::Text("found".into())
    );
    assert_eq!(probe("ClearedKeepsKeyType"), Value::Text("found".into()));
    assert_eq!(probe("LearnCharCounter"), Value::Integer(2));
    assert_eq!(
        probe("LearnCharCounterCount"),
        Value::Text("3:a=2;b=1;c=1;".into())
    );
    assert_eq!(probe("CharKeyFromIndex"), Value::Text("found".into()));
    assert_eq!(probe("TextKeyFromChar"), Value::Text("found".into()));
}

const VAR_RESULT_FORMS: &str = r#"codeunit 50308 "Var Result Forms"
{
    procedure ListGetVar(): Text
    var
        L: List of [Integer];
        V: Integer;
    begin
        L.AddRange(4, 5);
        if not L.Get(2, V) then
            exit('false');
        if L.Get(9, V) then
            exit('found 9');
        exit(Format(V));
    end;

    procedure ListGetVarStatement()
    var
        L: List of [Integer];
        V: Integer;
    begin
        L.Add(4);
        L.Get(2, V);
    end;

    procedure ListSetVar(): Text
    var
        L: List of [Integer];
        Old: Integer;
    begin
        L.AddRange(4, 5);
        L.Set(1, 7, Old);
        exit(Format(Old) + '/' + Format(L.Get(1)));
    end;

    procedure ListSetVarOutOfRange(): Text
    var
        L: List of [Integer];
        Old: Integer;
    begin
        L.Add(4);
        Old := 1;
        if L.Set(3, 7, Old) then
            exit('set');
        exit(Format(Old) + '/' + Format(L.Count()));
    end;

    procedure DictSetVar(): Text
    var
        D: Dictionary of [Integer, Integer];
        Old: Integer;
        Other: Integer;
        Replaced: Boolean;
    begin
        D.Add(1, 10);
        D.Set(1, 20, Old);
        Replaced := D.Set(2, 30, Other);
        exit(Format(Old) + '/' + Format(D.Get(1)) + '/' + Format(Replaced) + '/' + Format(D.Get(2)));
    end;
}
"#;

/// `List.Get(Index, var Result)`, `List.Set(Index, Value, var OldValue)` and
/// `Dictionary.Set(Key, Value, var OldValue)` write the element or the old
/// value to the variable and return whether it was there. A List index out
/// of range returns false, or raises when the call is a statement.
#[test]
fn var_forms_of_list_get_list_set_and_dictionary_set_run_locally() {
    let files = [("/ws/VarResultForms.al", VAR_RESULT_FORMS)];
    let probe = |proc: &str| ok(run(&files, "Var Result Forms", proc, vec![]));
    assert_eq!(probe("ListGetVar"), Value::Text("5".into()));
    assert_eq!(probe("ListSetVar"), Value::Text("4/7".into()));
    assert_eq!(probe("ListSetVarOutOfRange"), Value::Text("1/1".into()));
    assert_eq!(probe("DictSetVar"), Value::Text("10/20/No/30".into()));
    let message = error_message(run(
        &files,
        "Var Result Forms",
        "ListGetVarStatement",
        vec![],
    ));
    assert!(message.contains("index 2 out of range"), "got: {message}");
}

const LIST_RANGE_OVERLOADS: &str = r#"codeunit 50309 "List Range Overloads"
{
    procedure RemoveRangeUsed(): Text
    var
        L: List of [Integer];
    begin
        L.AddRange(1, 2);
        if not L.RemoveRange(5, 10) then
            exit('false/' + Format(L.Count()));
        exit('true');
    end;

    procedure RemoveRangeStatement()
    var
        L: List of [Integer];
    begin
        L.AddRange(1, 2);
        L.RemoveRange(5, 10);
    end;

    procedure AddRangeNested(): Text
    var
        Outer: List of [List of [Integer]];
        Inner: List of [Integer];
    begin
        Inner.AddRange(1, 2, 3);
        Outer.AddRange(Inner);
        exit(Format(Outer.Count()) + '/' + Format(Outer.Get(1).Count()));
    end;

    procedure AddRangeNestedEmpty(): Integer
    var
        Outer: List of [List of [Integer]];
        Inner: List of [Integer];
    begin
        Outer.AddRange(Inner);
        exit(Outer.Count());
    end;

    procedure AddRangeListOfLists(): Integer
    var
        Outer: List of [List of [Integer]];
        Other: List of [List of [Integer]];
        Inner: List of [Integer];
    begin
        Other.Add(Inner);
        Other.Add(Inner);
        Outer.AddRange(Other);
        exit(Outer.Count());
    end;

    procedure AddRangeAfterClear(): Integer
    var
        Outer: List of [List of [Integer]];
        Inner: List of [Integer];
    begin
        Outer.Add(Inner);
        Clear(Outer);
        Inner.AddRange(1, 2);
        Outer.AddRange(Inner);
        exit(Outer.Count());
    end;
}
"#;

/// `RemoveRange` returns false for a range out of bounds when its result is
/// used and raises as a statement. `AddRange` with one List argument adds its
/// elements for `AddRange(List of [T])`, and adds the list as one element
/// when the declared element type is a List, for `AddRange(T)`.
#[test]
fn removerange_result_and_addrange_overloads_follow_the_declared_types() {
    let files = [("/ws/ListRangeOverloads.al", LIST_RANGE_OVERLOADS)];
    let probe = |proc: &str| ok(run(&files, "List Range Overloads", proc, vec![]));
    assert_eq!(probe("RemoveRangeUsed"), Value::Text("false/2".into()));
    assert_eq!(probe("AddRangeNested"), Value::Text("1/3".into()));
    assert_eq!(probe("AddRangeNestedEmpty"), Value::Integer(1));
    assert_eq!(probe("AddRangeListOfLists"), Value::Integer(2));
    assert_eq!(probe("AddRangeAfterClear"), Value::Integer(1));
    let message = error_message(run(
        &files,
        "List Range Overloads",
        "RemoveRangeStatement",
        vec![],
    ));
    assert!(message.contains("out of range"), "got: {message}");
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

const CHAIN_PROBE: &str = r#"codeunit 50190 Chain
{
    procedure SplitCount(): Integer
    var
        S: Text;
    begin
        S := 'a,b';
        exit(S.Split(',').Count());
    end;

    procedure TrimThenUpper(): Text
    var
        S: Text;
    begin
        S := ' a,b ';
        exit(S.Trim().ToUpper());
    end;

    procedure FormatThenPad(): Text
    begin
        exit(Format(12).PadLeft(4, '0') + '|' + 'ab'.PadRight(3) + '|');
    end;

    procedure FieldThenUpper(): Text
    var
        Item: Record Item;
    begin
        Item."No." := 'X1';
        Item.Description := 'hello';
        exit(Item.Description.ToUpper());
    end;

    procedure SplitElementTrimmed(): Text
    var
        S: Text;
        Parts: List of [Text];
    begin
        S := 'a, b ,c';
        Parts := S.Split(',');
        exit(Parts.Get(2).Trim() + S.Remove(2, 1).Split(' ').Get(1));
    end;

    procedure ElementCharacter(): Text
    var
        S: Text;
    begin
        S := 'ab,cd';
        exit(Format(S.Split(',').Get(2)[2]));
    end;

    procedure UnsupportedStep(): Text
    var
        S: Text;
    begin
        exit(Format(S.Trim().IndexOfAny('x')));
    end;
}
"#;

/// Only the last call of a chain ran, on the first receiver: `S.Trim().ToUpper()`
/// answered ' A,B ' and `S.Split(',').Count()` failed to find an object 'S'.
#[test]
fn chained_calls_apply_every_step_in_order() {
    let chain = |proc: &str| {
        run(
            &[("/ws/Chain.al", CHAIN_PROBE), ("/ws/Item.al", ITEM_TABLE)],
            "Chain",
            proc,
            vec![],
        )
    };
    assert_eq!(ok(chain("SplitCount")), Value::Integer(2));
    assert_eq!(ok(chain("TrimThenUpper")), Value::Text("A,B".into()));
    assert_eq!(ok(chain("FormatThenPad")), Value::Text("0012|ab |".into()));
    assert_eq!(ok(chain("FieldThenUpper")), Value::Text("HELLO".into()));
    assert_eq!(ok(chain("SplitElementTrimmed")), Value::Text("ba".into()));
    assert_eq!(ok(chain("ElementCharacter")), Value::Text("d".into()));
    let unsupported = error_message(chain("UnsupportedStep"));
    assert!(
        unsupported.contains("Text.IndexOfAny in a chained call is not supported"),
        "{unsupported}"
    );
}

const POINT_ORDER: &str = r#"codeunit 50153 "Point Order"
{
    procedure Order(): Text
    var
        Calc: Codeunit "Point Calc";
        Ledger: Record "Point Ledger";
        Seen: Text;
    begin
        Calc.Add(1, 'A', 5);
        Calc.Add(2, 'B', 9);
        Calc.Add(3, 'C', 5);
        Ledger.SetCurrentKey(Points);
        Ledger.Ascending(false);
        if Ledger.Ascending() then
            exit('still ascending');
        if Ledger.FindSet() then
            repeat
                Seen += Ledger.Member;
            until Ledger.Next() = 0;
        Ledger.Reset();
        Ledger.FindFirst();
        exit(Seen + '|' + Ledger.Member);
    end;
}
"#;

/// Ascending was unsupported, so a test sorting backwards went to live BC.
/// Descending order reverses ties on the current key too.
#[test]
fn ascending_false_iterates_the_key_backwards_until_reset() {
    let files = [
        ("/ws/Ledger.al", POINT_LEDGER),
        ("/ws/Calc.al", POINT_CALC),
        ("/ws/Order.al", POINT_ORDER),
    ];
    assert_eq!(
        ok(run(&files, "Point Order", "Order", vec![])),
        Value::Text("BCA|A".into())
    );
}

const ENUM_PROBE: &str = r#"codeunit 50192 "Enum Probe"
{
    procedure AssignedFormat(): Text
    var
        C: Enum Colour;
    begin
        C := Colour::Blue;
        exit(Format(C));
    end;

    procedure UnassignedIsOrdinalZero(): Boolean
    var
        C: Enum Colour;
    begin
        exit(C = Colour::Red);
    end;

    procedure AsInteger(): Integer
    var
        C: Enum Colour;
    begin
        C := Colour::Blue;
        exit(C.AsInteger());
    end;

    procedure ValueAsInteger(): Integer
    begin
        exit(Colour::Blue.AsInteger() + Enum::Colour::Blue.AsInteger());
    end;

    procedure FromIntegerAndNames(): Text
    var
        C: Enum Colour;
    begin
        C := Enum::Colour.FromInteger(3);
        exit(Format(C) + Format(Enum::Colour.Names().Count()));
    end;

    procedure UnassignedFormatAndNames(): Text
    var
        C: Enum Colour;
    begin
        exit(Format(C) + '|' + C.Names().Get(2));
    end;

    procedure CaseOnEnum(): Integer
    var
        C: Enum Colour;
    begin
        C := Enum::Colour::Blue;
        case C of
            Colour::Red: exit(1);
            Colour::Blue: exit(2);
        end;
    end;
}
"#;

const COLOUR_ENUM: &str =
    "enum 50191 Colour\n{\n    value(0; Red) { }\n    value(3; Blue) { }\n}\n";

/// `Enum` variables were never bound, so assigning one failed as an
/// undeclared identifier, and no enum method ran locally.
#[test]
fn enum_variables_and_methods_run_locally() {
    let call = |proc: &str| {
        ok(run(
            &[("/ws/Probe.al", ENUM_PROBE), ("/ws/Colour.al", COLOUR_ENUM)],
            "Enum Probe",
            proc,
            vec![],
        ))
    };
    assert_eq!(call("AssignedFormat"), Value::Text("Blue".into()));
    assert_eq!(call("UnassignedIsOrdinalZero"), Value::Boolean(true));
    assert_eq!(call("AsInteger"), Value::Integer(3));
    assert_eq!(call("CaseOnEnum"), Value::Integer(2));
    assert_eq!(call("ValueAsInteger"), Value::Integer(6));
    assert_eq!(call("FromIntegerAndNames"), Value::Text("Blue2".into()));
    assert_eq!(
        call("UnassignedFormatAndNames"),
        Value::Text("Red|Blue".into())
    );
}

const MISC_PROBE: &str = r#"codeunit 50193 "Misc Probe"
{
    procedure Builder(): Text
    var
        B: TextBuilder;
    begin
        B.Append('ab');
        B.AppendLine('c');
        B.Insert(1, '>');
        B.Replace('b', 'B');
        B.Remove(2, 1);
        exit(Format(B.Length()) + '|' + B.ToText().TrimEnd());
    end;

    procedure Guids(): Text
    var
        Id: Guid;
        Seen: Text;
    begin
        if IsNullGuid(Id) then
            Seen := 'null';
        Id := CreateGuid();
        if not IsNullGuid(Id) then
            Seen += '|created';
        exit(Seen);
    end;

    procedure RenameAndTest(): Text
    var
        Item: Record Item;
    begin
        Item.Init();
        Item."No." := 'A';
        Item.Description := 'Chair';
        Item.Insert();
        Item.Rename('B');
        if Item.Get('A') then
            exit('old key still there');
        Item.Get('B');
        Item.TestField(Description);
        Item.TestField(Description, 'Chair');
        exit(Item."No.");
    end;

    procedure TestFieldEmpty()
    var
        Item: Record Item;
    begin
        Item.Init();
        Item."No." := 'C';
        Item.TestField("Unit Price");
    end;

    procedure TestFieldValue()
    var
        Item: Record Item;
    begin
        Item.Init();
        Item.Description := 'Chair';
        Item.TestField(Description, 'Table');
    end;
}
"#;

/// TextBuilder, CreateGuid, IsNullGuid, Rename and TestField were
/// unsupported, so tests using them were routed to live BC.
#[test]
fn textbuilder_guids_rename_and_testfield_run_locally() {
    let call = |proc: &str| {
        run(
            &[("/ws/Misc.al", MISC_PROBE), ("/ws/Item.al", ITEM_TABLE)],
            "Misc Probe",
            proc,
            vec![],
        )
    };
    // '>' + 'aBc' + CRLF, then the 'a' at position 2 removed: '>Bc' + CRLF.
    assert_eq!(ok(call("Builder")), Value::Text("5|>Bc".into()));
    assert_eq!(ok(call("Guids")), Value::Text("null|created".into()));
    assert_eq!(ok(call("RenameAndTest")), Value::Code("B".into()));
    let empty = error_message(call("TestFieldEmpty"));
    assert!(
        empty.contains("Unit Price must have a value in Item"),
        "{empty}"
    );
    let wrong = error_message(call("TestFieldValue"));
    assert!(
        wrong.contains("Description must be equal to 'Table' in Item. Current value is 'Chair'."),
        "{wrong}"
    );
}

const MEMBER_TABLE: &str = r#"table 50180 "Tour Member"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Name; Text[50])
        {
            trigger OnValidate()
            begin
                "Search Name" := UpperCase(Name);
                if Name <> xRec.Name then
                    Changes += 1;
            end;
        }
        field(3; "Search Name"; Code[50]) { }
        field(4; Balance; Decimal) { }
        field(5; Changes; Integer) { }
        field(6; "Item No."; Code[20]) { TableRelation = Item; }
        field(7; "Last Balance"; Decimal) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }

    trigger OnInsert()
    begin
        if Balance = 0 then
            Balance := 100;
    end;

    trigger OnModify()
    begin
        "Last Balance" := xRec.Balance;
    end;

    trigger OnDelete()
    begin
        if Balance > 0 then
            Error('Cannot delete %1 with a balance', "No.");
    end;

    procedure Deposit(Amount: Decimal): Decimal
    begin
        Balance += Amount;
        Touch();
        exit(Balance);
    end;

    local procedure Touch()
    begin
        TestField(Name);
    end;
}
"#;

const MEMBER_PROBE: &str = r#"codeunit 50194 "Member Probe"
{
    procedure ValidateRunsOnValidate(): Text
    var
        Member: Record "Tour Member";
    begin
        Member.Init();
        Member."No." := 'M1';
        Member.Validate(Name, 'alice');
        Member.Validate(Name, 'alice');
        Member.Validate(Name, 'bob');
        exit(Member."Search Name" + '|' + Format(Member.Changes));
    end;

    procedure InsertTrueRunsOnInsert(): Text
    var
        Member: Record "Tour Member";
        Plain: Record "Tour Member";
    begin
        Member."No." := 'M1';
        Member.Insert(true);
        Plain."No." := 'M2';
        Plain.Insert();
        Member.Get('M1');
        Plain.Get('M2');
        exit(Format(Member.Balance) + '|' + Format(Plain.Balance));
    end;

    procedure ModifyTrueSeesStoredRow(): Decimal
    var
        Member: Record "Tour Member";
    begin
        Member."No." := 'M1';
        Member.Balance := 10;
        Member.Insert();
        Member.Balance := 25;
        Member.Modify(true);
        Member.Get('M1');
        exit(Member."Last Balance");
    end;

    procedure DeleteTrueCanRefuse()
    var
        Member: Record "Tour Member";
    begin
        Member."No." := 'M1';
        Member.Insert(true);
        Member.Delete(true);
    end;

    procedure TableProcedure(): Decimal
    var
        Member: Record "Tour Member";
    begin
        Member."No." := 'M1';
        Member.Name := 'x';
        Member.Balance := 5;
        exit(Member.Deposit(7) + Member.Balance);
    end;

    procedure TableProcedureRaises(): Decimal
    var
        Member: Record "Tour Member";
    begin
        exit(Member.Deposit(1));
    end;

    procedure RelationChecked(): Text
    var
        Item: Record Item;
        Member: Record "Tour Member";
    begin
        Item."No." := 'CHAIR';
        Item.Insert();
        Member.Validate("Item No.", 'CHAIR');
        Member.Validate("Item No.", '');
        exit('ok');
    end;

    procedure RelationRefused()
    var
        Member: Record "Tour Member";
    begin
        Member.Validate("Item No.", 'TABLE');
    end;
}
"#;

/// Tables with triggers were refused outright and none of their code ran
/// locally: no OnValidate, no Insert(true) trigger, no table procedures,
/// and bare field names in table code were unbound identifiers.
#[test]
fn table_triggers_validate_and_procedures_run_on_the_record() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/Member.al", MEMBER_TABLE),
                ("/ws/Probe.al", MEMBER_PROBE),
                ("/ws/Item.al", ITEM_TABLE),
            ],
            "Member Probe",
            proc,
            vec![],
        )
    };
    // alice (a change from blank), alice again (no change), bob (a change).
    assert_eq!(
        ok(call("ValidateRunsOnValidate")),
        Value::Text("BOB|2".into())
    );
    assert_eq!(
        ok(call("InsertTrueRunsOnInsert")),
        Value::Text("100|0".into())
    );
    assert_eq!(
        ok(call("ModifyTrueSeesStoredRow")),
        Value::Decimal(dec!(10))
    );
    let refused = error_message(call("DeleteTrueCanRefuse"));
    assert!(
        refused.contains("Cannot delete M1 with a balance"),
        "{refused}"
    );
    // Deposit returns 12 and leaves Balance at 12 on the caller's record.
    assert_eq!(ok(call("TableProcedure")), Value::Decimal(dec!(24)));
    let untested = error_message(call("TableProcedureRaises"));
    assert!(untested.contains("Name must have a value"), "{untested}");
    assert_eq!(ok(call("RelationChecked")), Value::Text("ok".into()));
    let missing = error_message(call("RelationRefused"));
    assert!(
        missing.contains("cannot be found in the related table (Item)"),
        "{missing}"
    );
}

const EVENT_PUBLISHER: &str = r#"codeunit 50170 Publisher
{
    procedure Post(var Total: Integer; Label: Text): Integer
    begin
        OnBeforePost(Total, Label);
        exit(Total);
    end;

    procedure Bonus(): Integer
    begin
        exit(100);
    end;

    [IntegrationEvent(true, false)]
    local procedure OnBeforePost(var Total: Integer; Label: Text)
    begin
    end;
}
"#;

const EVENT_SUBSCRIBERS: &str = r#"codeunit 50171 Subscribers
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnBeforePost', '', false, false)]
    local procedure AddTen(var Total: Integer)
    begin
        Total += 10;
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnBeforePost', '', false, false)]
    local procedure AddFromSender(sender: Codeunit Publisher; var Total: Integer)
    begin
        Total += sender.Bonus();
    end;

    [EventSubscriber(ObjectType::Codeunit, 50170, 'OnBeforePost', '', false, false)]
    local procedure AddLabelLength(Label: Text; var Total: Integer)
    begin
        Total += StrLen(Label);
    end;

    [EventSubscriber(ObjectType::Table, Database::"Tour Member", 'OnAfterInsertEvent', '', false, false)]
    local procedure StampInsert(var Rec: Record "Tour Member"; RunTrigger: Boolean)
    begin
        if Rec.IsTemporary() or (Rec."No." = 'MOD') then
            exit;
        Rec."Last Balance" := 999;
        Rec.Modify();
    end;

    [EventSubscriber(ObjectType::Table, Database::"Tour Member", 'OnAfterModifyEvent', '', false, false)]
    local procedure ReportChange(var Rec: Record "Tour Member"; var xRec: Record "Tour Member")
    begin
        if Rec."No." = 'MOD' then
            Error('changed %1 to %2', xRec.Balance, Rec.Balance);
    end;

    [EventSubscriber(ObjectType::Table, Database::"Tour Member", 'OnBeforeValidateEvent', 'Name', false, false)]
    local procedure RefuseBlankName(var Rec: Record "Tour Member")
    begin
        if Rec.Name = '' then
            Error('A name is required');
    end;
}
"#;

const MANUAL_SUBSCRIBERS: &str = r#"codeunit 50173 "Manual Subscribers"
{
    EventSubscriberInstance = Manual;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnBeforePost', '', false, false)]
    local procedure AddThousand(var Total: Integer)
    begin
        Total += 1000;
    end;
}
"#;

const EVENT_PROBE: &str = r#"codeunit 50172 "Event Probe"
{
    procedure PostRunsSubscribers(): Integer
    var
        P: Codeunit Publisher;
        Total: Integer;
    begin
        Total := 1;
        P.Post(Total, 'abc');
        exit(Total);
    end;

    procedure InsertRaisesTableEvent(): Text
    var
        Member: Record "Tour Member";
        Temp: Record "Tour Member" temporary;
    begin
        Member."No." := 'M1';
        Member.Insert();
        Member.Get('M1');
        Temp."No." := 'T1';
        Temp.Insert();
        Temp.Get('T1');
        exit(Format(Member."Last Balance") + '|' + Format(Temp."Last Balance"));
    end;

    procedure ModifySubscriberSeesStoredRow()
    var
        Member: Record "Tour Member";
    begin
        Member."No." := 'MOD';
        Member.Balance := 10;
        Member.Insert();
        Member.Balance := 25;
        Member.Modify();
    end;

    procedure ValidateRaisesFieldEvent()
    var
        Member: Record "Tour Member";
    begin
        Member.Validate(Name, '');
    end;
}
"#;

/// Subscribers never ran locally, yet the router followed the publisher's
/// edges to them and kept such tests local: a test that relied on one
/// failed here and passed on BC.
#[test]
fn events_run_their_automatic_subscribers() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/Publisher.al", EVENT_PUBLISHER),
                ("/ws/Subscribers.al", EVENT_SUBSCRIBERS),
                ("/ws/Manual.al", MANUAL_SUBSCRIBERS),
                ("/ws/Member.al", MEMBER_TABLE),
                ("/ws/Item.al", ITEM_TABLE),
                ("/ws/Probe.al", EVENT_PROBE),
            ],
            "Event Probe",
            proc,
            vec![],
        )
    };
    // 1 + 10 (by name) + 100 (from the sender) + 3 (by ID, 'abc'); the
    // manual subscriber is unbound.
    assert_eq!(ok(call("PostRunsSubscribers")), Value::Integer(114));
    assert_eq!(
        ok(call("InsertRaisesTableEvent")),
        Value::Text("999|0".into())
    );
    // OnAfterModifyEvent's xRec is the row as stored before the Modify.
    let changed = error_message(call("ModifySubscriberSeesStoredRow"));
    assert!(changed.contains("changed 10 to 25"), "{changed}");
    let refused = error_message(call("ValidateRaisesFieldEvent"));
    assert!(refused.contains("A name is required"), "{refused}");
}

const RENAMED_MEMBER_TABLE: &str = r#"table 50260 "Renamed Member"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Renamed From"; Code[20]) { }
    }
    keys
    {
        key(PK; "No.") { }
    }

    trigger OnRename()
    var
        Logger: Codeunit "Rename Logger";
    begin
        if StrLen("No.") < 3 then
            Error('%1 is too short', "No.");
        Logger.Add('OnRename', "No.", xRec."No.");
        "Renamed From" := xRec."No.";
    end;
}
"#;

const RENAME_LOG_TABLE: &str = r#"table 50261 "Rename Log"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Step; Text[30]) { }
        field(3; "Rec No."; Code[20]) { }
        field(4; "xRec No."; Code[20]) { }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

const RENAME_LOGGER: &str = r#"codeunit 50262 "Rename Logger"
{
    procedure Add(Step: Text; RecNo: Code[20]; XRecNo: Code[20])
    var
        Log: Record "Rename Log";
    begin
        Log."Entry No." := Log.Count() + 1;
        Log.Step := Step;
        Log."Rec No." := RecNo;
        Log."xRec No." := XRecNo;
        Log.Insert();
    end;

    procedure Read(): Text
    var
        Log: Record "Rename Log";
        Seen: Text;
    begin
        if Log.FindSet() then
            repeat
                Seen += Log.Step + ':' + Log."Rec No." + '<-' + Log."xRec No." + '|';
            until Log.Next() = 0;
        exit(Seen);
    end;
}
"#;

const RENAME_SUBSCRIBERS: &str = r#"codeunit 50263 "Rename Subscribers"
{
    [EventSubscriber(ObjectType::Table, Database::"Renamed Member", 'OnBeforeRenameEvent', '', false, false)]
    local procedure BeforeRename(var Rec: Record "Renamed Member"; var xRec: Record "Renamed Member"; RunTrigger: Boolean)
    var
        Logger: Codeunit "Rename Logger";
    begin
        Logger.Add('Before', Rec."No.", xRec."No.");
    end;

    [EventSubscriber(ObjectType::Table, Database::"Renamed Member", 'OnAfterRenameEvent', '', false, false)]
    local procedure AfterRename(var Rec: Record "Renamed Member"; var xRec: Record "Renamed Member"; RunTrigger: Boolean)
    var
        Logger: Codeunit "Rename Logger";
    begin
        Logger.Add('After', Rec."No.", xRec."No.");
    end;
}
"#;

const RENAME_PROBE: &str = r#"codeunit 50264 "Rename Probe"
{
    procedure RenameSeesBothKeys(): Text
    var
        Member: Record "Renamed Member";
        Logger: Codeunit "Rename Logger";
    begin
        Member."No." := 'OLD';
        Member.Insert();
        Member.Rename('NEW');
        Member.Get('NEW');
        exit(Logger.Read() + Member."Renamed From");
    end;

    procedure RenameToShortKey()
    var
        Member: Record "Renamed Member";
    begin
        Member."No." := 'LONGKEY';
        Member.Insert();
        Member.Rename('AB');
    end;

    procedure FailedRenameKeepsTheOldKey(): Text
    var
        Member: Record "Renamed Member";
        Other: Record "Renamed Member";
    begin
        Other."No." := 'TAKEN';
        Other.Insert();
        Member."No." := 'OLD';
        Member.Insert();
        if Member.Rename('TAKEN') then
            exit('renamed onto an existing key');
        exit(Member."No.");
    end;
}
"#;

/// In Business Central `Rec` holds the new key and `xRec` the row as stored
/// in OnBeforeRenameEvent, OnRename and OnAfterRenameEvent, and Rename
/// writes what OnRename sets on `Rec`. OnRename ran before the key changed,
/// so `Rec` held the old key there and in OnBeforeRenameEvent.
#[test]
fn rename_code_sees_the_new_key_as_rec_and_the_stored_row_as_xrec() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/RenamedMember.al", RENAMED_MEMBER_TABLE),
                ("/ws/RenameLog.al", RENAME_LOG_TABLE),
                ("/ws/RenameLogger.al", RENAME_LOGGER),
                ("/ws/RenameSubscribers.al", RENAME_SUBSCRIBERS),
                ("/ws/RenameProbe.al", RENAME_PROBE),
            ],
            "Rename Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("RenameSeesBothKeys")),
        Value::Text("Before:NEW<-OLD|OnRename:NEW<-OLD|After:NEW<-OLD|OLD".into())
    );
    let refused = error_message(call("RenameToShortKey"));
    assert!(refused.contains("AB is too short"), "{refused}");
    assert_eq!(
        ok(call("FailedRenameKeepsTheOldKey")),
        Value::Code("OLD".into())
    );
}

const PLAIN_TABLE: &str = r#"table 50270 "Plain"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Parent No."; Code[20]) { TableRelation = Plain; }
    }
    keys
    {
        key(PK; "No.") { }
    }
}
"#;

const PLAIN_ENTRY_TABLE: &str = r#"table 50271 "Plain Entry"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Plain No."; Code[20]) { TableRelation = "Plain"; }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

const PLAIN_LINE_TABLE: &str = r#"table 50272 "Plain Line"
{
    fields
    {
        field(1; "Plain No."; Code[20]) { TableRelation = "Plain"."No."; }
        field(2; "Line No."; Integer) { }
    }
    keys
    {
        key(PK; "Plain No.", "Line No.") { }
    }
}
"#;

const PLAIN_PROBE: &str = r#"codeunit 50273 "Plain Probe"
{
    procedure RenameCascades(): Text
    var
        P: Record "Plain";
        Child: Record "Plain";
        Entry: Record "Plain Entry";
        Line: Record "Plain Line";
        Other: Record "Plain Entry";
    begin
        P."No." := 'OLD';
        P.Insert();
        Child."No." := 'CHILD';
        Child."Parent No." := 'OLD';
        Child.Insert();
        Entry."Entry No." := 1;
        Entry."Plain No." := 'OLD';
        Entry.Insert();
        Other."Entry No." := 2;
        Other."Plain No." := 'ELSE';
        Other.Insert();
        Line."Plain No." := 'OLD';
        Line."Line No." := 10000;
        Line.Insert();
        P.Rename('NEW');
        Entry.Get(1);
        Other.Get(2);
        Child.Get('CHILD');
        if Line.Get('OLD', 10000) then
            exit('the line kept the old key');
        Line.Get('NEW', 10000);
        exit(Entry."Plain No." + '|' + Other."Plain No." + '|' + Child."Parent No." + '|' + Line."Plain No.");
    end;

    procedure RenameLine()
    var
        Line: Record "Plain Line";
    begin
        Line."Plain No." := 'A';
        Line."Line No." := 1;
        Line.Insert();
        Line.Rename('A', 2);
    end;

    procedure TemporaryRenameStaysLocal(): Text
    var
        P: Record "Plain" temporary;
        Entry: Record "Plain Entry";
    begin
        P."No." := 'OLD';
        P.Insert();
        Entry."Entry No." := 1;
        Entry."Plain No." := 'OLD';
        Entry.Insert();
        P.Rename('NEW');
        Entry.Get(1);
        exit(Entry."Plain No.");
    end;
}
"#;

/// Rename "updates the primary key value in all related tables": every
/// field whose plain TableRelation names the renamed key gets the new value,
/// and a row whose key holds it moves. A temporary record has no related
/// rows.
#[test]
fn rename_updates_the_fields_that_relate_to_the_key() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/Plain.al", PLAIN_TABLE),
                ("/ws/PlainEntry.al", PLAIN_ENTRY_TABLE),
                ("/ws/PlainLine.al", PLAIN_LINE_TABLE),
                ("/ws/PlainProbe.al", PLAIN_PROBE),
            ],
            "Plain Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("RenameCascades")),
        Value::Text("NEW|ELSE|NEW|NEW".into())
    );
    assert_eq!(
        ok(call("TemporaryRenameStaysLocal")),
        Value::Code("OLD".into())
    );
}

/// A relation the local rename cannot follow makes the rename an error that
/// names it, where a silent rename would leave the related rows behind.
#[test]
fn rename_refuses_a_relation_it_cannot_follow() {
    let conditional = r#"table 50274 "Plain Usage"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Kind; Option) { OptionMembers = Plain,Other; }
        field(3; "No."; Code[20]) { TableRelation = if (Kind = const(Plain)) Plain; }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;
    let result = run(
        &[
            ("/ws/Plain.al", PLAIN_TABLE),
            ("/ws/PlainEntry.al", PLAIN_ENTRY_TABLE),
            ("/ws/PlainLine.al", PLAIN_LINE_TABLE),
            ("/ws/PlainUsage.al", conditional),
            ("/ws/PlainProbe.al", PLAIN_PROBE),
        ],
        "Plain Probe",
        "RenameCascades",
        vec![],
    );
    let refused = error_message(result);
    assert!(
        refused.contains("field No. of table Plain Usage") && refused.contains("cannot follow"),
        "{refused}"
    );
    let line_ref = r#"table 50275 "Line Ref"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Line No."; Integer) { TableRelation = "Plain Line"."Line No."; }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;
    let result = run(
        &[
            ("/ws/Plain.al", PLAIN_TABLE),
            ("/ws/PlainEntry.al", PLAIN_ENTRY_TABLE),
            ("/ws/PlainLine.al", PLAIN_LINE_TABLE),
            ("/ws/LineRef.al", line_ref),
            ("/ws/PlainProbe.al", PLAIN_PROBE),
        ],
        "Plain Probe",
        "RenameLine",
        vec![],
    );
    let refused = error_message(result);
    assert!(refused.contains("composite primary key"), "{refused}");
}

/// A table then a codeunit in one file: the codeunit's calls took the
/// file's first object (the table) as their identity and read globals from
/// the whole file, failing as "stateful codeunit 'Tour Member'". Label
/// globals were never bound.
#[test]
fn second_object_of_a_file_runs_as_itself_with_its_label_globals() {
    let file = r#"table 50195 "Greeting Log"
{
    fields
    {
        field(1; "No."; Integer) { }
    }
    keys
    {
        key(PK; "No.") { }
    }
    var
        TableCounter: Integer;
}

codeunit 50196 "Greeter"
{
    var
        GreetingLbl: Label 'Hello %1, it''s %2', Comment = '%1 = name, %2 = day';

    procedure Greet(): Text
    begin
        exit(Compose('Ann'));
    end;

    local procedure Compose(Name: Text): Text
    begin
        exit(StrSubstNo(GreetingLbl, Name, 'Monday'));
    end;
}
"#;
    assert_eq!(
        ok(run(&[("/ws/Greeter.al", file)], "Greeter", "Greet", vec![])),
        Value::Text("Hello Ann, it's Monday".into())
    );
}

const JSON_PROBE: &str = r#"codeunit 50197 "Json Probe"
{
    procedure BuildAndWrite(): Text
    var
        Customer: JsonObject;
        Address: JsonObject;
        Lines: JsonArray;
        Out: Text;
    begin
        Customer.Add('name', 'Ann');
        Customer.Add('balance', 12.5);
        Customer.Add('vip', true);
        Address.Add('city', 'Oslo');
        Customer.Add('address', Address);
        Lines.Add(1);
        Lines.Add('two');
        Customer.Add('lines', Lines);
        // Address was added by reference: later changes show in Customer.
        Address.Add('zip', '0150');
        Customer.WriteTo(Out);
        exit(Out);
    end;

    procedure ReadAndNavigate(): Text
    var
        Doc: JsonObject;
        Token: JsonToken;
        Items: JsonArray;
        Total: Decimal;
        i: Integer;
    begin
        if not Doc.ReadFrom('{"order":{"no":"SO1","items":[{"qty":2,"price":1.25},{"qty":1,"price":10}]}}') then
            exit('unreadable');
        Doc.SelectToken('$.order.items', Token);
        Items := Token.AsArray();
        for i := 0 to Items.Count() - 1 do begin
            Items.Get(i, Token);
            Total += Token.AsObject().GetDecimal('qty') * Token.AsObject().GetDecimal('price');
        end;
        Doc.SelectToken('order.no', Token);
        exit(Token.AsValue().AsText() + '|' + Format(Total) + '|' + Format(Doc.Contains('order')));
    end;

    procedure SharedReference(): Integer
    var
        A: JsonObject;
        B: JsonObject;
        Token: JsonToken;
    begin
        A.Add('n', 1);
        B := A;
        B.Replace('n', 2);
        A.Get('n', Token);
        exit(Token.AsValue().AsInteger());
    end;

    procedure SharedBeforeFirstUse(): Text
    var
        A: JsonObject;
        B: JsonObject;
        Out: Text;
    begin
        B := A;
        A.Add('a', 1);
        Fill(B);
        B.WriteTo(Out);
        exit(Out);
    end;

    local procedure Fill(Target: JsonObject)
    begin
        Target.Add('b', 2);
    end;

    procedure UnassignedTokenReads(): Text
    var
        Token: JsonToken;
    begin
        if not Token.ReadFrom('{"a":[1,2]}') then
            exit('unreadable');
        exit(Format(Token.IsObject()) + '|' + Format(Token.AsObject().Contains('a')));
    end;

    procedure DuplicateKey()
    var
        A: JsonObject;
    begin
        A.Add('n', 1);
        A.Add('n', 2);
    end;
}
"#;

/// JSON types were unsupported, so any test using them went to live BC.
#[test]
fn json_objects_arrays_and_tokens_run_locally() {
    let call = |proc: &str| run(&[("/ws/Json.al", JSON_PROBE)], "Json Probe", proc, vec![]);
    assert_eq!(
        ok(call("BuildAndWrite")),
        Value::Text(
            r#"{"name":"Ann","balance":12.5,"vip":true,"address":{"city":"Oslo","zip":"0150"},"lines":[1,"two"]}"#
                .into()
        )
    );
    assert_eq!(
        ok(call("ReadAndNavigate")),
        Value::Text("SO1|12.5|Yes".into())
    );
    assert_eq!(ok(call("SharedReference")), Value::Integer(2));
    // JSON values are references even before first use, and a by-value
    // parameter passes the reference.
    assert_eq!(
        ok(call("SharedBeforeFirstUse")),
        Value::Text(r#"{"a":1,"b":2}"#.into())
    );
    assert_eq!(
        ok(call("UnassignedTokenReads")),
        Value::Text("Yes|Yes".into())
    );
    let duplicate = error_message(call("DuplicateKey"));
    assert!(
        duplicate.contains("the key 'n' already exists"),
        "{duplicate}"
    );
}

/// A subscriber codeunit with globals failed as a stateful codeunit. BC
/// runs an automatic subscriber on a new instance each time the event is
/// raised, so its globals start afresh on every call.
#[test]
fn subscriber_codeunits_with_globals_run_on_a_fresh_instance() {
    let publisher = r#"codeunit 50198 Ticker
{
    procedure Tick(var Seen: Integer)
    begin
        OnTick(Seen);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnTick(var Seen: Integer)
    begin
    end;
}
"#;
    let subscriber = r#"codeunit 50199 "Tick Counter"
{
    var
        Calls: Integer;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::Ticker, 'OnTick', '', false, false)]
    local procedure Count(var Seen: Integer)
    begin
        Calls += 1;
        Seen := Seen * 10 + Calls;
    end;
}
"#;
    let probe = r#"codeunit 50200 "Tick Probe"
{
    procedure TickTwice(): Integer
    var
        T: Codeunit Ticker;
        Seen: Integer;
    begin
        T.Tick(Seen);
        T.Tick(Seen);
        exit(Seen);
    end;
}
"#;
    let result = run(
        &[
            ("/ws/Ticker.al", publisher),
            ("/ws/TickCounter.al", subscriber),
            ("/ws/TickProbe.al", probe),
        ],
        "Tick Probe",
        "TickTwice",
        vec![],
    );
    // Calls is 1 on both events: 0 * 10 + 1 = 1, then 1 * 10 + 1 = 11.
    assert_eq!(ok(result), Value::Integer(11));
}

/// "Each event subscriber will be run in its own codeunit instance"
/// (EventSubscriberInstance on Learn), also when an instance of the
/// subscriber's codeunit is the one raising the event. The subscriber found
/// the running instance's globals on the stack and wrote into them.
#[test]
fn a_subscriber_gets_its_own_instance_while_its_codeunit_runs() {
    let publisher = r#"codeunit 50430 "Self Ticker"
{
    procedure Tick()
    begin
        OnTick();
    end;

    [IntegrationEvent(false, false)]
    local procedure OnTick()
    begin
    end;
}
"#;
    let subscriber = r#"codeunit 50431 "Self Sub"
{
    procedure Run(): Integer
    var
        T: Codeunit "Self Ticker";
    begin
        T.Tick();
        T.Tick();
        exit(Calls);
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Self Ticker", 'OnTick', '', false, false)]
    local procedure CountTick()
    begin
        Calls += 1;
    end;

    var
        Calls: Integer;
}
"#;
    let probe = r#"codeunit 50432 "Self Sub Probe"
{
    procedure Run(): Integer
    var
        S: Codeunit "Self Sub";
    begin
        exit(S.Run());
    end;
}
"#;
    let files = [
        ("/ws/SelfTicker.al", publisher),
        ("/ws/SelfSub.al", subscriber),
        ("/ws/SelfSubProbe.al", probe),
    ];
    assert_eq!(
        ok(run(&files, "Self Sub Probe", "Run", vec![])),
        Value::Integer(0)
    );
    // Run as the first object of the run, with no variable.
    assert_eq!(
        ok(run(&files, "Self Sub", "Run", vec![])),
        Value::Integer(0)
    );
}

const SAME_NAME_SETUP_PAGE: &str = r#"page 50300 "My Setup"
{
    SourceTable = "My Setup";

    layout
    {
        area(Content)
        {
            field(Stamp; Rec.Stamp) { }
        }
    }
}
"#;

const SAME_NAME_SETUP_TABLE: &str = r#"table 50300 "My Setup"
{
    fields
    {
        field(1; "Primary Key"; Code[10]) { }
        field(2; Stamp; Text[30]) { }
        field(3; Status; Enum "Setup Status") { }
    }
    keys
    {
        key(PK; "Primary Key") { }
    }

    trigger OnInsert()
    begin
        Stamp := 'inserted';
    end;
}
"#;

const SAME_NAME_STATUS_TABLE: &str = r#"table 50301 "Setup Status"
{
    fields
    {
        field(1; Code; Code[10]) { }
    }
    keys
    {
        key(PK; Code) { }
    }
}
"#;

const SAME_NAME_STATUS_ENUM: &str = r#"enum 50302 "Setup Status"
{
    value(0; Draft) { }
    value(1; Ready) { }
}
"#;

const SAME_NAME_HELPER_PAGE: &str = r#"page 50303 "Setup Helper"
{
    layout
    {
        area(Content)
        {
        }
    }
}
"#;

const SAME_NAME_HELPER_CODEUNIT: &str = r#"codeunit 50303 "Setup Helper"
{
    procedure Describe(): Text
    begin
        exit('helper');
    end;
}
"#;

const SAME_NAME_PROBE: &str = r#"codeunit 50304 "Same Name Probe"
{
    procedure InsertRunsTheTablesTrigger(): Text
    var
        Setup: Record "My Setup";
        Helper: Codeunit "Setup Helper";
    begin
        Setup.Init();
        Setup.Insert(true);
        Setup.FindFirst();
        exit(Setup.Stamp + '|' + Format(Setup.Status) + '|' + Helper.Describe());
    end;

    procedure EnumMembersComeFromTheEnum(): Integer
    var
        Setup: Record "My Setup";
    begin
        Setup.Status := "Setup Status"::Ready;
        exit(Setup.Status.AsInteger());
    end;
}
"#;

/// A setup table and its card page share a name, and so can a table and an
/// enum, or a page and a codeunit. The runtime found objects by name alone,
/// so whichever file was indexed first won: with the page first, every
/// record operation on the table failed.
#[test]
fn objects_are_found_by_kind_when_another_kind_shares_the_name() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/MySetup.Page.al", SAME_NAME_SETUP_PAGE),
                ("/ws/MySetup.Table.al", SAME_NAME_SETUP_TABLE),
                ("/ws/SetupStatus.Table.al", SAME_NAME_STATUS_TABLE),
                ("/ws/SetupStatus.Enum.al", SAME_NAME_STATUS_ENUM),
                ("/ws/SetupHelper.Page.al", SAME_NAME_HELPER_PAGE),
                ("/ws/SetupHelper.Codeunit.al", SAME_NAME_HELPER_CODEUNIT),
                ("/ws/SameNameProbe.al", SAME_NAME_PROBE),
            ],
            "Same Name Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("InsertRunsTheTablesTrigger")),
        Value::Text("inserted|Draft|helper".into())
    );
    assert_eq!(ok(call("EnumMembersComeFromTheEnum")), Value::Integer(1));
}

const BULK_PARENT_TABLE: &str = r#"table 50270 "Bulk Parent"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Status; Text[20]) { }
        field(3; Touched; Integer) { }
    }
    keys
    {
        key(PK; "No.") { }
    }

    trigger OnDelete()
    var
        Log: Codeunit "Bulk Log";
    begin
        Log.Add('OnDelete ' + "No.");
    end;

    trigger OnModify()
    var
        Log: Codeunit "Bulk Log";
    begin
        Log.Add('OnModify ' + "No." + ' ' + xRec.Status + '>' + Status);
        Touched := Touched + 1;
    end;
}
"#;

const BULK_CHILD_TABLE: &str = r#"table 50271 "Bulk Child"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Parent No."; Code[20]) { }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

const BULK_LOG_TABLE: &str = r#"table 50272 "Bulk Log Entry"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Step; Text[100]) { }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

const BULK_LOG: &str = r#"codeunit 50273 "Bulk Log"
{
    procedure Add(Step: Text)
    var
        Entry: Record "Bulk Log Entry";
    begin
        Entry."Entry No." := Entry.Count() + 1;
        Entry.Step := Step;
        Entry.Insert();
    end;

    procedure Read(): Text
    var
        Entry: Record "Bulk Log Entry";
        Seen: Text;
    begin
        if Entry.FindSet() then
            repeat
                Seen += Entry.Step + '|';
            until Entry.Next() = 0;
        exit(Seen);
    end;
}
"#;

const BULK_SUBSCRIBERS: &str = r#"codeunit 50274 "Bulk Subscribers"
{
    [EventSubscriber(ObjectType::Table, Database::"Bulk Parent", 'OnBeforeDeleteEvent', '', false, false)]
    local procedure BeforeDelete(var Rec: Record "Bulk Parent"; RunTrigger: Boolean)
    var
        Log: Codeunit "Bulk Log";
    begin
        Log.Add('BeforeDelete ' + Rec."No." + ' ' + Format(RunTrigger));
    end;

    [EventSubscriber(ObjectType::Table, Database::"Bulk Parent", 'OnAfterDeleteEvent', '', false, false)]
    local procedure DeleteChildren(var Rec: Record "Bulk Parent")
    var
        Child: Record "Bulk Child";
        Log: Codeunit "Bulk Log";
    begin
        Log.Add('AfterDelete ' + Rec."No.");
        Child.SetRange("Parent No.", Rec."No.");
        Child.DeleteAll();
    end;

    [EventSubscriber(ObjectType::Table, Database::"Bulk Parent", 'OnBeforeModifyEvent', '', false, false)]
    local procedure RefuseClosing(var Rec: Record "Bulk Parent"; var xRec: Record "Bulk Parent")
    var
        Log: Codeunit "Bulk Log";
    begin
        if Rec.Status = 'Closed' then
            Error('%1 cannot be closed', Rec."No.");
        Log.Add('BeforeModify ' + Rec."No." + ' ' + xRec.Status + '>' + Rec.Status);
    end;

    [EventSubscriber(ObjectType::Table, Database::"Bulk Parent", 'OnAfterModifyEvent', '', false, false)]
    local procedure AfterModify(var Rec: Record "Bulk Parent")
    var
        Log: Codeunit "Bulk Log";
    begin
        Log.Add('AfterModify ' + Rec."No.");
    end;
}
"#;

const BULK_PROBE: &str = r#"codeunit 50275 "Bulk Probe"
{
    local procedure Seed()
    var
        Parent: Record "Bulk Parent";
        Child: Record "Bulk Child";
    begin
        Parent."No." := 'P1';
        Parent.Status := 'Open';
        Parent.Insert();
        Parent."No." := 'P2';
        Parent.Insert();
        Parent."No." := 'Q1';
        Parent.Insert();
        Child."Entry No." := 1;
        Child."Parent No." := 'P1';
        Child.Insert();
        Child."Entry No." := 2;
        Child."Parent No." := 'P2';
        Child.Insert();
        Child."Entry No." := 3;
        Child."Parent No." := 'Q1';
        Child.Insert();
    end;

    procedure DeleteAllRaisesDeleteEvents(): Text
    var
        Parent: Record "Bulk Parent";
        Child: Record "Bulk Child";
        Log: Codeunit "Bulk Log";
    begin
        Seed();
        Parent.SetRange("No.", 'P1', 'P2');
        Parent.DeleteAll();
        Parent.Reset();
        exit(Log.Read() + Format(Parent.Count()) + Format(Child.Count()));
    end;

    procedure DeleteAllTrueRunsOnDelete(): Text
    var
        Parent: Record "Bulk Parent";
        Log: Codeunit "Bulk Log";
    begin
        Seed();
        Parent.SetRange("No.", 'P1');
        Parent.DeleteAll(true);
        exit(Log.Read());
    end;

    procedure ModifyAllRaisesModifyEvents(): Text
    var
        Parent: Record "Bulk Parent";
        Log: Codeunit "Bulk Log";
    begin
        Seed();
        Parent.SetRange("No.", 'P1', 'P2');
        Parent.ModifyAll(Status, 'Held');
        Parent.Get('P2');
        exit(Log.Read() + Parent.Status + Format(Parent.Touched));
    end;

    procedure ModifyAllTrueRunsOnModify(): Text
    var
        Parent: Record "Bulk Parent";
        Log: Codeunit "Bulk Log";
    begin
        Seed();
        Parent.SetRange("No.", 'P1');
        Parent.ModifyAll(Status, 'Held', true);
        Parent.Get('P1');
        exit(Log.Read() + Parent.Status + Format(Parent.Touched));
    end;

    procedure ModifyAllGuardRefuses()
    var
        Parent: Record "Bulk Parent";
    begin
        Seed();
        Parent.ModifyAll(Status, 'Closed');
    end;
}
"#;

/// Business Central raises OnBeforeDeleteEvent and OnAfterDeleteEvent for
/// each row of a DeleteAll, and OnBeforeModifyEvent and OnAfterModifyEvent
/// for each row of a ModifyAll, and runs OnDelete or OnModify when
/// RunTrigger is true. Both removed or wrote the rows in one pass, so no
/// subscriber ran, and RunTrigger true failed.
#[test]
fn deleteall_and_modifyall_raise_the_table_events_for_each_row() {
    let call = |proc: &str| {
        run(
            &[
                ("/ws/BulkParent.al", BULK_PARENT_TABLE),
                ("/ws/BulkChild.al", BULK_CHILD_TABLE),
                ("/ws/BulkLogEntry.al", BULK_LOG_TABLE),
                ("/ws/BulkLog.al", BULK_LOG),
                ("/ws/BulkSubscribers.al", BULK_SUBSCRIBERS),
                ("/ws/BulkProbe.al", BULK_PROBE),
            ],
            "Bulk Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("DeleteAllRaisesDeleteEvents")),
        Value::Text(
            "BeforeDelete P1 No|AfterDelete P1|BeforeDelete P2 No|AfterDelete P2|11".into()
        )
    );
    assert_eq!(
        ok(call("DeleteAllTrueRunsOnDelete")),
        Value::Text("BeforeDelete P1 Yes|OnDelete P1|AfterDelete P1|".into())
    );
    assert_eq!(
        ok(call("ModifyAllRaisesModifyEvents")),
        Value::Text(
            "BeforeModify P1 Open>Held|AfterModify P1|BeforeModify P2 Open>Held|AfterModify P2|Held0"
                .into()
        )
    );
    assert_eq!(
        ok(call("ModifyAllTrueRunsOnModify")),
        Value::Text("BeforeModify P1 Open>Held|OnModify P1 Open>Held|AfterModify P1|Held1".into())
    );
    let refused = error_message(call("ModifyAllGuardRefuses"));
    assert!(refused.contains("P1 cannot be closed"), "{refused}");
}

const JSON_READFROM_PROBE: &str = r#"codeunit 50276 "Json ReadFrom Probe"
{
    procedure ReusedInALoop(): Text
    var
        Lines: List of [Text];
        Line: Text;
        LineObj: JsonObject;
        Arr: JsonArray;
        Out: Text;
    begin
        Lines.Add('{"n":1}');
        Lines.Add('{"n":2}');
        Lines.Add('{"n":3}');
        foreach Line in Lines do begin
            LineObj.ReadFrom(Line);
            Arr.Add(LineObj);
        end;
        Arr.WriteTo(Out);
        exit(Out);
    end;

    procedure LeavesTheParentAlone(): Text
    var
        Parent: JsonObject;
        Child: JsonObject;
        Out: Text;
    begin
        Parent.Add('child', Child);
        Child.ReadFrom('{"x":1}');
        Parent.WriteTo(Out);
        exit(Out);
    end;

    procedure TokenFromGetLeavesTheParentAlone(): Text
    var
        Parent: JsonObject;
        Token: JsonToken;
        Out: Text;
        Line: Text;
    begin
        Parent.ReadFrom('{"child":{"x":1}}');
        Parent.Get('child', Token);
        Token.ReadFrom('{"y":2}');
        Parent.WriteTo(Out);
        Token.WriteTo(Line);
        exit(Out + Line);
    end;

    procedure AnAliasSeesTheNewValue(): Text
    var
        A: JsonObject;
        B: JsonObject;
        Out: Text;
    begin
        A.Add('old', 1);
        B := A;
        A.ReadFrom('{"new":2}');
        B.WriteTo(Out);
        exit(Out);
    end;
}
"#;

/// ReadFrom wrote the parsed value into the variable's node, which a parent
/// object or array may hold, so a variable read again in a loop rewrote the
/// rows already added. Business Central disconnects the variable from its
/// tree and gives it the new value.
#[test]
fn readfrom_gives_the_variable_a_new_value_and_leaves_its_tree_alone() {
    let call = |proc: &str| {
        run(
            &[("/ws/JsonReadFrom.al", JSON_READFROM_PROBE)],
            "Json ReadFrom Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("ReusedInALoop")),
        Value::Text(r#"[{"n":1},{"n":2},{"n":3}]"#.into())
    );
    assert_eq!(
        ok(call("LeavesTheParentAlone")),
        Value::Text(r#"{"child":{}}"#.into())
    );
    assert_eq!(
        ok(call("TokenFromGetLeavesTheParentAlone")),
        Value::Text(r#"{"child":{"x":1}}{"y":2}"#.into())
    );
    // `B := A` shares A's reference, so B sees what A reads.
    assert_eq!(
        ok(call("AnAliasSeesTheNewValue")),
        Value::Text(r#"{"new":2}"#.into())
    );
}

const JSON_POSITION_PROBE: &str = r#"codeunit 50277 "Json Position Probe"
{
    procedure MissesAsExpressions(): Text
    var
        Obj: JsonObject;
        Arr: JsonArray;
        Token: JsonToken;
        Seen: Text;
    begin
        Obj.Add('a', 1);
        Arr.Add(1);
        if not Obj.Get('missing', Token) then
            Seen += 'get|';
        if not Obj.ReadFrom('not json') then
            Seen += 'read|';
        if not Obj.SelectToken('$.missing', Token) then
            Seen += 'select|';
        if not Obj.Add('a', 2) then
            Seen += 'add|';
        if not Obj.Replace('missing', 2) then
            Seen += 'replace|';
        if not Arr.Get(5, Token) then
            Seen += 'arrayget|';
        if not Arr.Insert(5, 2) then
            Seen += 'insert|';
        if not Arr.Set(5, 2) then
            Seen += 'set|';
        if not Arr.RemoveAt(5) then
            Seen += 'removeat|';
        if not Arr.ReadFrom('{"an":"object"}') then
            Seen += 'shape|';
        exit(Seen);
    end;

    procedure GetMiss()
    var
        Obj: JsonObject;
        Token: JsonToken;
    begin
        Obj.Add('a', 1);
        Obj.Get('missing', Token);
    end;

    procedure ReadFromBadText()
    var
        Obj: JsonObject;
    begin
        Obj.ReadFrom('not json');
    end;

    procedure SelectTokenMiss()
    var
        Obj: JsonObject;
        Token: JsonToken;
    begin
        Obj.SelectToken('$.missing', Token);
    end;

    procedure ReplaceMiss()
    var
        Obj: JsonObject;
    begin
        Obj.Replace('missing', 2);
    end;

    procedure ArrayGetMiss()
    var
        Arr: JsonArray;
        Token: JsonToken;
    begin
        Arr.Get(5, Token);
    end;

    procedure AssertErrorCatchesAGetMiss(): Text
    var
        Obj: JsonObject;
        Token: JsonToken;
    begin
        asserterror Obj.Get('missing', Token);
        exit('caught');
    end;
}
"#;

/// Business Central raises a failed Get, ReadFrom, SelectToken, Add,
/// Replace, Insert, Set or RemoveAt as a runtime error when the return
/// value is not used, and returns false when it is. The local runtime
/// returned false as a statement and raised in an expression.
#[test]
fn json_failures_raise_as_statements_and_return_false_as_expressions() {
    let call = |proc: &str| {
        run(
            &[("/ws/JsonPosition.al", JSON_POSITION_PROBE)],
            "Json Position Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("MissesAsExpressions")),
        Value::Text("get|read|select|add|replace|arrayget|insert|set|removeat|shape|".into())
    );
    for (proc, expected) in [
        ("GetMiss", "the key 'missing' does not exist"),
        ("ReadFromBadText", "not valid JSON"),
        ("SelectTokenMiss", "no token matches"),
        ("ReplaceMiss", "the key 'missing' does not exist"),
        ("ArrayGetMiss", "index 5 is outside the JSON array"),
    ] {
        let error = error_message(call(proc));
        assert!(error.contains(expected), "{proc}: {error}");
    }
    assert_eq!(
        ok(call("AssertErrorCatchesAGetMiss")),
        Value::Text("caught".into())
    );
}

const JSON_DEFAULT_PROBE: &str = r#"codeunit 50278 "Json Default Probe"
{
    procedure MissingKeysGiveDefaults(): Text
    var
        Obj: JsonObject;
    begin
        Obj.Add('a', 'x');
        exit('[' + Obj.GetText('missing', true) + '|' + Obj.GetCode('missing', true) + '|' +
            Format(Obj.GetInteger('missing', true)) + '|' + Format(Obj.GetBigInteger('missing', true)) + '|' +
            Format(Obj.GetDecimal('missing', true)) + '|' + Format(Obj.GetBoolean('missing', true)) + '|' +
            Obj.GetText('a', true) + ']');
    end;

    procedure MissingKeyWithoutDefault(): Text
    var
        Obj: JsonObject;
    begin
        exit(Obj.GetText('missing', false));
    end;
}
"#;

/// JsonObject's typed getters take DefaultIfNotFound (runtime 15.0): with
/// true, a missing key gives the type's default value. The local runtime
/// raised an error.
#[test]
fn json_object_getters_honour_default_if_not_found() {
    let call = |proc: &str| {
        run(
            &[("/ws/JsonDefault.al", JSON_DEFAULT_PROBE)],
            "Json Default Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("MissingKeysGiveDefaults")),
        Value::Text("[||0|0|0|No|x]".into())
    );
    let error = error_message(call("MissingKeyWithoutDefault"));
    assert!(
        error.contains("the key 'missing' does not exist"),
        "{error}"
    );
}

const JSON_PATH_PROBE: &str = r#"codeunit 50279 "Json Path Probe"
{
    local procedure Company(var Doc: JsonObject)
    begin
        Doc.ReadFrom('{"company":{"boss":"Diana","employees":[{"id":"Marcy","salary":8.95},{"id":"John","salary":7,"bonus":1},{"id":"Diana","salary":10.95}]}}');
    end;

    procedure FilterFromTheLearnExample(): Decimal
    var
        Doc: JsonObject;
        Token: JsonToken;
        EmployeeId: Text;
    begin
        Company(Doc);
        EmployeeId := 'John';
        Doc.SelectToken('$.company.employees[?(@.id==''' + EmployeeId + ''')].salary', Token);
        exit(Token.AsValue().AsDecimal());
    end;

    procedure RecursiveDescent(): Text
    var
        Doc: JsonObject;
        Token: JsonToken;
    begin
        Doc.ReadFrom('{"a":{"b":{"c":"deep"}}}');
        if not Doc.SelectToken('$..c', Token) then
            exit('not found');
        exit(Token.AsValue().AsText());
    end;

    procedure Filters(): Text
    var
        Doc: JsonObject;
        Token: JsonToken;
        Seen: Text;
    begin
        Company(Doc);
        Doc.SelectToken('$.company.employees[?(@.salary > 8 && @.salary < 10)].id', Token);
        Seen := Token.AsValue().AsText();
        Doc.SelectToken('$.company.employees[?(@.bonus)].id', Token);
        Seen += '|' + Token.AsValue().AsText();
        Doc.SelectToken('$.company.employees[?(@.id == $.company.boss)].salary', Token);
        Seen += '|' + Format(Token.AsValue().AsDecimal());
        Doc.SelectToken('$.company.employees[?(@.id == ''Nobody'' || @.salary >= 10.95)].id', Token);
        Seen += '|' + Token.AsValue().AsText();
        Doc.SelectToken('$..employees[2].id', Token);
        Seen += '|' + Token.AsValue().AsText();
        if not Doc.SelectToken('$..id', Token) then
            Seen += '|many';
        if not Doc.SelectToken('$.company.employees[*].id', Token) then
            Seen += '|wildcard many';
        Doc.SelectToken('$.company.*[1].id', Token);
        exit(Seen + '|' + Token.AsValue().AsText());
    end;

    procedure SeveralMatchesAsAStatement()
    var
        Doc: JsonObject;
        Token: JsonToken;
    begin
        Company(Doc);
        Doc.SelectToken('$..id', Token);
    end;

    procedure UnsupportedStep(): Text
    var
        Doc: JsonObject;
        Token: JsonToken;
    begin
        Company(Doc);
        if not Doc.SelectToken('$.company.employees[0:2]', Token) then
            exit('not found');
        exit('found');
    end;
}
"#;

/// SelectToken read only member and index steps: a filter failed as
/// "not an array index" and `..` found nothing. Business Central selects
/// with filters and recursive descent, and fails unless exactly one token
/// matches.
#[test]
fn selecttoken_follows_filters_and_recursive_descent() {
    let call = |proc: &str| {
        run(
            &[("/ws/JsonPath.al", JSON_PATH_PROBE)],
            "Json Path Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("FilterFromTheLearnExample")),
        Value::Decimal(dec!(7))
    );
    assert_eq!(ok(call("RecursiveDescent")), Value::Text("deep".into()));
    assert_eq!(
        ok(call("Filters")),
        Value::Text("Marcy|John|10.95|Diana|Diana|many|wildcard many|John".into())
    );
    let several = error_message(call("SeveralMatchesAsAStatement"));
    assert!(several.contains("matches 3 tokens"), "{several}");
    let unsupported = error_message(call("UnsupportedStep"));
    assert!(
        unsupported.contains("'[0:2]'") && unsupported.contains("not supported"),
        "{unsupported}"
    );
}

const TABLE_PUBLISHER: &str = r#"table 50280 "Table Pub"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Note; Text[50]) { }
    }
    keys
    {
        key(PK; "No.") { }
    }

    procedure Stamp()
    begin
        OnStamp(5);
    end;

    [IntegrationEvent(true, false)]
    local procedure OnStamp(Times: Integer)
    begin
    end;
}
"#;

const TABLE_PUBLISHER_SUBSCRIBER: &str = r#"codeunit 50281 "Table Pub Sub"
{
    [EventSubscriber(ObjectType::Table, Database::"Table Pub", 'OnStamp', '', false, false)]
    local procedure OnStampSub(var Sender: Record "Table Pub"; Times: Integer)
    begin
        Sender.Note := 'stamped ' + Sender."No." + ' ' + Format(Times);
    end;
}
"#;

const TABLE_PUBLISHER_PROBE: &str = r#"codeunit 50282 "Table Pub Probe"
{
    procedure StampRunsTheSubscriber(): Text
    var
        Pub: Record "Table Pub";
    begin
        Pub."No." := 'P1';
        Pub.Insert();
        Pub.Stamp();
        exit(Pub.Note);
    end;
}
"#;

/// An event a table procedure publishes with IncludeSender passes the
/// record as `Sender`. The subscriber failed as declaring a parameter the
/// event does not publish.
#[test]
fn table_publisher_passes_its_record_as_sender() {
    let result = run(
        &[
            ("/ws/TablePub.al", TABLE_PUBLISHER),
            ("/ws/TablePubSub.al", TABLE_PUBLISHER_SUBSCRIBER),
            ("/ws/TablePubProbe.al", TABLE_PUBLISHER_PROBE),
        ],
        "Table Pub Probe",
        "StampRunsTheSubscriber",
        vec![],
    );
    assert_eq!(ok(result), Value::Text("stamped P1 5".into()));
}

const CODEUNIT_PUBLISHER_SENDER: &str = r#"codeunit 50444 "Sender Pub"
{
    var
        Counter: Integer;

    procedure Post()
    begin
        Counter := 7;
        OnPost();
    end;

    procedure PostAndRead(): Integer
    begin
        Post();
        exit(Counter);
    end;

    procedure GetCounter(): Integer
    begin
        exit(Counter);
    end;

    procedure SetCounter(Value: Integer)
    begin
        Counter := Value;
    end;

    [IntegrationEvent(true, false)]
    local procedure OnPost()
    begin
    end;
}

codeunit 50445 "Sender Sub"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sender Pub", 'OnPost', '', false, false)]
    local procedure OnPostSub(sender: Codeunit "Sender Pub")
    begin
        if sender.GetCounter() <> 7 then
            Error('sender counter %1', sender.GetCounter());
        sender.SetCounter(sender.GetCounter() + 1);
    end;
}

codeunit 50446 "Sender Probe"
{
    procedure ThroughVariable(): Integer
    var
        Pub: Codeunit "Sender Pub";
    begin
        Pub.Post();
        exit(Pub.GetCounter());
    end;
}
"#;

/// With IncludeSender, a codeunit publisher's `sender` is the instance that
/// raised the event, so a subscriber reads and changes its globals. The
/// sender was a new instance, whose `Counter` read 0.
#[test]
fn a_codeunit_publishers_sender_is_the_running_instance() {
    let files = [("/ws/SenderPub.al", CODEUNIT_PUBLISHER_SENDER)];
    assert_eq!(
        ok(run(&files, "Sender Probe", "ThroughVariable", vec![])),
        Value::Integer(8)
    );
    assert_eq!(
        ok(run(&files, "Sender Pub", "PostAndRead", vec![])),
        Value::Integer(8)
    );
}

const KEYWORD_NAMED_VARIABLES: &str = r#"table 50283 "Keyword Named"
{
    fields
    {
        field(1; Code; Code[20]) { }
        field(2; Description; Text[50]) { }
        field(3; Value; Text[50])
        {
            trigger OnValidate()
            begin
                Description := Code + '=' + Value;
            end;
        }
    }
    keys
    {
        key(PK; Code) { }
    }
}

enum 50285 "Keyword Kind"
{
    value(0; First) { }
    value(1; Second) { }
}

codeunit 50284 "Keyword Probe"
{
    procedure Reads(): Text
    var
        Value: Text;
        Code: Code[10];
        Text: Text;
        Date: Date;
        Time: Time;
        Version: Integer;
        File: Text;
        Page: Integer;
        Report: TextBuilder;
    begin
        Value := 'abc';
        Code := Value;
        Text := Value + Code;
        Date := 20240101D;
        Time := 120000T;
        Report.Append(Value);
        Version := Report.Length + StrLen(Code);
        File := Value.ToUpper();
        Page := Version * 2;
        exit(Text + '|' + Format(Date, 0, 9) + '|' + Format(Time, 0, 9) + '|' + Format(Version) + '|' + File + '|' + Format(Page));
    end;

    procedure Parameters(Value: Text; var Code: Code[10]): Text
    begin
        Code := CopyStr(Value, 1, 2);
        Append(Value);
        exit(Value + '|' + Code);
    end;

    procedure PassesVar(): Text
    var
        Code: Code[10];
    begin
        exit(Parameters('xyz', Code) + '|' + Code);
    end;

    local procedure Append(var Value: Text)
    begin
        Value += '!';
    end;

    procedure BuiltinsStillWork(): Integer
    var
        Keyed: Record "Keyword Named";
    begin
        Keyed.Code := 'K1';
        Keyed.Validate(Value, 'v1');
        if Keyed.Description <> 'K1=v1' then
            Error('bare field names read %1', Keyed.Description);
        exit(Enum::"Keyword Kind"::Second.AsInteger() + Enum::"Keyword Kind".FromInteger(1).AsInteger());
    end;
}
"#;

/// A variable, parameter or field named `Value`, `Code`, `Text`, `Date`,
/// `Time`, `Version`, `File`, `Page` or `Report` parses as an object or type
/// keyword node. Every read failed with `unsupported expression kind`.
/// `Report.Length` also calls a method without its parentheses.
#[test]
fn variables_named_after_keywords_read_and_write() {
    let call = |proc: &str| {
        run(
            &[("/ws/Keyword.al", KEYWORD_NAMED_VARIABLES)],
            "Keyword Probe",
            proc,
            vec![],
        )
    };
    assert_eq!(
        ok(call("Reads")),
        Value::Text("abcABC|2024-01-01|12:00:00|6|ABC|12".into())
    );
    assert_eq!(ok(call("PassesVar")), Value::Text("xyz!|XY|XY".into()));
    assert_eq!(ok(call("BuiltinsStillWork")), Value::Integer(2));
}

const DROPPED_PARENTHESES: &str = r#"table 50448 "Dropped Parens"
{
    fields
    {
        field(1; Code; Code[20]) { }
        field(2; Note; Text[50]) { }
    }
    keys
    {
        key(PK; Code) { }
    }

    procedure Stamp()
    begin
        Note := 'stamped';
    end;
}

codeunit 50449 "Dropped Parens Probe"
{
    procedure InsertStatement(): Integer
    var
        R: Record "Dropped Parens";
    begin
        R.Code := 'A';
        R.Insert;
        R.Reset;
        exit(R.Count());
    end;

    procedure FindFirstCondition(): Text
    var
        R: Record "Dropped Parens";
    begin
        R.Code := 'A';
        R.Insert();
        R.Code := '';
        if R.FindFirst then
            exit('found ' + R.Code);
        exit('none');
    end;

    procedure CountInExit(): Integer
    var
        R: Record "Dropped Parens";
    begin
        R.Code := 'Q';
        R.Insert();
        R.Code := 'R';
        R.Insert();
        exit(R.Count);
    end;

    procedure TableProcedure(): Text
    var
        R: Record "Dropped Parens";
    begin
        R.Stamp;
        exit(R.Note);
    end;

    procedure DuplicateInsertStatementRaises(): Text
    var
        R: Record "Dropped Parens";
    begin
        R.Code := 'A';
        R.Insert;
        asserterror R.Insert;
        exit('raised');
    end;
}
"#;

/// A record method or table procedure written without parentheses runs, as
/// AL compiles it (CodeCop AA0008 warns about the form). It was read as a
/// field and failed with "field 'Insert' is not declared".
#[test]
fn record_methods_without_parentheses_run() {
    let call = |proc: &str| {
        ok(run(
            &[("/ws/DroppedParens.al", DROPPED_PARENTHESES)],
            "Dropped Parens Probe",
            proc,
            vec![],
        ))
    };
    assert_eq!(call("InsertStatement"), Value::Integer(1));
    assert_eq!(call("FindFirstCondition"), Value::Text("found A".into()));
    assert_eq!(call("CountInExit"), Value::Integer(2));
    assert_eq!(call("TableProcedure"), Value::Text("stamped".into()));
    assert_eq!(
        call("DuplicateInsertStatementRaises"),
        Value::Text("raised".into())
    );
}

const KEYWORD_NAMED_INDEXES: &str = r#"codeunit 50450 "Keyword Indexes"
{
    procedure Indexes(): Text
    var
        Page: Text;
        Code: Code[10];
        Value: array[3] of Integer;
        Letter: Text;
    begin
        Page := 'abc';
        Page[1] := 'x';
        Code := 'AB';
        Letter := Code[2];
        Value[2] := 5;
        exit(Page + '|' + Format(Letter) + '|' + Format(Value[2]));
    end;
}
"#;

/// A variable named after an object or type keyword can be indexed. The
/// grammar gives its name as `object_keyword` or `type_keyword`, which the
/// index read and write did not accept (GR3-2).
#[test]
fn variables_named_after_keywords_can_be_indexed() {
    let result = run(
        &[("/ws/KeywordIndexes.al", KEYWORD_NAMED_INDEXES)],
        "Keyword Indexes",
        "Indexes",
        vec![],
    );
    assert_eq!(ok(result), Value::Text("xbc|B|5".into()));
}

const SIGNED_CASE_LABELS: &str = r#"codeunit 50286 "Signed Labels"
{
    procedure ByInteger(X: Integer): Integer
    var
        Limit: Integer;
    begin
        Limit := 7;
        case X of
            -1:
                exit(1);
            -2, 2:
                exit(2);
            -Limit:
                exit(7);
            else
                exit(0);
        end;
    end;

    procedure ByDecimal(D: Decimal): Integer
    begin
        case D of
            -2.5:
                exit(1);
            2.5:
                exit(2);
        end;
        exit(0);
    end;
}
"#;

/// A case label with a leading minus is one `signed_case_label` node,
/// which `eval_expr` rejected as an unsupported expression kind.
#[test]
fn case_labels_with_a_leading_minus_match() {
    let call = |proc: &str, arg: Value| {
        run(
            &[("/ws/Signed.al", SIGNED_CASE_LABELS)],
            "Signed Labels",
            proc,
            vec![arg],
        )
    };
    assert_eq!(ok(call("ByInteger", Value::Integer(-1))), Value::Integer(1));
    assert_eq!(ok(call("ByInteger", Value::Integer(-2))), Value::Integer(2));
    assert_eq!(ok(call("ByInteger", Value::Integer(2))), Value::Integer(2));
    assert_eq!(ok(call("ByInteger", Value::Integer(-7))), Value::Integer(7));
    assert_eq!(ok(call("ByInteger", Value::Integer(1))), Value::Integer(0));
    assert_eq!(
        ok(call("ByDecimal", Value::Decimal(dec!(-2.5)))),
        Value::Integer(1)
    );
    assert_eq!(
        ok(call("ByDecimal", Value::Decimal(dec!(2.5)))),
        Value::Integer(2)
    );
    assert_eq!(
        ok(call("ByDecimal", Value::Decimal(dec!(-1.5)))),
        Value::Integer(0)
    );
}

const SIGNED_CASE_RANGES: &str = r#"codeunit 50434 "Signed Ranges"
{
    procedure ByRange(X: Integer): Integer
    begin
        case X of
            -5..-3:
                exit(1);
            - 2:
                exit(2);
            -10..- 8:
                exit(3);
            0..-1:
                exit(4);
            else
                exit(0);
        end;
    end;
}
"#;

/// Grammar a108400 scans `-5..-3:` as a `signed_case_label`, `..`, a unary
/// minus and an integer, where it made `-5..` one token before, and accepts
/// `- 2:` with a space after the minus (GR2-4).
#[test]
fn negative_range_labels_and_a_spaced_minus_match() {
    let call = |x: i64| {
        ok(run(
            &[("/ws/SignedRanges.al", SIGNED_CASE_RANGES)],
            "Signed Ranges",
            "ByRange",
            vec![Value::Integer(x)],
        ))
    };
    for (x, arm) in [
        (-5, 1),
        (-4, 1),
        (-3, 1),
        (-2, 2),
        (-10, 3),
        (-8, 3),
        (-6, 0),
        (-11, 0),
        (-1, 0),
        (0, 0),
    ] {
        assert_eq!(call(x), Value::Integer(arm), "case {x}");
    }
}

const QUOTED_VARIABLE_NAMES: &str = r#"codeunit 50447 "Quoted Names"
{
    procedure ByLocal(X: Integer): Integer
    var
        "My Limit": Integer;
    begin
        "My Limit" := 4;
        case X of
            -"My Limit":
                exit(1);
            "My Limit":
                exit(2);
        end;
        exit(-"My Limit" * 10);
    end;

    procedure ByParameter("Line No.": Integer): Integer
    begin
        exit("Line No." + 1);
    end;
}
"#;

/// A quoted variable or parameter name reads its value, also as a case
/// label with a leading minus. The read looked the name up with its quotes
/// and failed with `unbound identifier`.
#[test]
fn quoted_variable_names_read_their_value() {
    let call = |proc: &str, arg: i64| {
        ok(run(
            &[("/ws/Quoted.al", QUOTED_VARIABLE_NAMES)],
            "Quoted Names",
            proc,
            vec![Value::Integer(arg)],
        ))
    };
    assert_eq!(call("ByLocal", -4), Value::Integer(1));
    assert_eq!(call("ByLocal", 4), Value::Integer(2));
    assert_eq!(call("ByLocal", 0), Value::Integer(-40));
    assert_eq!(call("ByParameter", 10000), Value::Integer(10001));
}

const ID_BOUND_PUBLISHER: &str = r#"codeunit 50287 "Id Publisher"
{
    procedure Raise(): Integer
    var
        Total: Integer;
    begin
        OnRaise(Total);
        exit(Total);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnRaise(var Total: Integer)
    begin
    end;
}

table 50288 "Id Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Note; Text[50]) { }
    }
    keys
    {
        key(PK; "No.") { }
    }
}

codeunit 50289 "Id Subscribers"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Id Publisher", 'OnRaise', '', false, false)]
    local procedure AddOne(var Total: Integer)
    begin
        Total += 1;
    end;

    [EventSubscriber(ObjectType::Codeunit, 50287, 'OnRaise', '', false, false)]
    local procedure AddTen(var Total: Integer)
    begin
        Total += 10;
    end;

    [EventSubscriber(ObjectType::Table, 50288, 'OnBeforeInsertEvent', '', false, false)]
    local procedure StampInsert(var Rec: Record "Id Table")
    begin
        Rec.Note := 'by id';
    end;
}

codeunit 50290 "Id Probe"
{
    procedure Inserts(): Text
    var
        Row: Record "Id Table";
    begin
        Row."No." := 'R1';
        Row.Insert();
        Row.Get('R1');
        exit(Row.Note);
    end;
}
"#;

/// An EventSubscriber may name its publisher by a bare object ID. Only the
/// `Codeunit::Name` form was bound, so the ID subscriber never ran.
#[test]
fn subscribers_bound_by_object_id_run() {
    let files = [("/ws/IdEvents.al", ID_BOUND_PUBLISHER)];
    assert_eq!(
        ok(run(&files, "Id Publisher", "Raise", vec![])),
        Value::Integer(11)
    );
    assert_eq!(
        ok(run(&files, "Id Probe", "Inserts", vec![])),
        Value::Text("by id".into())
    );
}

const OVERLOADED_PROCEDURES: &str = r#"codeunit 50440 "Overloads"
{
    procedure Helper(A: Integer): Text
    begin
        exit('int');
    end;

    procedure Helper(A: Text): Text
    begin
        exit('text');
    end;

    procedure Other(): Text
    begin
        exit('none');
    end;

    procedure Other(A: Integer): Text
    begin
        exit('one');
    end;

    procedure Amount(A: Decimal): Text
    begin
        exit('decimal');
    end;

    procedure Amount(A: Integer): Text
    begin
        exit('integer');
    end;

    procedure Calls(): Text
    begin
        exit(Helper(1) + '|' + Helper('x') + '|' + Other() + '|' + Other(5) + '|' + Amount(1) + '|' + Amount(1.5));
    end;

    procedure NoMatch(): Text
    begin
        exit(Helper(true));
    end;
}

codeunit 50441 "Overloads Reversed"
{
    procedure Helper(A: Text): Text
    begin
        exit('text');
    end;

    procedure Helper(A: Integer): Text
    begin
        exit('int');
    end;

    procedure Other(A: Integer): Text
    begin
        exit('one');
    end;

    procedure Other(): Text
    begin
        exit('none');
    end;

    procedure Calls(): Text
    begin
        exit(Helper(1) + '|' + Helper('x') + '|' + Other() + '|' + Other(5));
    end;
}

table 50442 "Overload Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
    keys
    {
        key(PK; "No.") { }
    }

    procedure Describe(A: Integer): Text
    begin
        exit('int');
    end;

    procedure Describe(A: Text): Text
    begin
        exit('text');
    end;

    procedure Kind(A: Text): Text
    begin
        exit('text');
    end;

    procedure Kind(A: Integer): Text
    begin
        exit('int');
    end;
}

codeunit 50443 "Overload Probe"
{
    procedure ThroughVariables(): Text
    var
        Cu: Codeunit "Overloads";
        Reversed: Codeunit "Overloads Reversed";
    begin
        exit(Cu.Helper(1) + '|' + Reversed.Helper(1) + '|' + Cu.Other() + '|' + Reversed.Other());
    end;

    procedure TableProcedures(): Text
    var
        R: Record "Overload Table";
    begin
        exit(R.Describe(1) + '|' + R.Describe('x') + '|' + R.Kind(1) + '|' + R.Kind('x'));
    end;
}
"#;

/// A call to an overloaded procedure runs the declaration whose parameters
/// take its arguments, in either order of declaration. The codeunit lookup
/// always ran the last declaration of the name, and the table lookup the
/// first, so the other overload failed its type or count check.
#[test]
fn overloaded_procedures_run_the_declaration_that_takes_the_arguments() {
    let files = [("/ws/Overloads.al", OVERLOADED_PROCEDURES)];
    assert_eq!(
        ok(run(&files, "Overloads", "Calls", vec![])),
        Value::Text("int|text|none|one|integer|decimal".into())
    );
    assert_eq!(
        ok(run(&files, "Overloads Reversed", "Calls", vec![])),
        Value::Text("int|text|none|one".into())
    );
    assert_eq!(
        ok(run(&files, "Overload Probe", "ThroughVariables", vec![])),
        Value::Text("int|int|none|none".into())
    );
    assert_eq!(
        ok(run(&files, "Overload Probe", "TableProcedures", vec![])),
        Value::Text("int|text|int|text".into())
    );
    let message = error_message(run(&files, "Overloads", "NoMatch", vec![]));
    assert!(
        message.contains("no overload of 'Helper' takes these arguments"),
        "got: {message}"
    );
}

const KEPT_MEMBER: &str = r#"table 50420 "Kept Member"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Name; Text[50])
        {
            trigger OnValidate()
            begin
                if not HideDialog then
                    Error('dialog shown');
            end;
        }
        field(3; Stamp; Text[10]) { }
    }
    keys
    {
        key(PK; "No.") { }
    }

    var
        Strict: Boolean;
        HideDialog: Boolean;
        Inserted: Integer;
        Deleted: Integer;

    trigger OnInsert()
    begin
        if Strict then
            Error('strict insert');
        Inserted += 1;
    end;

    trigger OnModify()
    begin
        if Strict then
            Error('strict modify');
        Stamp := 'S';
    end;

    trigger OnDelete()
    var
        Log: Record "Kept Log";
    begin
        if Strict then
            Error('strict delete');
        Deleted += 1;
        Log."Entry No." := Log.Count() + 1;
        Log.Deleted := Deleted;
        Log.Insert();
    end;

    procedure SetStrict()
    begin
        Strict := true;
    end;

    procedure SetHideDialog(Hide: Boolean)
    begin
        HideDialog := Hide;
    end;

    procedure InsertedCount(): Integer
    begin
        exit(Inserted);
    end;

    procedure StrictAfterReset(): Boolean
    begin
        Strict := true;
        Reset();
        exit(Strict);
    end;
}
"#;

const KEPT_LOG: &str = r#"table 50422 "Kept Log"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; Deleted; Integer) { }
    }
    keys
    {
        key(PK; "Entry No.") { }
    }
}
"#;

const KEPT_PROBE: &str = r#"codeunit 50421 "Kept Probe"
{
    procedure HideDialogReachesOnValidate(): Text
    var
        Member: Record "Kept Member";
    begin
        Member.SetHideDialog(true);
        Member.Validate(Name, 'x');
        exit(Member.Name);
    end;

    procedure StrictReachesOnInsert(): Text
    var
        Member: Record "Kept Member";
    begin
        Member."No." := 'A';
        Member.SetStrict();
        asserterror Member.Insert(true);
        exit(GetLastErrorText());
    end;

    procedure TriggerStateIsKept(): Integer
    var
        Member: Record "Kept Member";
    begin
        Member."No." := 'A';
        Member.Insert(true);
        Member."No." := 'B';
        Member.Insert(true);
        exit(Member.InsertedCount());
    end;

    procedure EachVariableHasItsOwn(): Integer
    var
        Strict: Record "Kept Member";
        Plain: Record "Kept Member";
    begin
        Strict.SetStrict();
        Plain."No." := 'A';
        Plain.Insert(true);
        exit(10 * Plain.InsertedCount() + Strict.InsertedCount());
    end;

    procedure ResetClearsThem(): Integer
    var
        Member: Record "Kept Member";
    begin
        Member."No." := 'A';
        Member.Insert(true);
        Member.SetStrict();
        Member.Reset();
        Member."No." := 'B';
        Member.Insert(true);
        exit(Member.InsertedCount());
    end;

    procedure ResetInTableCodeClearsThem(): Boolean
    var
        Member: Record "Kept Member";
    begin
        exit(Member.StrictAfterReset());
    end;

    procedure ModifyAllStartsThemFresh(): Text
    var
        Member: Record "Kept Member";
    begin
        Member."No." := 'A';
        Member.Insert();
        Member.SetStrict();
        Member.ModifyAll(Name, 'y', true);
        Member.Get('A');
        exit(Member.Name + Member.Stamp);
    end;

    procedure DeleteAllSharesOneFreshCopy(): Text
    var
        Member: Record "Kept Member";
        Log: Record "Kept Log";
        Seen: Text;
    begin
        Member."No." := 'A';
        Member.Insert();
        Member."No." := 'B';
        Member.Insert();
        Member.SetStrict();
        Member.DeleteAll(true);
        if Log.FindSet() then
            repeat
                Seen += Format(Log.Deleted);
            until Log.Next() = 0;
        Member."No." := 'C';
        asserterror Member.Insert(true);
        exit(Seen + '|' + GetLastErrorText());
    end;
}
"#;

/// Business Central keeps a table's global variables with the record
/// variable, so a setter such as `SetHideValidationDialog` reaches a trigger
/// that runs later. Each call used to start them fresh. Record.Reset clears
/// them, and ModifyAll and DeleteAll run their triggers on a copy whose
/// globals start at their defaults.
#[test]
fn table_globals_are_kept_with_the_record_variable() {
    let call = |proc: &str| {
        ok(run(
            &[
                ("/ws/KeptMember.al", KEPT_MEMBER),
                ("/ws/KeptLog.al", KEPT_LOG),
                ("/ws/KeptProbe.al", KEPT_PROBE),
            ],
            "Kept Probe",
            proc,
            vec![],
        ))
    };
    assert_eq!(call("HideDialogReachesOnValidate"), Value::Text("x".into()));
    assert_eq!(
        call("StrictReachesOnInsert"),
        Value::Text("strict insert".into())
    );
    assert_eq!(call("TriggerStateIsKept"), Value::Integer(2));
    assert_eq!(call("EachVariableHasItsOwn"), Value::Integer(10));
    assert_eq!(call("ResetClearsThem"), Value::Integer(1));
    assert_eq!(call("ResetInTableCodeClearsThem"), Value::Boolean(false));
    assert_eq!(call("ModifyAllStartsThemFresh"), Value::Text("yS".into()));
    // One copy with fresh globals for the whole DeleteAll, and the record
    // keeps its own afterwards.
    assert_eq!(
        call("DeleteAllSharesOneFreshCopy"),
        Value::Text("12|strict insert".into())
    );
}

const CLEAR_COUNTER: &str = r#"codeunit 50424 "Clear Counter"
{
    procedure Bump()
    begin
        Count += 1;
    end;

    procedure Get(): Integer
    begin
        exit(Count);
    end;

    var
        Count: Integer;
}
"#;

const CLEAR_PROBE: &str = r#"codeunit 50425 "Clear Probe"
{
    procedure ClearsScalars(): Text
    var
        N: Integer;
        D: Decimal;
        T: Text;
        C: Code[10];
        B: Boolean;
        G: Guid;
    begin
        N := 5;
        D := 1.5;
        T := 'x';
        C := 'Y';
        B := true;
        G := CreateGuid();
        Clear(N);
        Clear(D);
        Clear(T);
        Clear(C);
        Clear(B);
        Clear(G);
        if N <> 0 then
            exit('N');
        if D <> 0 then
            exit('D');
        if T <> '' then
            exit('T');
        if C <> '' then
            exit('C');
        if B then
            exit('B');
        if not IsNullGuid(G) then
            exit('G');
        exit('cleared');
    end;

    procedure ClearsCodeunitInstance(): Integer
    var
        C: Codeunit "Clear Counter";
        D: Codeunit "Clear Counter";
    begin
        C.Bump();
        C.Bump();
        D := C;
        Clear(C);
        C.Bump();
        exit(10 * C.Get() + D.Get());
    end;

    procedure KeepsTheVariableStorageQueue(): Integer
    var
        LibraryVariableStorage: Codeunit "Library - Variable Storage";
        N: Integer;
    begin
        LibraryVariableStorage.Enqueue(7);
        N := 5;
        Clear(N);
        exit(LibraryVariableStorage.DequeueInteger() + N);
    end;

    procedure ClearsARecord(): Integer
    var
        Member: Record "Kept Member";
    begin
        Member."No." := 'A';
        Member.Insert();
        Member.SetRange("No.", 'Z');
        Member.SetStrict();
        Clear(Member);
        if Member."No." <> '' then
            exit(-1);
        Member."No." := 'B';
        Member.Insert(true);
        exit(Member.Count());
    end;

    procedure ClearsAField(): Text
    var
        Member: Record "Kept Member";
    begin
        Member.Stamp := 'S';
        Clear(Member.Stamp);
        exit('[' + Member.Stamp + ']');
    end;

    procedure EvaluatesAField(): Text
    var
        Member: Record "Kept Member";
    begin
        Evaluate(Member.Stamp, 'abc');
        exit(Member.Stamp);
    end;

    procedure OwnProcedureNamedLikeAStub(): Text
    begin
        exit(AreEqual());
    end;

    local procedure AreEqual(): Text
    begin
        exit('own');
    end;
}
"#;

/// `Clear(X)` resolved to the Library - Variable Storage stub's `Clear`: it
/// reset nothing and emptied the test's variable storage queue. It now resets
/// the variable to its type's default, and a stub catalog answers only a call
/// on its own codeunit.
#[test]
fn clear_resets_the_variable_to_its_default() {
    let call = |proc: &str| {
        ok(run(
            &[
                ("/ws/KeptMember.al", KEPT_MEMBER),
                ("/ws/KeptLog.al", KEPT_LOG),
                ("/ws/ClearCounter.al", CLEAR_COUNTER),
                ("/ws/ClearProbe.al", CLEAR_PROBE),
            ],
            "Clear Probe",
            proc,
            vec![],
        ))
    };
    assert_eq!(call("ClearsScalars"), Value::Text("cleared".into()));
    // "Only the reference to the codeunit is deleted": C gets a new instance
    // (1) and D keeps the old one (2).
    assert_eq!(call("ClearsCodeunitInstance"), Value::Integer(12));
    assert_eq!(call("KeepsTheVariableStorageQueue"), Value::Integer(7));
    // Fields, filters and the table's globals go, the rows stay.
    assert_eq!(call("ClearsARecord"), Value::Integer(2));
    // A record field passed to a `var` parameter takes the value back.
    assert_eq!(call("ClearsAField"), Value::Text("[]".into()));
    assert_eq!(call("EvaluatesAField"), Value::Text("abc".into()));
    assert_eq!(
        call("OwnProcedureNamedLikeAStub"),
        Value::Text("own".into())
    );
}
