#!/usr/bin/env python3
"""Build an AL corpus with deliberately planted defects — one project per case.

Isolation matters. An earlier version of this corpus put every case in a single
project, and the three syntax errors aborted compilation before the binder ever
ran, so alc scored 3/13 and looked far weaker than it is. A compiler that stops
at parse errors is behaving correctly; the benchmark was wrong. Each case now
gets its own project so every defect is reached.

Each case records the line its defect sits on, so scoring can require the
diagnostic to actually land on the defect rather than merely appear somewhere
in the file.

Cases must contain EXACTLY the planted defect and nothing else. An earlier
revision named every codeunit method `Run`, which collides with the built-in
`Codeunit.Run` and produced AL0440 in all fourteen files — alc reported that
instead of the planted defect and scored 8/14 for the wrong reasons. Methods
are named `Execute` for this reason. Any change here should be re-validated by
confirming the control case compiles clean.
"""
import json
import os
import shutil

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)
SRC_PKGS = os.environ.get("AL_BENCH_PACKAGES", "")
PKG_SET = [
    "System.app",
    "Microsoft_System_28.0.51202.0.app",
    "Microsoft_System Application_28.1.49838.50794.app",
    "Microsoft_Business Foundation_28.1.49838.50065.app",
    "Microsoft_Base Application_28.1.49838.51422.app",
    "Microsoft_Application_28.1.49838.50065.app",
]

CASES = [
    # ---- syntactic: any parser should catch these ----
    dict(id="syn_missing_semicolon", cls="syntax", detects="missing ';'",
         files={"Case.al": '''codeunit 50100 "Syn Missing Semi"
{
    procedure Bad()
    var
        X: Integer;
    begin
        X := 1
        X := 2;
    end;
}
'''}, defect_line=7),

    dict(id="syn_unclosed_brace", cls="syntax", detects="unbalanced braces / unexpected EOF",
         files={"Case.al": '''codeunit 50101 "Syn Unclosed"
{
    procedure Bad()
    begin
        Message('hi');
    end;
'''}, defect_line=6),

    dict(id="syn_bad_property", cls="syntax", detects="invalid property name",
         files={"Case.al": '''table 50102 "Syn Bad Property"
{
    fields
    {
        field(1; "Entry No."; Integer)
        {
            DataClassificationX = CustomerContent;
        }
    }
}
'''}, defect_line=7),

    # ---- name resolution: needs a symbol table ----
    dict(id="sem_undeclared_var", cls="binding", detects="undeclared identifier",
         files={"Case.al": '''codeunit 50110 "Sem Undeclared Var"
{
    procedure Execute()
    var
        Known: Integer;
    begin
        Known := 1;
        Unknown := Known + 1;
    end;
}
'''}, defect_line=8),

    dict(id="sem_unknown_object", cls="binding", detects="unknown object reference",
         files={"Case.al": '''codeunit 50111 "Sem Unknown Object"
{
    procedure Execute()
    var
        Rec: Record "This Table Does Not Exist At All";
    begin
        Rec.Init();
    end;
}
'''}, defect_line=5),

    dict(id="sem_unknown_field", cls="binding", detects="unknown field on a known record",
         files={"Case.al": '''codeunit 50112 "Sem Unknown Field"
{
    procedure Execute()
    var
        Cust: Record Customer;
    begin
        Cust."No." := '10000';
        Cust."Totally Bogus Field" := 1;
    end;
}
'''}, defect_line=8),

    dict(id="sem_unknown_method", cls="binding", detects="unknown method on a known record",
         files={"Case.al": '''codeunit 50113 "Sem Unknown Method"
{
    procedure Execute()
    var
        Cust: Record Customer;
    begin
        Cust.NoSuchMethodHere();
    end;
}
'''}, defect_line=7),

    dict(id="sem_unknown_procedure", cls="binding", detects="call to undefined procedure",
         files={"Case.al": '''codeunit 50114 "Sem Unknown Procedure"
{
    procedure Execute()
    begin
        ThisProcedureWasNeverDefined(42);
    end;
}
'''}, defect_line=5),

    # ---- type checking: needs expression types ----
    dict(id="typ_assign_mismatch", cls="typecheck", detects="type mismatch in assignment",
         files={"Case.al": '''codeunit 50120 "Typ Assign Mismatch"
{
    procedure Execute()
    var
        N: Integer;
        R: Record Customer;
    begin
        N := R;
    end;
}
'''}, defect_line=8),

    dict(id="typ_arg_count", cls="typecheck", detects="wrong argument count",
         files={"Case.al": '''codeunit 50121 "Typ Arg Count"
{
    procedure Execute()
    begin
        Helper(1, 2, 3);
    end;

    local procedure Helper(A: Integer)
    begin
        Message(Format(A));
    end;
}
'''}, defect_line=5),

    dict(id="typ_arg_type", cls="typecheck", detects="wrong argument type",
         files={"Case.al": '''codeunit 50122 "Typ Arg Type"
{
    procedure Execute()
    var
        R: Record Customer;
    begin
        Helper(R);
    end;

    local procedure Helper(A: Integer)
    begin
        Message(Format(A));
    end;
}
'''}, defect_line=7),

    dict(id="typ_missing_return", cls="typecheck", detects="not all code paths return a value",
         files={"Case.al": '''codeunit 50123 "Typ Missing Return"
{
    procedure Compute(X: Integer): Integer
    begin
        if X > 0 then
            exit(X);
    end;
}
'''}, defect_line=3),

    # ---- project rules: need whole-workspace knowledge ----
    dict(id="prj_id_out_of_range", cls="project", detects="object ID outside app.json idRanges",
         files={"Case.al": '''codeunit 90000 "Prj Out Of Range"
{
    procedure Execute()
    begin
        Message('id 90000 is outside the declared 50000-69999 range');
    end;
}
'''}, defect_line=1),

    dict(id="prj_duplicate_id", cls="project", detects="duplicate object ID across files",
         files={
             "First.al": '''codeunit 50140 "Prj Dup First"
{
    procedure Execute()
    begin
        Message('first');
    end;
}
''',
             "Second.al": '''codeunit 50140 "Prj Dup Second"
{
    procedure Execute()
    begin
        Message('second — same ID as First.al');
    end;
}
''',
         }, defect_line=1, defect_file="Second.al"),

    # ---- control: reporting anything here is a false positive ----
    dict(id="ok_clean", cls="control", detects="NOTHING — must report zero errors",
         files={"Case.al": '''codeunit 50130 "Ok Clean"
{
    procedure Execute()
    var
        Cust: Record Customer;
        Total: Decimal;
    begin
        if Cust.FindSet() then
            repeat
                Total += Cust."Balance (LCY)";
            until Cust.Next() = 0;
        Message(Format(Total));
    end;
}
'''}, defect_line=None),
]


