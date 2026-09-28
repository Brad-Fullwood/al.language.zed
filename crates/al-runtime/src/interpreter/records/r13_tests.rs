//! Interpreter tests for the round 13 runtime findings on overloads by enum
//! type and on the element type of the lists the runtime builds.

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
