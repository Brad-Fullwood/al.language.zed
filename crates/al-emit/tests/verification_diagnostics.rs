//! Each native verification diagnostic, driven the way a user reaches it: a project on
//! disk goes into `build_verified_app_from_project`, and the diagnostic codes come out.
//!
//! `crates/al-emit/src/verification.rs` was the largest block of uncovered
//! user-input-driven logic in the workspace (76% of lines). Most of the gap was the
//! diagnostic arms themselves: the conditions were reachable but nothing exercised them.

use al_emit::build_verified_app_from_project;
use std::path::Path;

const APP_JSON: &str = r#"{
  "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
  "name": "VerifyApp",
  "publisher": "Pub",
  "version": "1.0.0.0",
  "brief": "",
  "description": "",
  "platform": "1.0.0.0",
  "application": "24.0.0.0",
  "runtime": "13.0",
  "idRanges": [ { "from": 50100, "to": 50199 } ],
  "dependencies": []
}
"#;

/// Write a one-file project and return every diagnostic code the verifier reports.
fn codes_for(sources: &[(&str, &str)]) -> Vec<String> {
    codes_for_with_manifest(APP_JSON, sources)
}

fn codes_for_with_manifest(app_json: &str, sources: &[(&str, &str)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), app_json, sources);
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification must run even when the project is wrong");
    let mut codes: Vec<String> = built
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

fn write_project(dir: &Path, app_json: &str, sources: &[(&str, &str)]) {
    std::fs::write(dir.join("app.json"), app_json).unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (name, text) in sources {
        std::fs::write(src.join(name), text).unwrap();
    }
}

fn assert_reports(codes: &[String], wanted: &str) {
    assert!(
        codes.iter().any(|c| c == wanted),
        "expected {wanted}, got {codes:?}"
    );
}

fn assert_silent(codes: &[String], unwanted: &str) {
    assert!(
        !codes.iter().any(|c| c == unwanted),
        "unexpected {unwanted} in {codes:?}"
    );
}

/// A table whose fields the negative cases refer to.
const CUSTOMER_TABLE: (&str, &str) = (
    "Customer.Table.al",
    "table 50100 \"Prop Customer\"\n\
     {\n\
     \x20   DataClassification = CustomerContent;\n\
     \n\
     \x20   fields\n\
     \x20   {\n\
     \x20       field(1; \"No.\"; Code[20])\n\
     \x20       {\n\
     \x20           DataClassification = CustomerContent;\n\
     \x20       }\n\
     \x20   }\n\
     \n\
     \x20   keys\n\
     \x20   {\n\
     \x20       key(PK; \"No.\")\n\
     \x20       {\n\
     \x20           Clustered = true;\n\
     \x20       }\n\
     \x20   }\n\
     }\n",
);

/// A clean project reports nothing, so every positive case below is the code under test
/// rather than background noise.
#[test]
fn a_correct_project_verifies_clean() {
    let codes = codes_for(&[
        CUSTOMER_TABLE,
        (
            "Helper.Codeunit.al",
            "codeunit 50101 \"Prop Helper\"\n\
             {\n\
             \x20   procedure Total(): Integer\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Customer.Reset();\n\
             \x20       exit(1);\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert!(codes.is_empty(), "clean project reported {codes:?}");
}

#[test]
fn aln2405_field_property_typo() {
    let codes = codes_for(&[(
        "T.Table.al",
        "table 50100 T\n{\n    fields\n    {\n        field(1; A; Integer)\n        {\n            DataClassificationn = CustomerContent;\n        }\n    }\n}\n",
    )]);
    assert_reports(&codes, "ALN2405");
}

#[test]
fn aln2401_unknown_record_subtype() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    var\n        Missing: Record \"No Such Table\";\n    begin\n        Missing.Reset();\n    end;\n}\n",
    )]);
    assert_reports(&codes, "ALN2401");
}

#[test]
fn al0118_undeclared_identifier_on_the_left_of_an_assignment() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        Nowhere := 1;\n    end;\n}\n",
    )]);
    assert_reports(&codes, "AL0118");
}