def app_json(idx, name):
    return {
        "id": f"d0000000-0000-0000-0000-0000000001{idx:02d}",
        "name": f"AL Bench Accuracy {name}",
        "publisher": "Bench",
        "version": "1.0.0.0",
        "platform": "28.0.0.0",
        "application": "28.0.0.0",
        "runtime": "17.0",
        "idRanges": [{"from": 50000, "to": 69999}],
    }


def main():
    root = os.path.join(BENCH, "projects", "accuracy")
    if os.path.isdir(root):
        shutil.rmtree(root)
    os.makedirs(root)

    # One shared symbol set: 60 MB copied once, referenced by every case via
    # /packagecachepath rather than duplicated 15 times.
    shared = os.path.join(root, "_packages")
    os.makedirs(shared)
    for n in PKG_SET:
        s = os.path.join(SRC_PKGS, n)
        if os.path.isfile(s):
            shutil.copy2(s, os.path.join(shared, n))
        else:
            print(f"  WARN missing package: {n}")

    manifest = []
    for i, c in enumerate(CASES):
        proj = os.path.join(root, "case_" + c["id"])
        src = os.path.join(proj, "src")
        os.makedirs(src)
        with open(os.path.join(proj, "app.json"), "w") as fh:
            json.dump(app_json(i, c["id"]), fh, indent=2)
        # Each case points at the shared package set.
        os.symlink(shared, os.path.join(proj, ".alpackages"))
        for fn, body in c["files"].items():
            with open(os.path.join(src, fn), "w") as fh:
                fh.write(body)
        manifest.append({
            "id": c["id"], "class": c["cls"], "expect": c["detects"],
            "project": proj,
            "files": sorted(c["files"]),
            "defect_file": c.get("defect_file", sorted(c["files"])[0]),
            "defect_line": c["defect_line"],
        })

    with open(os.path.join(root, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)

    print(f"wrote {len(CASES)} isolated case projects to {root}")
    for m in manifest:
        print(f"  {m['class']:<10} {m['id']:<24} line {m['defect_line']}  {m['expect']}")


if __name__ == "__main__":
    main()
