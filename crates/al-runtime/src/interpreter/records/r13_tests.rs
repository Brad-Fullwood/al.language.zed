//! Interpreter tests for overloads chosen by the enum type of an argument and
//! for the element type of the lists the runtime builds.

use std::sync::Arc;

use crate::interpreter::dispatch::test_support::ok;
use crate::interpreter::dispatch::{dispatch_call, DispatchCtx};
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;
use crate::test_support::MockSource as Workspace;

fn run(files: &[(&str, &str)], object: &str, proc: &str) -> Eval {
    let ws = Arc::new(Workspace::new());
    for (path, src) in files {
        ws.file_index
            .add_file(std::path::PathBuf::from(path), src.to_string());
    }
    let mut ctx = DispatchCtx::new_with_records(ws, Default::default());
    dispatch_call(Some(object), proc, vec![], &mut ctx)
}

/// Two overloads of `Describe` that differ only by the enum type of their
/// parameter.
const ENUM_OVERLOADS: &str = r#"enum 50396 "R13 Color"
{
    value(0; Red) { }
    value(1; Green) { }
}

enum 50397 "R13 Size"
{
    value(0; Small) { }
    value(1; Large) { }
}

codeunit 50398 "R13 Enum Overloads"
{
    procedure Describe(E: Enum "R13 Color"): Text
    begin
        exit('color');
    end;

    procedure Describe(E: Enum "R13 Size"): Text
    begin
        exit('size');
    end;

    procedure ByEnumType(): Text
    var
        C: Enum "R13 Color";
        S: Enum "R13 Size";
    begin
        C := "R13 Color"::Green;
        S := "R13 Size"::Large;
        exit(Describe(C) + '|' + Describe(S) + '|' + Describe("R13 Size"::Small) + '|' + Describe(Enum::"R13 Color"::Red));
    end;
}
"#;

/// A variable of each enum type and an enum literal of each form run the
/// overload whose parameter names the argument's enum type, as alc chooses.
#[test]
fn an_overload_is_chosen_by_the_enum_type_of_its_argument() {
    assert_eq!(
        ok(run(
            &[("/ws/R13EnumOverloads.al", ENUM_OVERLOADS)],
            "R13 Enum Overloads",
            "ByEnumType"
        )),
        Value::Text("color|size|size|color".into())
    );
}

/// Lists that `Text.Split`, `Enum.Names()`, `Enum.Ordinals()` and
/// `JsonObject.Keys()` return, used with a Code argument. Business Central
/// types them `List of [Text]` and `List of [Integer]`, so a Code argument
/// to `Contains`, `IndexOf` or `Add` is converted to Text first.
const RUNTIME_LISTS: &str = r#"enum 50399 "R13 Upper"
{
    value(0; RED) { }
    value(1; GREEN) { }
}

codeunit 50399 "R13 Runtime Lists"
{
    procedure SplitThenContainsCode(): Text
    var
        L: List of [Text];
        C: Code[20];
        S: Text;
    begin
        S := 'ITEM1|ITEM2';
        L := S.Split('|');
        C := 'ITEM1';
        if L.Contains(C) then
            exit('found');
        exit('missing');
    end;

    procedure SplitThenAddCode(): Integer
    var
        L: List of [Text];
        C: Code[20];
        S: Text;
    begin
        S := 'ITEM1|ITEM2';
        L := S.Split('|');
        C := 'item3';
        L.Add(C);
        exit(L.IndexOf('ITEM3'));
    end;

    procedure SplitIntoParameter(): Text
    var
        C: Code[20];
        S: Text;
    begin
        S := 'ITEM1|ITEM2';
        C := 'item2';
        exit(FindIn(S.Split('|'), C));
    end;

    local procedure FindIn(Parts: List of [Text]; No: Code[20]): Text
    begin
        if Parts.Contains(No) then
            exit('found');
        exit('missing');
    end;

    procedure NamesThenContainsCode(): Text
    var
        L: List of [Text];
        C: Code[20];
    begin
        L := Enum::"R13 Upper".Names();
        C := 'green';
        if L.Contains(C) then
            exit('found');
        exit('missing');
    end;

    procedure JsonKeysThenContainsCode(): Text
    var
        O: JsonObject;
        L: List of [Text];
        C: Code[20];
    begin
        O.Add('ITEM1', 1);
        L := O.Keys();
        C := 'item1';
        if L.Contains(C) then
            exit('found');
        exit('missing');
    end;

    procedure SplitList(): List of [Text]
    var
        S: Text;
    begin
        S := 'a|b';
        exit(S.Split('|'));
    end;

    procedure SplitWithoutSeparatorList(): List of [Text]
    var
        S: Text;
    begin
        S := 'a|b';
        exit(S.Split());
    end;

    procedure NamesList(): List of [Text]
    begin
        exit(Enum::"R13 Upper".Names());
    end;

    procedure OrdinalsList(): List of [Integer]
    begin
        exit(Enum::"R13 Upper".Ordinals());
    end;

    procedure JsonKeysList(): List of [Text]
    var
        O: JsonObject;
    begin
        O.Add('a', 1);
        exit(O.Keys());
    end;

    procedure JsonValuesList(): List of [JsonToken]
    var
        O: JsonObject;
    begin
        O.Add('a', 1);
        exit(O.Values());
    end;
}
"#;

fn run_runtime_lists(proc: &str) -> Value {
    ok(run(
        &[("/ws/R13RuntimeLists.al", RUNTIME_LISTS)],
        "R13 Runtime Lists",
        proc,
    ))
}

/// `L := S.Split('|')` then `L.Contains(C)` with `C: Code[20]` finds the
/// part, as a test that checks a record's number against a split filter
/// string does.
#[test]
fn a_split_list_finds_a_code_argument() {
    assert_eq!(
        run_runtime_lists("SplitThenContainsCode"),
        Value::Text("found".into())
    );
}

/// `L.Add(C)` with `C := 'item3'` stores Text `ITEM3`, so
/// `L.IndexOf('ITEM3')` is 3.
#[test]
fn a_split_list_converts_a_code_it_adds_to_text() {
    assert_eq!(run_runtime_lists("SplitThenAddCode"), Value::Integer(3));
}

/// A split list passed straight to a `List of [Text]` parameter keeps its
/// element type, so `Parts.Contains(No)` with a Code parameter finds it.
#[test]
fn a_split_list_passed_to_a_parameter_finds_a_code_argument() {
    assert_eq!(
        run_runtime_lists("SplitIntoParameter"),
        Value::Text("found".into())
    );
}

/// `Enum.Names()` and `JsonObject.Keys()` are lists of Text as well.
#[test]
fn enum_names_and_json_keys_find_a_code_argument() {
    assert_eq!(
        run_runtime_lists("NamesThenContainsCode"),
        Value::Text("found".into())
    );
    assert_eq!(
        run_runtime_lists("JsonKeysThenContainsCode"),
        Value::Text("found".into())
    );
}

/// Each list the runtime builds carries the element type Business Central
/// gives it.
#[test]
fn the_lists_the_runtime_builds_carry_their_element_type() {
    for (proc, element_type) in [
        ("SplitList", "Text"),
        ("SplitWithoutSeparatorList", "Text"),
        ("NamesList", "Text"),
        ("OrdinalsList", "Integer"),
        ("JsonKeysList", "Text"),
        ("JsonValuesList", "JsonToken"),
    ] {
        match run_runtime_lists(proc) {
            Value::List(list) => assert_eq!(list.member_type(), Some(element_type), "{proc}"),
            other => panic!("{proc} returned {other:?}"),
        }
    }
}