#[test]
fn aln2403_assigning_a_record_to_an_integer() {
    let codes = codes_for(&[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Count: Integer;\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Count := Customer;\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_reports(&codes, "ALN2403");
}

#[test]
fn al0132_unknown_field_on_a_known_record() {
    let codes = codes_for(&[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Customer.\"Not A Field\" := '';\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_reports(&codes, "AL0132");
    // The field that does exist must not be reported.
    let clean = codes_for(&[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Customer.\"No.\" := '';\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_silent(&clean, "AL0132");
}

/// A field a table extension adds belongs to the table it extends. The
/// verifier only read fields from `table` objects, so every per-tenant
/// extension that wrote to its own field on a base table failed to build.
#[test]
fn al0132_accepts_a_field_added_by_a_table_extension() {
    let codes = codes_for(&[
        CUSTOMER_TABLE,
        (
            "Ext.TableExt.al",
            "tableextension 50102 \"Prop Customer Ext\" extends \"Prop Customer\"\n\
             {\n\
             \x20   fields\n\
             \x20   {\n\
             \x20       field(50100; \"Loyalty Tier\"; Code[10]) { DataClassification = CustomerContent; }\n\
             \x20   }\n\
             }\n",
        ),
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Customer.\"Loyalty Tier\" := '';\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_silent(&codes, "AL0132");
}

#[test]
fn aln2209_unknown_method_on_a_record_and_unknown_local_call() {
    let on_record = codes_for(&[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       Customer.NotARecordMethod();\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_reports(&on_record, "ALN2209");

    let unqualified = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        NoSuchLocalProcedure();\n    end;\n}\n",
    )]);
    assert_reports(&unqualified, "ALN2209");
}

/// A table's own procedures, and those its extensions add, are methods of a
/// Record of that table: `Cust.SelectCustomer(Cust);` was refused as
/// unknown, so no extension calling a table procedure could be built.
#[test]
fn aln2209_accepts_procedures_the_table_and_its_extensions_declare() {
    let codes = codes_for(&[
        (
            "T.Table.al",
            "table 50100 \"Proc Table\"\n\
             {\n\
             \x20   fields { field(1; \"No.\"; Code[20]) { } }\n\
             \x20   procedure SelectIt(var Rec: Record \"Proc Table\")\n\
             \x20   begin\n\
             \x20   end;\n\
             }\n",
        ),
        (
            "T.TableExt.al",
            "tableextension 50101 \"Proc Ext\" extends \"Proc Table\"\n\
             {\n\
             \x20   procedure HasAddress(): Boolean\n\
             \x20   begin\n\
             \x20       exit(true);\n\
             \x20   end;\n\
             }\n",
        ),
        (
            "C.Codeunit.al",
            "codeunit 50102 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   var\n\
             \x20       Rec: Record \"Proc Table\";\n\
             \x20   begin\n\
             \x20       Rec.SelectIt(Rec);\n\
             \x20       Rec.HasAddress();\n\
             \x20   end;\n\
             }\n",
        ),
    ]);
    assert_silent(&codes, "ALN2209");
}

#[test]
fn commented_call_is_not_a_call_site() {
    let source = r#"codeunit 50100 C
{
    local procedure Init()
    begin
    end;

    procedure P()
    var
        Txt: Text;
    begin
        // Init (when empty)
        Txt := 'Init (x)';
    end;
}
"#;
    let codes = codes_for(&[("C.Codeunit.al", source)]);
    assert_silent(&codes, "ALN2201");
}

#[test]
fn real_arity_mismatch_still_reported() {
    let source = r#"codeunit 50100 C
{
    local procedure Init()
    begin
    end;

    procedure P()
    var
        Txt: Text;
    begin
        Init(1);
    end;
}
"#;
    let codes = codes_for(&[("C.Codeunit.al", source)]);
    assert_reports(&codes, "ALN2201");
}

#[test]
fn aln2203_and_aln2204_missing_and_valueless_exit_in_a_returning_procedure() {
    let missing = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P(): Integer\n    begin\n    end;\n}\n",
    )]);
    assert_reports(&missing, "ALN2203");

    let valueless = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P(): Integer\n    begin\n        exit();\n    end;\n}\n",
    )]);
    assert_reports(&valueless, "ALN2204");
}

#[test]
fn aln2206_exit_with_a_value_from_a_procedure_that_returns_nothing() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        exit(1);\n    end;\n}\n",
    )]);
    assert_reports(&codes, "ALN2206");
}

#[test]
fn aln2207_and_aln2208_break_and_continue_outside_a_loop() {
    let brk = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        break;\n    end;\n}\n",
    )]);
    assert_reports(&brk, "ALN2207");

    let cont = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        continue;\n    end;\n}\n",
    )]);
    assert_reports(&cont, "ALN2208");

    // Inside a loop, both are fine.
    let in_loop = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    var\n        I: Integer;\n    begin\n        for I := 1 to 10 do begin\n            break;\n        end;\n    end;\n}\n",
    )]);
    assert_silent(&in_loop, "ALN2207");
}

#[test]
fn aln1003_object_id_outside_the_manifest_ranges() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 60000 C\n{\n    procedure P()\n    begin\n    end;\n}\n",
    )]);
    assert_reports(&codes, "ALN1003");
}

