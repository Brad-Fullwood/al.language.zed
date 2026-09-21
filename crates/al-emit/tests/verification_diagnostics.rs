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
fn aln2402_undeclared_identifier_on_the_left_of_an_assignment() {
    let codes = codes_for(&[(
        "C.Codeunit.al",
        "codeunit 50100 C\n{\n    procedure P()\n    begin\n        Nowhere := 1;\n    end;\n}\n",
    )]);
    assert_reports(&codes, "ALN2402");
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
fn aln2404_unknown_field_on_a_known_record() {
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
    assert_reports(&codes, "ALN2404");
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
    assert_silent(&clean, "ALN2404");
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
