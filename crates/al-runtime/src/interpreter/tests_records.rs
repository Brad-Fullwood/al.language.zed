//! End-to-end interpreter tests for B5 (workspace procedure dispatch) and
//! B6 (records in tests, wired to the mock store).
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
    }
}

// ───────────────────────────────── B6: records ─────────────────────────────

#[test]
fn b6_init_set_insert_get_field_get() {
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
fn b6_get_missing_returns_false() {
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
fn b6_field_assignment_then_readback() {
    // Field set/get round-trips a non-PK field through the buffer.
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
    // 19 is an integer literal assigned to a Decimal field — the mock stores the
    // value as written (the interpreter does not coerce numeric field types).
    assert_eq!(ok(r), Value::Integer(19));
}

#[test]
fn b6_insert_duplicate_key_errors() {
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
fn b6_asserterror_catches_duplicate_insert() {
    // The duplicate-insert error is catchable by `asserterror`, the way a BC
    // test would assert it.
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
fn b6_setrange_then_count() {
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
fn b6_setrange_single_value_exact() {
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
fn b6_setfilter_then_count() {
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
fn b6_findset_next_iteration_sums_amount() {
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
    // Amounts 10+20+30+40 = 100. i*10 with Integer i yields Integer values; the
    // running `total` starts at Integer(0) so the sum stays Integer(100).
    assert_eq!(ok(r), Value::Integer(100));
}

#[test]
fn b6_findfirst_reads_lowest_key() {
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
fn b6_isempty_true_then_false() {
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
fn b6_delete_then_count() {
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
fn b6_deleteall_empties_table() {
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
fn b6_modify_updates_row() {
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
fn b6_unknown_table_errors_gracefully() {
    // A record of a table that is not in the workspace must produce a clear
    // error rather than a panic.
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
    let r = run(&[("/ws/CustTests.al", cu)], "Cust Tests", "UseCustomer", vec![]);
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
fn b6_two_record_vars_share_physical_table() {
    // Inserting through one variable and reading through another variable of the
    // same table reflects the shared physical table (records keyed by table name).
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

// ──────────────────────────── B5: procedure dispatch ───────────────────────

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
fn b5_codeunit_variable_dispatch_executes_real_body() {
    // `lib.Add(2, 3)` where `lib: Codeunit "Math Lib"` resolves the variable's
    // declared subtype to the workspace object and runs the real procedure body.
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
fn b5_codeunit_variable_nested_calls() {
    // Two codeunit-variable calls composed in one expression.
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
fn b5_rhs_expression_position_cross_object_dispatch() {
    // Calling another object's procedure by name in RHS-expression position.
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
fn b5_unresolved_codeunit_object_is_graceful_error() {
    // A codeunit variable whose subtype object is missing from the workspace
    // produces a clear error, not a panic.
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

// ──────────────────────────── List of [T] (W2-08) ──────────────────────────

#[test]
fn b6_list_add_get_count_via_dispatch() {
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
fn b6_list_get_returns_element() {
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
fn b6_list_contains() {
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