#[test]
fn aln1001_and_aln1002_duplicate_object_id_and_name() {
    let duplicate_id = codes_for(&[
        (
            "A.Codeunit.al",
            "codeunit 50100 A\n{\n    procedure P()\n    begin\n    end;\n}\n",
        ),
        (
            "B.Codeunit.al",
            "codeunit 50100 B\n{\n    procedure P()\n    begin\n    end;\n}\n",
        ),
    ]);
    assert_reports(&duplicate_id, "ALN1001");

    let duplicate_name = codes_for(&[
        (
            "A.Codeunit.al",
            "codeunit 50100 Same\n{\n    procedure P()\n    begin\n    end;\n}\n",
        ),
        (
            "B.Codeunit.al",
            "codeunit 50101 Same\n{\n    procedure P()\n    begin\n    end;\n}\n",
        ),
    ]);
    assert_reports(&duplicate_name, "ALN1002");
}

#[test]
fn aln1105_duplicate_procedure_signature() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P(A: Integer)\n    begin\n    end;\n\n    procedure P(B: Integer)\n    begin\n    end;\n}\n",
    )]);
    assert_reports(&codes, "ALN1105");
}

#[test]
fn aln11xx_duplicate_table_members() {
    let duplicate_field_id = codes_for(&[(
        "T.Table.al",
        "table 50100 T\n{\n    fields\n    {\n        field(1; A; Integer)\n        {\n        }\n        field(1; B; Integer)\n        {\n        }\n    }\n}\n",
    )]);
    assert!(
        duplicate_field_id.iter().any(|c| c.starts_with("ALN11")),
        "expected a duplicate-member diagnostic, got {duplicate_field_id:?}"
    );

    let duplicate_field_name = codes_for(&[(
        "T.Table.al",
        "table 50100 T\n{\n    fields\n    {\n        field(1; A; Integer)\n        {\n        }\n        field(2; A; Integer)\n        {\n        }\n    }\n}\n",
    )]);
    assert!(
        duplicate_field_name.iter().any(|c| c.starts_with("ALN11")),
        "expected a duplicate-member diagnostic, got {duplicate_field_name:?}"
    );
}

/// Manifest diagnostics. `app.json` is user input too, and a broken one has to produce a
/// diagnostic rather than a panic or a silent build.
#[test]
fn aln01xx_manifest_problems_are_reported() {
    let cases = [
        // No id.
        r#"{ "name": "A", "publisher": "P", "version": "1.0.0.0", "runtime": "13.0", "idRanges": [ { "from": 50100, "to": 50199 } ] }"#,
        // Not a GUID.
        r#"{ "id": "not-a-guid", "name": "A", "publisher": "P", "version": "1.0.0.0", "runtime": "13.0", "idRanges": [ { "from": 50100, "to": 50199 } ] }"#,
        // No name.
        r#"{ "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "publisher": "P", "version": "1.0.0.0", "runtime": "13.0", "idRanges": [ { "from": 50100, "to": 50199 } ] }"#,
        // Version is not four parts.
        r#"{ "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "name": "A", "publisher": "P", "version": "1.0", "runtime": "13.0", "idRanges": [ { "from": 50100, "to": 50199 } ] }"#,
        // Backwards id range.
        r#"{ "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "name": "A", "publisher": "P", "version": "1.0.0.0", "runtime": "13.0", "idRanges": [ { "from": 50199, "to": 50100 } ] }"#,
    ];
    for manifest in cases {
        let codes = codes_for_with_manifest(manifest, &[]);
        assert!(
            codes.iter().any(|c| c.starts_with("ALN01")),
            "manifest {manifest} produced {codes:?}"
        );
    }
}

/// A project whose manifest is not JSON at all must not produce an `.app`.
#[test]
fn an_unparseable_manifest_does_not_produce_an_app() {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), "{ not json", &[]);
    match build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z") {
        Err(_) => {}
        Ok(built) => {
            assert!(
                built.app.is_none(),
                "a malformed app.json produced an .app anyway"
            );
            assert!(
                !built.diagnostics.is_empty(),
                "a malformed app.json produced neither an error nor a diagnostic"
            );
        }
    }
}

/// Verification is deterministic: the same project reports the same codes in the same
/// order, whatever the file system hands back.
#[test]
fn verification_is_deterministic() {
    let sources: &[(&str, &str)] = &[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50101 C\n\
             {\n\
             \x20   procedure P()\n\
             \x20   begin\n\
             \x20       Nowhere := 1;\n\
             \x20       exit(1);\n\
             \x20   end;\n\
             }\n",
        ),
    ];
    assert_eq!(codes_for(sources), codes_for(sources));
}

/// A named return value is returned by assigning it: `Result := ...` was an
/// undeclared identifier and the missing `exit(...)` an error.
#[test]
fn a_named_return_value_is_declared_and_needs_no_exit() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure Describe(Value: Integer) Result: Text\n    begin\n        Result := Format(Value);\n        Value := 2;\n    end;\n}\n",
    )]);
    assert_silent(&codes, "AL0118");
    assert_silent(&codes, "ALN2203");
}

/// `Token.AsValue().AsText()` calls AsText on the value AsValue returns,
/// not a local procedure named AsText.
#[test]
fn a_method_on_a_call_result_is_not_an_unknown_local_procedure() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure Name(): Text\n    var\n        Token: JsonToken;\n    begin\n        exit(Token.AsValue().AsText());\n    end;\n}\n",
    )]);
    assert_silent(&codes, "ALN2209");
}

/// `Record "X" temporary` names table X.
#[test]
fn a_temporary_record_of_a_known_table_is_known() {
    let codes = codes_for(&[
        CUSTOMER_TABLE,
        (
            "C.Codeunit.al",
            "codeunit 50100 C\n{\n    procedure P()\n    var\n        TempCust: Record \"Prop Customer\" temporary;\n    begin\n        TempCust.Init();\n    end;\n}\n",
        ),
    ]);
    assert_silent(&codes, "ALN2401");
}

/// Every object of a multi-object file is bound, so an error in each object
/// is reported on that object's own line.
#[test]
fn errors_in_each_object_of_a_multi_object_file_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        APP_JSON,
        &[(
            "Two.al",
            "codeunit 50100 A\n{\n    procedure P()\n    begin\n        Nowhere := 1;\n    end;\n}\n\ncodeunit 50101 B\n{\n    procedure Q()\n    begin\n        Elsewhere := 1;\n    end;\n}\n",
        )],
    );
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification runs");
    let mut lines: Vec<_> = built
        .diagnostics
        .iter()
        .filter(|d| d.code == "AL0118")
        .map(|d| d.line)
        .collect();
    lines.sort();
    assert_eq!(lines, [5, 13], "{:?}", built.diagnostics);
}

#[test]
fn aln2501_a_missing_layout_file_is_reported_on_the_report() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        APP_JSON,
        &[(
            "R.Report.al",
            "report 50100 R\n{\n    DefaultRenderingLayout = L;\n    dataset { }\n    rendering\n    {\n        layout(L)\n        {\n            Type = Excel;\n            LayoutFile = 'Missing.xlsx';\n        }\n    }\n}\n",
        )],
    );
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("a missing layout is a diagnostic, not a build failure");
    let missing = built
        .diagnostics
        .iter()
        .find(|d| d.code == "ALN2501")
        .unwrap_or_else(|| panic!("{:?}", built.diagnostics));
    assert!(missing.file.ends_with("R.Report.al"), "{missing:?}");
    assert_eq!(missing.line, 10);
}

// ---------------------------------------------------------------------------
// Valid AL that the text checks once refused (audit finding emitter-01).
// Each case is a small synthetic project. None may produce an error, and the
// heuristic checks that stay may only warn.
// ---------------------------------------------------------------------------

fn errors_for(sources: &[(&str, &str)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), APP_JSON, sources);
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification must run");
    built
        .diagnostics
        .iter()
        .filter(|d| d.severity == al_emit::VerificationSeverity::Error)
        .map(|d| format!("{} {}", d.code, d.message))
        .collect()
}

fn assert_no_errors(sources: &[(&str, &str)]) {
    let errors = errors_for(sources);
    assert!(errors.is_empty(), "valid AL was refused: {errors:?}");
}

#[test]
fn a_parenthesised_condition_after_a_keyword_is_not_a_call() {
    assert_no_errors(&[
        CUSTOMER_TABLE,
        (
            "Cond.Codeunit.al",
            "codeunit 50102 \"Prop Cond\"\n{\n\
             \x20   procedure Run(Value: Integer)\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20   begin\n\
             \x20       if (Value > 1) and (Value < 5) or (Value = 9) then\n\
             \x20           Customer.Reset();\n\
             \x20       case (Value) of\n\
             \x20           1:\n\
             \x20               Customer.Reset();\n\
             \x20       end;\n\
             \x20       repeat\n\
             \x20           Value += 1;\n\
             \x20       until (Value > 10);\n\
             \x20   end;\n}\n",
        ),
    ]);
}

#[test]
fn a_named_return_value_needs_no_exit() {
    assert_no_errors(&[(
        "Named.Codeunit.al",
        "codeunit 50103 \"Prop Named\"\n{\n\
         \x20   procedure Names() Result: List of [Code[50]]\n\
         \x20   begin\n\
         \x20       Result.Add('A');\n\
         \x20   end;\n}\n",
    )]);
}

#[test]
fn overloads_that_differ_by_type_are_not_ambiguous_errors() {
    assert_no_errors(&[
        CUSTOMER_TABLE,
        (
            "Over.Codeunit.al",
            "codeunit 50104 \"Prop Over\"\n{\n\
             \x20   procedure Caller()\n\
             \x20   var\n\
             \x20       Customer: Record \"Prop Customer\";\n\
             \x20       Text: Text;\n\
             \x20   begin\n\
             \x20       Build(Customer);\n\
             \x20       Build(Text);\n\
             \x20   end;\n\
             \n\
             \x20   local procedure Build(Customer: Record \"Prop Customer\")\n\
             \x20   begin\n\
             \x20   end;\n\
             \n\
             \x20   local procedure Build(Text: Text)\n\
             \x20   begin\n\
             \x20   end;\n}\n",
        ),
    ]);
}

#[test]
fn a_field_name_used_as_an_identifier_in_a_field_trigger_is_accepted() {
    assert_no_errors(&[(
        "Setup.Table.al",
        "table 50105 \"Prop Setup\"\n{\n\
         \x20   fields\n\
         \x20   {\n\
         \x20       field(1; Code; Code[10]) { }\n\
         \x20       field(2; Expires; DateTime)\n\
         \x20       {\n\
         \x20           trigger OnValidate()\n\
         \x20           begin\n\
         \x20               Expires := 0DT;\n\
         \x20           end;\n\
         \x20       }\n\
         \x20   }\n}\n",
    )]);
}

#[test]
fn a_record_parameter_named_rec_resolves_to_its_own_table() {
    assert_no_errors(&[
        (
            "Alpha.Table.al",
            "table 50106 \"Prop Alpha Line\"\n{\n\
             \x20   fields\n\
             \x20   {\n\
             \x20       field(1; \"Alpha Only\"; Code[10]) { }\n\
             \x20   }\n}\n",
        ),
        (
            "Beta.Table.al",
            "table 50107 \"Prop Beta Line\"\n{\n\
             \x20   fields\n\
             \x20   {\n\
             \x20       field(1; \"Beta Only\"; Code[10]) { }\n\
             \x20   }\n}\n",
        ),
        (
            "Rec.Codeunit.al",
            "codeunit 50108 \"Prop Rec\"\n{\n\
             \x20   procedure Read(Rec: Record \"Prop Beta Line\"): Code[10]\n\
             \x20   begin\n\
             \x20       exit(Rec.\"Beta Only\");\n\
             \x20   end;\n}\n",
        ),
    ]);
}

#[test]
fn continue_inside_a_loop_or_text_is_accepted() {
    assert_no_errors(&[(
        "Loop.Codeunit.al",
        "codeunit 50109 \"Prop Loop\"\n{\n\
         \x20   procedure Run(): Boolean\n\
         \x20   var\n\
         \x20       i: Integer;\n\
         \x20   begin\n\
         \x20       for i := 1 to 3 do begin\n\
         \x20           if i = 2 then\n\
         \x20               continue;\n\
         \x20       end;\n\
         \x20       exit(Confirm('Do you wish to continue?', false));\n\
         \x20   end;\n}\n",
    )]);
}

#[test]
fn built_in_methods_outside_the_record_catalog_are_accepted() {
    assert_no_errors(&[(
        "Builtin.Codeunit.al",
        "codeunit 50110 \"Prop Builtin\"\n{\n\
         \x20   procedure Run(Source: InStream)\n\
         \x20   var\n\
         \x20       Target: OutStream;\n\
         \x20       Blob: Codeunit \"Temp Blob\";\n\
         \x20   begin\n\
         \x20       Blob.CreateOutStream(Target);\n\
         \x20       CopyStream(Target, Source);\n\
         \x20       BindSubscription(this);\n\
         \x20       UnbindSubscription(this);\n\
         \x20   end;\n}\n",
    )]);
}

#[test]
fn a_logo_outside_res_and_a_dot_slash_layout_path_package_like_alc() {
    let dir = tempfile::tempdir().unwrap();
    let app_json = APP_JSON.replace(
        "\"dependencies\": []",
        "\"logo\": \"Images/logo.png\",\n  \"dependencies\": []",
    );
    write_project(
        dir.path(),
        &app_json,
        &[(
            "Rep.Report.al",
            "report 50111 \"Prop Rep\"\n{\n\
             \x20   rendering\n\
             \x20   {\n\
             \x20       layout(Sheet)\n\
             \x20       {\n\
             \x20           Type = Excel;\n\
             \x20           LayoutFile = './Layouts/Sheet.xlsx';\n\
             \x20       }\n\
             \x20   }\n}\n",
        )],
    );
    std::fs::create_dir_all(dir.path().join("Images")).unwrap();
    std::fs::write(dir.path().join("Images/logo.png"), b"PNG").unwrap();
    std::fs::create_dir_all(dir.path().join("Layouts")).unwrap();
    std::fs::write(dir.path().join("Layouts/Sheet.xlsx"), b"XLSX").unwrap();
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("build runs");
    let errors: Vec<_> = built
        .diagnostics
        .iter()
        .filter(|d| d.severity == al_emit::VerificationSeverity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    let app = built.app.expect("package is produced");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(app.bytes[40..].to_vec())).unwrap();
    // alc 18.1 names these `logo/logo.png` and `layout/Layouts/Sheet.xlsx`.
    assert!(archive.by_name("logo/logo.png").is_ok());
    assert!(archive.by_name("layout/Layouts/Sheet.xlsx").is_ok());
}

// ---------------------------------------------------------------------------
// Heuristic checks that only warn (ALN2204 to ALN2210 are in HEURISTIC_CODES).
// ---------------------------------------------------------------------------

/// Build a project and return each diagnostic as (code, severity), and whether the .app was written.
fn build_summary(sources: &[(&str, &str)]) -> (Vec<(String, al_emit::VerificationSeverity)>, bool) {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), APP_JSON, sources);
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification must run");
    let diagnostics = built
        .diagnostics
        .iter()
        .map(|d| (d.code.to_string(), d.severity))
        .collect();
    (diagnostics, built.app.is_some())
}

#[test]
fn implicit_false_return_only_warns() {
    let (diagnostics, written) = build_summary(&[(
        "Implicit.Codeunit.al",
        r#"codeunit 50100 "Prop Implicit"
{
    procedure Check(Value: Integer): Boolean
    begin
        if Value < 0 then
            exit(true);
    end;
}
"#,
    )]);
    assert!(
        diagnostics.contains(&(
            "ALN2210".to_string(),
            al_emit::VerificationSeverity::Warning
        )),
        "expected ALN2210 as a warning: {diagnostics:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|(_, severity)| *severity == al_emit::VerificationSeverity::Error),
        "{diagnostics:?}"
    );
    assert!(written, "{diagnostics:?}");
}

#[test]
fn variable_redeclared_with_another_type_only_warns() {
    // variable_types keeps one map per object, so procedure B's Record declaration
    // of Cust reaches procedure A, where Cust is an Integer.
    let (diagnostics, written) = build_summary(&[
        (
            "Customer.Table.al",
            r#"table 50100 Customer
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}
"#,
        ),
        (
            "Redeclared.Codeunit.al",
            r#"codeunit 50101 "Prop Redeclared"
{
    procedure A()
    var
        Cust: Integer;
        N: Integer;
    begin
        N := Cust;
    end;

    procedure B()
    var
        Cust: Record Customer;
    begin
    end;
}
"#,
        ),
    ]);
    assert!(
        diagnostics.contains(&(
            "ALN2403".to_string(),
            al_emit::VerificationSeverity::Warning
        )),
        "expected ALN2403 as a warning: {diagnostics:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|(_, severity)| *severity == al_emit::VerificationSeverity::Error),
        "{diagnostics:?}"
    );
    assert!(written, "{diagnostics:?}");
}

#[test]
fn curr_report_break_only_warns() {
    let (diagnostics, written) = build_summary(&[(
        "Break.Report.al",
        r#"report 50100 "Prop Break"
{
    dataset
    {
        dataitem(Integer; Integer)
        {
            trigger OnAfterGetRecord()
            begin
                StopLoop();
            end;
        }
    }

    procedure StopLoop()
    begin
        CurrReport.Break();
    end;
}
"#,
    )]);
    assert!(
        diagnostics.contains(&(
            "ALN2207".to_string(),
            al_emit::VerificationSeverity::Warning
        )),
        "expected ALN2207 as a warning: {diagnostics:?}"
    );
    assert!(
        !diagnostics.iter().any(|(code, severity)| code == "ALN2207"
            && *severity == al_emit::VerificationSeverity::Error),
        "{diagnostics:?}"
    );
    assert!(written, "{diagnostics:?}");
}

// ---------------------------------------------------------------------------
// Permissions, keys, event subscribers and manifest runtime.
// ---------------------------------------------------------------------------

#[test]
fn system_permission_is_accepted() {
    let codes = codes_for(&[(
        "System.PermissionSet.al",
        r#"permissionset 50100 "Prop System"
{
    Assignable = true;
    Permissions = system "Tools, Object Designer" = X;
}
"#,
    )]);
    assert_silent(&codes, "ALN2101");
    assert_silent(&codes, "ALN2103");
}

#[test]
fn key_on_system_id_is_accepted() {
    let codes = codes_for(&[(
        "Keyed.Table.al",
        r#"table 50100 "Prop Keyed"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
        key(BySystemId; SystemId)
        {
        }
    }
}
"#,
    )]);
    assert_silent(&codes, "ALN1107");
}

const EVENT_PUBLISHER: (&str, &str) = (
    "Publisher.Codeunit.al",
    r#"codeunit 50100 Publisher
{
    [IntegrationEvent(false, false)]
    procedure OnPosted(Value: Integer; Other: Text)
    begin
    end;
}
"#,
);

#[test]
fn subscriber_with_fewer_parameters_is_accepted() {
    let codes = codes_for(&[
        EVENT_PUBLISHER,
        (
            "Subscriber.Codeunit.al",
            r#"codeunit 50101 Subscriber
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnPosted', '', false, false)]
    procedure HandlePosted(Value: Integer)
    begin
    end;
}
"#,
        ),
    ]);
    assert_silent(&codes, "ALN2301");
}

#[test]
fn subscriber_with_unknown_parameter_is_rejected() {
    let errors = errors_for(&[
        EVENT_PUBLISHER,
        (
            "Subscriber.Codeunit.al",
            r#"codeunit 50101 Subscriber
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnPosted', '', false, false)]
    procedure HandlePosted(Sender: Codeunit Publisher; Missing: Text)
    begin
    end;
}
"#,
        ),
    ]);
    assert!(
        errors
            .iter()
            .any(|error| error.starts_with("ALN2301") && error.contains("'Missing'")),
        "{errors:?}"
    );
}

#[test]
fn wildcard_tabledata_in_permissionset_is_accepted() {
    assert_no_errors(&[(
        "Chk.PermissionSet.al",
        r#"permissionset 50100 "Chk Perm"
{
    Assignable = true;
    Permissions = tabledata * = R;
}
"#,
    )]);
}

#[test]
fn runtime_with_suffix_is_rejected() {
    let app_json = APP_JSON.replace("\"runtime\": \"13.0\"", "\"runtime\": \"17.typo\"");
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), &app_json, &[CUSTOMER_TABLE]);
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification must run");
    assert!(
        built.diagnostics.iter().any(|d| d.code == "ALN0103"
            && d.message == "app.json field `runtime` is not a valid version: 17.typo"),
        "{:?}",
        built.diagnostics
    );
}

#[test]
fn runtime_14_0_is_accepted() {
    let app_json = APP_JSON.replace("\"runtime\": \"13.0\"", "\"runtime\": \"14.0\"");
    let codes = codes_for_with_manifest(&app_json, &[CUSTOMER_TABLE]);
    assert_silent(&codes, "ALN0103");
}

/// A dependency app at `version`, with one table, built into `.app` bytes.
const DEP_APP_JSON: &str = r#"{
  "id": "bbbbbbbb-cccc-dddd-eeee-ffffffffffff",
  "name": "DepApp",
  "publisher": "Pub",
  "version": "VERSION",
  "brief": "",
  "description": "",
  "platform": "1.0.0.0",
  "application": "24.0.0.0",
  "runtime": "13.0",
  "idRanges": [ { "from": 50100, "to": 50199 } ],
  "dependencies": []
}
"#;

const DEP_TABLE: (&str, &str) = (
    "Item.Table.al",
    "table 50100 \"Dep Item\"\n{\n    DataClassification = CustomerContent;\n    fields\n    {\n        field(1; Code; Code[20]) { }\n    }\n}\n",
);

fn dependency_app_bytes(version: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        &DEP_APP_JSON.replace("VERSION", version),
        &[DEP_TABLE],
    );
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("dependency project must build");
    assert!(
        !built.has_errors(),
        "dependency {version} must build, got {:?}",
        built.diagnostics
    );
    built.app.expect("dependency app is written").bytes
}

/// Build a consumer that depends on `DepApp` at `required`, with `packages` in
/// `.alpackages`. Returns the ALN1007 messages.
fn consumer_aln1007(required: &str, packages: &[(&str, Vec<u8>)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let app_json = format!(
        r#"{{
  "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
  "name": "VerifyApp",
  "publisher": "Pub",
  "version": "1.0.0.0",
  "brief": "",
  "description": "",
  "platform": "1.0.0.0",
  "application": "24.0.0.0",
  "runtime": "13.0",
  "idRanges": [ {{ "from": 50100, "to": 50199 }} ],
  "dependencies": [ {{ "id": "bbbbbbbb-cccc-dddd-eeee-ffffffffffff", "name": "DepApp", "publisher": "Pub", "version": "{required}" }} ]
}}
"#
    );
    write_project(dir.path(), &app_json, &[]);
    let alpackages = dir.path().join(".alpackages");
    std::fs::create_dir_all(&alpackages).unwrap();
    for (file, bytes) in packages {
        std::fs::write(alpackages.join(file), bytes).unwrap();
    }
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("consumer project must run");
    built
        .diagnostics
        .iter()
        .filter(|d| d.code == "ALN1007")
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn dependency_below_required_version_is_rejected() {
    let dep_9 = dependency_app_bytes("9.0.0.0");
    let dep_10 = dependency_app_bytes("10.0.0.0");

    // The loaded package is below the required version: one ALN1007 with both versions.
    let below = consumer_aln1007("10.0.0.0", &[("Pub_DepApp_9.0.0.0.app", dep_9.clone())]);
    assert_eq!(
        below,
        vec![
            "Dependency 'DepApp' (bbbbbbbb-cccc-dddd-eeee-ffffffffffff) requires version 10.0.0.0 but the highest loaded package is 9.0.0.0"
                .to_string()
        ],
        "a loaded 9.0.0.0 must not satisfy a required 10.0.0.0"
    );

    // Equal versions satisfy the requirement (control).
    let equal = consumer_aln1007("9.0.0.0", &[("Pub_DepApp_9.0.0.0.app", dep_9.clone())]);
    assert!(
        equal.is_empty(),
        "an equal version must pass, got {equal:?}"
    );

    // Numeric comparison: 10.0.0.0 is above 9.0.0.0, although text order puts "10" first.
    let numeric = consumer_aln1007("9.0.0.0", &[("Pub_DepApp_10.0.0.0.app", dep_10.clone())]);
    assert!(
        numeric.is_empty(),
        "10.0.0.0 must satisfy a required 9.0.0.0, got {numeric:?}"
    );

    // The highest loaded version counts, not the first one found.
    let highest = consumer_aln1007(
        "10.0.0.0",
        &[
            ("Pub_DepApp_9.0.0.0.app", dep_9),
            ("Pub_DepApp_10.0.0.0.app", dep_10),
        ],
    );
    assert!(
        highest.is_empty(),
        "the highest loaded 10.0.0.0 must satisfy a required 10.0.0.0, got {highest:?}"
    );
}

/// A namespace qualified type names the object by its full name. The verifier
/// checks the object name, so a known codeunit gives no ALN2003.
#[test]
fn namespace_qualified_codeunit_variable_is_known() {
    let codes = codes_for(&[(
        "Finder.Codeunit.al",
        r#"namespace Purchases.Tools;

codeunit 50100 Finder
{
    procedure Run()
    var
        Own: Codeunit Purchases.Tools.Finder;
    begin
    end;
}
"#,
    )]);
    assert_silent(&codes, "ALN2003");
}

/// A Record subtype with a namespace is checked by its object name, as a
/// codeunit subtype is above.
#[test]
fn namespace_qualified_record_variable_is_known() {
    let codes = codes_for(&[(
        "Widget.al",
        r#"namespace Sales.Tools;

table 50100 Widget
{
    fields
    {
        field(1; Id; Code[20]) { }
    }
    keys
    {
        key(PK; Id) { Clustered = true; }
    }
}

codeunit 50101 User
{
    procedure Run()
    var
        W: Record Sales.Tools.Widget;
    begin
    end;
}
"#,
    )]);
    assert_silent(&codes, "ALN2401");
}

/// A quoted table name may hold a dot. The dot does not split the name.
#[test]
fn record_name_with_a_dot_is_known() {
    let codes = codes_for(&[(
        "Series.al",
        r#"table 50102 "No. Series"
{
    fields
    {
        field(1; Id; Code[20]) { }
    }
    keys
    {
        key(PK; Id) { Clustered = true; }
    }
}

codeunit 50103 User
{
    procedure Run()
    var
        N: Record "No. Series";
    begin
    end;
}
"#,
    )]);
    assert_silent(&codes, "ALN2401");
}

#[test]
fn unknown_qualified_record_is_still_reported() {
    let codes = codes_for(&[(
        "Missing.al",
        r#"namespace Sales.Tools;

codeunit 50100 User
{
    procedure Run()
    var
        M: Record Sales.Tools.Missing;
    begin
    end;
}
"#,
    )]);
    assert_reports(&codes, "ALN2401");
}

#[test]
fn aln2401_points_at_the_declaration() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        APP_JSON,
        &[(
            "Finder.al",
            "codeunit 50100 Finder\n{\n    procedure Run()\n    var\n        c: Record Missing;\n    begin\n    end;\n}\n",
        )],
    );
    let built = build_verified_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
        .expect("verification runs");
    let unknown = built
        .diagnostics
        .iter()
        .find(|d| d.code == "ALN2401")
        .unwrap_or_else(|| panic!("{:?}", built.diagnostics));
    assert_eq!(unknown.line, 5, "{:?}", built.diagnostics);
}
