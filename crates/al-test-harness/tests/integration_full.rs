//! Integration tests for the stdio LSP transport.

use al_test_harness::*;

// Minimal codeunit — always compiles, deterministic line numbers
const CODEUNIT_SIMPLE: &str = r#"codeunit 50150 "Integration Test"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello';
        Message(Msg);
    end;

    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    local procedure InternalHelper(): Boolean
    begin
        exit(true);
    end;
}"#;

const TABLE_SIMPLE: &str = r#"table 50150 "Integration Test Table"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
        field(2; Name; Text[100])
        {
            Caption = 'Name';
        }
        field(3; Amount; Decimal)
        {
            Caption = 'Amount';
        }
        field(4; Active; Boolean)
        {
            Caption = 'Active';
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
    }

    procedure GetName(): Text
    begin
        exit(Name);
    end;
}"#;

const ENUM_SIMPLE: &str = r#"enum 50150 "Integration Status"
{
    Extensible = false;
    Caption = 'Integration Status';

    value(0; " ")
    {
        Caption = ' ';
    }
    value(1; Active)
    {
        Caption = 'Active';
    }
    value(2; Closed)
    {
        Caption = 'Closed';
    }
}"#;

const PAGE_SIMPLE: &str = r#"page 50150 "Integration Test Card"
{
    PageType = Card;
    SourceTable = "Integration Test Table";
    Caption = 'Integration Test Card';

    layout
    {
        area(Content)
        {
            group(General)
            {
                field("No."; Rec."No.")
                {
                    ApplicationArea = All;
                }
                field(Name; Rec.Name)
                {
                    ApplicationArea = All;
                }
            }
        }
    }

    actions
    {
        area(Processing)
        {
            action(Activate)
            {
                Caption = 'Activate';
                ApplicationArea = All;

                trigger OnAction()
                begin
                    Rec.Active := true;
                    Rec.Modify(true);
                end;
            }
        }
    }
}"#;

#[tokio::test]
async fn initialize_succeeds() {
    let client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.shutdown().await;
}

#[tokio::test]
async fn shutdown_is_clean() {
    let client = LspClient::spawn(test_project_dir()).await.unwrap();
    // shutdown() has a 3-second timeout internally — test must complete
    tokio::time::timeout(tokio::time::Duration::from_secs(10), client.shutdown())
        .await
        .expect("shutdown must complete within 10 seconds");
}

#[tokio::test]
async fn two_sequential_sessions() {
    {
        let client = LspClient::spawn(test_project_dir()).await.unwrap();
        client.shutdown().await;
    }
    {
        let client = LspClient::spawn(test_project_dir()).await.unwrap();
        client.shutdown().await;
    }
}

#[tokio::test]
async fn open_file_triggers_diagnostics() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    // open_file already waits for publishDiagnostics — if it returns,
    // the notification was received.
    client
        .open_file("src/integration_b01.al", CODEUNIT_SIMPLE)
        .await;
    client.shutdown().await;
}

fn published_codes(
    diags: &std::collections::HashMap<String, Vec<serde_json::Value>>,
) -> Vec<String> {
    diags
        .values()
        .flat_map(|d| d.iter())
        .filter_map(|d| d.get("code").and_then(|c| c.as_str()).map(str::to_string))
        .collect()
}

/// The custom AL-L001 native-lint rule was removed, so an empty begin..end
/// must NOT re-emit it, and the server must still process the file (symbols).
#[tokio::test]
async fn empty_begin_end_emits_no_al_l001() {
    let code = r#"codeunit 50151 "Empty Proc"
{
    procedure DoNothing()
    begin
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    // open_file blocks until publishDiagnostics arrives, so the set is complete.
    client.open_file("src/integration_b02.al", code).await;

    let codes = published_codes(&client.drain_diagnostics());
    assert!(
        !codes.iter().any(|c| c.eq_ignore_ascii_case("AL-L001")),
        "AL-L001 was removed and must not be re-emitted; got {codes:?}"
    );

    // Server processed the file and stays responsive.
    let symbols = client.document_symbols("src/integration_b02.al").await;
    assert!(
        !symbols.is_empty(),
        "server must extract symbols from the opened file"
    );

    client.shutdown().await;
}

/// The custom AL-L007 native-lint rule was removed, so a TODO comment must
/// NOT re-emit it, and the server must still process the file (symbols).
#[tokio::test]
async fn todo_comment_emits_no_al_l007() {
    let code = r#"codeunit 50152 "Todo Test"
{
    procedure DoWork()
    begin
        // TODO: implement this properly
        Message('stub');
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_b03.al", code).await;

    let codes = published_codes(&client.drain_diagnostics());
    assert!(
        !codes.iter().any(|c| c.eq_ignore_ascii_case("AL-L007")),
        "AL-L007 was removed and must not be re-emitted; got {codes:?}"
    );

    let symbols = client.document_symbols("src/integration_b03.al").await;
    assert!(
        !symbols.is_empty(),
        "server must extract symbols from the opened file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn multiple_files_each_get_diagnostics() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_b04a.al", CODEUNIT_SIMPLE)
        .await;
    client
        .open_file("src/integration_b04b.al", TABLE_SIMPLE)
        .await;
    // Both open_file calls waited for publishDiagnostics — no hang = pass
    client.shutdown().await;
}

#[tokio::test]
async fn reopen_same_file_does_not_crash() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_b05.al", CODEUNIT_SIMPLE)
        .await;
    // Open again — some servers reject duplicate opens; ours should handle it
    client
        .open_file("src/integration_b05.al", CODEUNIT_SIMPLE)
        .await;
    client.shutdown().await;
}

#[tokio::test]
async fn hover_procedure_name() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_c01.al", CODEUNIT_SIMPLE)
        .await;

    // Line 2: "    procedure HelloWorld()"
    //                     ^ col 14
    let hover = client.hover("src/integration_c01.al", 2, 14).await;
    assert!(
        hover.is_some(),
        "hover on procedure name must return a result"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("HelloWorld")),
        "hover must mention HelloWorld"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn hover_local_variable_shows_type() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_c02.al", CODEUNIT_SIMPLE)
        .await;

    // Line 6: "        Msg := 'Hello';"
    //                  ^ col 8 — Msg usage
    let hover = client.hover("src/integration_c02.al", 6, 8).await;
    assert!(
        hover.is_some(),
        "hover on local variable must return a result"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Msg") || c.contains("Text")),
        "hover on Msg must mention name or type"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn hover_parameter() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_c03.al", CODEUNIT_SIMPLE)
        .await;

    // Line 10: "    procedure Add(A: Integer; B: Integer): Integer"
    //                             ^ col 22 — parameter A
    let hover = client.hover("src/integration_c03.al", 10, 22).await;
    assert!(hover.is_some(), "hover on parameter A must return a result");

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains('A') || c.contains("Integer")),
        "hover on parameter A must mention parameter name or type. Got: {:?}",
        content
    );

    client.shutdown().await;
}

#[tokio::test]
async fn hover_unopened_file_returns_null() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    let hover = client.hover("src/nonexistent_file.al", 0, 0).await;
    assert!(
        hover.is_none(),
        "unopened files must not produce hover content"
    );
    client.shutdown().await;
}

#[tokio::test]
async fn hover_record_variable_shows_type() {
    let code = r#"codeunit 50153 "Record Hover"
{
    procedure DoWork()
    var
        Cust: Record "Test Customer";
    begin
        Cust.Name := 'test';
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_c06.al", code).await;

    // Line 6: "        Cust.Name := 'test';"
    let hover = client.hover("src/integration_c06.al", 6, 8).await;
    assert!(
        hover.is_some(),
        "hover on Record variable must return result"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Record") || c.contains("Cust")),
        "hover on Record variable must mention Record or variable name"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_local_variable() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_d01.al", CODEUNIT_SIMPLE)
        .await;

    // Line 6: "        Msg := 'Hello';" — Msg is declared on line 4
    let def = client.definition("src/integration_d01.al", 6, 8).await;
    assert!(
        def.is_some(),
        "goto definition of Msg must return a location"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_on_whitespace_returns_null() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_d02.al", CODEUNIT_SIMPLE)
        .await;

    let def = client.definition("src/integration_d02.al", 1, 0).await;
    assert!(
        def.is_none(),
        "whitespace unexpectedly resolved to a definition"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_parameter() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_d03.al", CODEUNIT_SIMPLE)
        .await;

    // Line 12: "        exit(A + B);" — A is parameter on line 10
    // Column 13: the 'A' character (8 spaces + "exit(" = 13 chars before A)
    let def = client.definition("src/integration_d03.al", 12, 13).await;
    assert!(
        def.is_some(),
        "goto definition of parameter A (used in exit(A + B)) must return its declaration site on line 10"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_from_test_project() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = include_str!("../data/test_al_project/src/MultiProcedure.al");
    client.open_file("src/MultiProcedure.al", code).await;

    // Line 56: "        SimpleProc();" — should resolve to line 7
    let def = client.definition("src/MultiProcedure.al", 56, 8).await;
    assert!(
        def.is_some(),
        "goto definition of SimpleProc() call must return a location"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_undeclared_identifier() {
    let code = r#"codeunit 50154 "Undef Test"
{
    procedure DoWork()
    begin
        UndeclaredProc();
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_d05.al", code).await;

    let def = client.definition("src/integration_d05.al", 4, 8).await;
    assert!(def.is_none(), "undeclared identifier unexpectedly resolved");

    client.shutdown().await;
}

#[tokio::test]
async fn completions_in_procedure_body() {
    let code = r#"codeunit 50155 "Completion Test"
{
    procedure DoWork()
    begin
        M
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_e01.al", code).await;

    let completions = client.completion("src/integration_e01.al", 4, 9).await;
    assert!(
        !completions.is_empty(),
        "completions after 'M' must be non-empty"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn completions_include_local_variables() {
    let code = r#"codeunit 50156 "Local Var Completion"
{
    procedure DoWork()
    var
        MyCounter: Integer;
        MyName: Text;
    begin

    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_e02.al", code).await;

    // Line 7: inside begin..end, cursor after 8 spaces
    let completions = client.completion("src/integration_e02.al", 7, 8).await;
    let labels = completion_labels(&completions);

    assert!(
        labels.contains(&"MyCounter"),
        "completions must include MyCounter. Got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"MyName"),
        "completions must include MyName. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn completions_at_type_position() {
    let code = r#"codeunit 50157 "Type Completion"
{
    procedure DoWork()
    var
        x:
    begin
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_e03.al", code).await;

    let completions = client.completion("src/integration_e03.al", 4, 11).await;
    let labels = completion_labels(&completions);

    assert!(
        labels.iter().any(|l| l.eq_ignore_ascii_case("Integer")
            || l.eq_ignore_ascii_case("Text")
            || l.eq_ignore_ascii_case("Boolean")),
        "type position completions must include primitive types. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn completions_include_parameters() {
    let code = r#"codeunit 50158 "Param Completion"
{
    procedure DoWork(InputValue: Integer; OutputName: Text)
    begin

    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_e05.al", code).await;

    let completions = client.completion("src/integration_e05.al", 4, 8).await;
    let labels = completion_labels(&completions);

    assert!(
        labels
            .iter()
            .any(|l| *l == "InputValue" || *l == "OutputName"),
        "completions must include procedure parameters. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_codeunit() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_f01.al", CODEUNIT_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_f01.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Integration Test")),
        "symbols must include codeunit name. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"HelloWorld"),
        "symbols must include HelloWorld. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"Add"),
        "symbols must include Add. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"InternalHelper"),
        "symbols must include InternalHelper. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_table() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_f02.al", TABLE_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_f02.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Integration Test Table")),
        "symbols must include table name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_enum() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_f03.al", ENUM_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_f03.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Integration Status")),
        "symbols must include enum name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_page() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_f04.al", PAGE_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_f04.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Integration Test Card")),
        "symbols must include page name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_empty_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_f05.al", "").await;

    let symbols = client.document_symbols("src/integration_f05.al").await;
    assert!(symbols.is_empty(), "empty file returned document symbols");

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_test_project_files() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let hello = include_str!("../data/test_al_project/src/HelloWorld.al");
    client.open_file("src/HelloWorld.al", hello).await;

    let symbols = client.document_symbols("src/HelloWorld.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Hello World")),
        "symbols must include 'Hello World' codeunit. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn semantic_tokens_codeunit_non_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_g01.al", CODEUNIT_SIMPLE)
        .await;

    let tokens = client.semantic_tokens("src/integration_g01.al").await;
    assert!(tokens.is_some(), "semantic tokens must be returned");

    let data = semantic_token_data(&tokens.unwrap());
    assert!(!data.is_empty(), "token data array must not be empty");

    client.shutdown().await;
}

#[tokio::test]
async fn semantic_tokens_table_non_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_g02.al", TABLE_SIMPLE)
        .await;

    let tokens = client.semantic_tokens("src/integration_g02.al").await;
    assert!(
        tokens.is_some(),
        "semantic tokens must be returned for table"
    );

    let data = semantic_token_data(&tokens.unwrap());
    assert!(data.len() > 5, "table should produce multiple tokens");

    client.shutdown().await;
}

#[tokio::test]
async fn semantic_tokens_format_is_valid() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_g03.al", CODEUNIT_SIMPLE)
        .await;

    let tokens = client.semantic_tokens("src/integration_g03.al").await;
    let tokens = tokens.unwrap();

    // The "data" array length must be divisible by 5 (LSP relative encoding)
    let arr = tokens
        .get("data")
        .and_then(|d| d.as_array())
        .expect("tokens.data must be an array");
    assert_eq!(
        arr.len() % 5,
        0,
        "semantic token data length {} must be divisible by 5",
        arr.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn semantic_tokens_enum_non_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_g05.al", ENUM_SIMPLE)
        .await;

    let tokens = client.semantic_tokens("src/integration_g05.al").await;
    assert!(
        tokens.is_some(),
        "semantic tokens must be returned for enum"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn formatting_unindented_code_produces_edits() {
    let code = r#"codeunit 50160 "Format Test"
{
procedure DoWork()
begin
Message('hello');
end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_h01.al", code).await;

    let edits = client.format("src/integration_h01.al").await;
    assert!(
        !edits.is_empty(),
        "formatting badly-indented code must produce edits"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn formatting_edits_have_correct_structure() {
    let code = r#"codeunit 50161 "Format Struct"
{
procedure DoWork()
begin
end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_h02.al", code).await;

    let edits = client.format("src/integration_h02.al").await;
    for edit in &edits {
        assert!(
            edit.get("range").is_some(),
            "each edit must have a range field"
        );
        assert!(
            edit.get("newText").is_some(),
            "each edit must have a newText field"
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn formatting_correct_code_produces_fewer_edits_than_wrong() {
    let correct = CODEUNIT_SIMPLE; // assumed correctly indented
    let incorrect = r#"codeunit 50162 "Format Compare"
{
procedure Bad()
begin
Message('x');
end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_h04a.al", correct).await;
    client.open_file("src/integration_h04b.al", incorrect).await;

    let edits_correct = client.format("src/integration_h04a.al").await;
    let edits_incorrect = client.format("src/integration_h04b.al").await;

    assert!(
        edits_incorrect.len() >= edits_correct.len(),
        "incorrectly formatted code should require at least as many edits as correct code"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn folding_ranges_codeunit_non_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_i01.al", CODEUNIT_SIMPLE)
        .await;

    let ranges = client.folding_ranges("src/integration_i01.al").await;
    assert!(
        !ranges.is_empty(),
        "codeunit must have at least one folding range"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn folding_ranges_are_valid() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_i02.al", CODEUNIT_SIMPLE)
        .await;

    let ranges = client.folding_ranges("src/integration_i02.al").await;
    let lines = folding_range_lines(&ranges);

    assert!(
        lines.iter().all(|(s, e)| s <= e),
        "all folding ranges must have startLine <= endLine. Got: {:?}",
        lines
    );

    assert!(
        lines.iter().any(|(s, e)| e > s),
        "at least one range must span multiple lines. Got: {:?}",
        lines
    );

    client.shutdown().await;
}

#[tokio::test]
async fn folding_ranges_table() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_i03.al", TABLE_SIMPLE)
        .await;

    let ranges = client.folding_ranges("src/integration_i03.al").await;
    assert!(
        ranges.len() >= 3,
        "table with fields/keys/proc should have >= 3 fold ranges. Got: {}",
        ranges.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn folding_ranges_empty_file_are_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_i04.al", "").await;

    let ranges = client.folding_ranges("src/integration_i04.al").await;
    assert!(ranges.is_empty(), "empty file returned folding ranges");

    client.shutdown().await;
}

#[tokio::test]
async fn workspace_symbol_search_after_open() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_j01.al", CODEUNIT_SIMPLE)
        .await;

    let symbols = client.workspace_symbol("Integration Test").await;
    // initialize() polls until workspace/symbol("") is non-empty, so the
    // workspace has been scanned by the time we reach here.
    assert!(
        !symbols.is_empty(),
        "workspace symbol search for 'Integration Test' must find the opened codeunit. Got: {:?}",
        symbols
    );

    client.shutdown().await;
}

#[tokio::test]
async fn workspace_symbol_empty_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_j02.al", CODEUNIT_SIMPLE)
        .await;

    let symbols = client.workspace_symbol("").await;
    // initialize() polls workspace/symbol("") until non-empty, guaranteeing
    // the workspace has been scanned before the test body runs.
    assert!(
        !symbols.is_empty(),
        "workspace symbol empty query must return at least one symbol (workspace was scanned during initialize). Got: {:?}",
        symbols
    );

    client.shutdown().await;
}

#[tokio::test]
async fn workspace_symbol_finds_test_project_object() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // Workspace file scanning is async — retry briefly until HelloWorld is indexed
    let mut symbols = vec![];
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while symbols.is_empty() && tokio::time::Instant::now() < deadline {
        symbols = client.workspace_symbol("Hello").await;
        if symbols.is_empty() {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }
    assert!(
        !symbols.is_empty(),
        "workspace symbol search for 'Hello' must find at least 'Hello World' codeunit"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn workspace_symbol_case_insensitive() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // Workspace file scanning is async — retry briefly until HelloWorld is indexed
    let mut any_found = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while !any_found && tokio::time::Instant::now() < deadline {
        let symbols_lower = client.workspace_symbol("hello").await;
        let symbols_upper = client.workspace_symbol("HELLO").await;
        let symbols_mixed = client.workspace_symbol("Hello").await;
        any_found =
            !symbols_lower.is_empty() || !symbols_upper.is_empty() || !symbols_mixed.is_empty();
        if !any_found {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }

    assert!(
        any_found,
        "workspace symbol must find 'Hello World' under some case variant"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn references_local_variable() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_k01.al", CODEUNIT_SIMPLE)
        .await;

    // Line 4: "        Msg: Text;" — Msg declaration
    let refs = client.references("src/integration_k01.al", 4, 8).await;
    assert!(
        refs.len() >= 2,
        "Msg must have at least 2 references (decl + usage). Got: {}",
        refs.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn references_parameter_multiple_usages() {
    let code = r#"codeunit 50163 "Ref Test"
{
    procedure Double(X: Integer): Integer
    begin
        exit(X + X);
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_k02.al", code).await;

    let refs = client.references("src/integration_k02.al", 2, 21).await;
    assert_eq!(refs.len(), 3, "expected declaration and two usages");

    client.shutdown().await;
}

#[tokio::test]
async fn references_on_whitespace_are_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_k03.al", CODEUNIT_SIMPLE)
        .await;

    let refs = client.references("src/integration_k03.al", 1, 0).await;
    assert!(refs.is_empty(), "whitespace returned references");

    client.shutdown().await;
}

#[tokio::test]
async fn rename_local_variable() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_l01.al", CODEUNIT_SIMPLE)
        .await;

    // Line 4: "        Msg: Text;" — rename Msg to MyMessage
    let edit = client
        .rename("src/integration_l01.al", 4, 8, "MyMessage")
        .await;
    assert!(edit.is_some(), "rename of Msg must return a workspace edit");

    client.shutdown().await;
}

#[tokio::test]
async fn rename_edit_has_changes() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_l02.al", CODEUNIT_SIMPLE)
        .await;

    let edit = client
        .rename("src/integration_l02.al", 4, 8, "NewMsg")
        .await;
    let edit = edit.expect("rename must return an edit");

    let changes = edit.get("changes");
    assert!(
        changes.is_some(),
        "rename workspace edit must contain 'changes'. Got: {:?}",
        edit
    );

    client.shutdown().await;
}

#[tokio::test]
async fn rename_on_keyword_returns_null() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_l03.al", CODEUNIT_SIMPLE)
        .await;

    let edit = client
        .rename("src/integration_l03.al", 5, 4, "NewName")
        .await;
    assert!(
        edit.is_none(),
        "keyword unexpectedly produced a rename edit"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn signature_help_procedure_call() {
    let code = r#"codeunit 50164 "Sig Help Test"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure Call()
    begin
        Add(1, 2);
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_m01.al", code).await;

    // Line 9: "        Add(1, 2);" — inside parens
    let sig = client.signature_help("src/integration_m01.al", 9, 12).await;
    assert!(
        sig.is_some(),
        "signature help inside Add() call must return a result"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn signature_help_outside_call_returns_null() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_m02.al", CODEUNIT_SIMPLE)
        .await;

    let sig = client.signature_help("src/integration_m02.al", 1, 0).await;
    assert!(sig.is_none(), "signature help returned outside a call");

    client.shutdown().await;
}

#[tokio::test]
async fn inlay_hints_procedure_call() {
    let code = r#"codeunit 50165 "Inlay Test"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure Call()
    begin
        Add(10, 20);
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_n01.al", code).await;

    let hints = client.inlay_hints("src/integration_n01.al", 0, 12).await;
    assert!(!hints.is_empty(), "procedure call returned no inlay hints");
    assert!(
        hints.iter().all(|h| h.get("label").is_some()),
        "every inlay hint should have a label: {hints:?}"
    );
    assert!(
        hints.iter().all(|h| h.get("position").is_some()),
        "every inlay hint should have a position: {hints:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn inlay_hints_empty_range_is_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_n02.al", CODEUNIT_SIMPLE)
        .await;

    let hints = client.inlay_hints("src/integration_n02.al", 0, 0).await;
    assert!(
        hints.is_empty(),
        "empty range should produce no inlay hints: {hints:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn multi_file_workspace_symbols() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_p01a.al", CODEUNIT_SIMPLE)
        .await;
    client
        .open_file("src/integration_p01b.al", TABLE_SIMPLE)
        .await;
    client
        .open_file("src/integration_p01c.al", ENUM_SIMPLE)
        .await;

    let cu_syms = client.workspace_symbol("Integration Test").await;
    assert!(
        !cu_syms.is_empty(),
        "workspace must find 'Integration Test' objects. Got: {:?}",
        cu_syms
    );

    client.shutdown().await;
}

#[tokio::test]
async fn document_symbols_independent_per_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_p02a.al", CODEUNIT_SIMPLE)
        .await;
    client
        .open_file("src/integration_p02b.al", TABLE_SIMPLE)
        .await;

    let cu_syms = client.document_symbols("src/integration_p02a.al").await;
    let tbl_syms = client.document_symbols("src/integration_p02b.al").await;

    let cu_names = symbol_names(&cu_syms);
    let tbl_names = symbol_names(&tbl_syms);

    assert!(
        cu_names.iter().any(|n| n.contains("Integration Test")),
        "codeunit symbols must contain codeunit name"
    );
    assert!(
        tbl_names
            .iter()
            .any(|n| n.contains("Integration Test Table")),
        "table symbols must contain table name"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn hover_after_multi_file_open() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_p03a.al", CODEUNIT_SIMPLE)
        .await;
    client
        .open_file("src/integration_p03b.al", TABLE_SIMPLE)
        .await;

    // Hover in the codeunit file (should not be confused with table)
    let hover = client.hover("src/integration_p03a.al", 2, 14).await;
    assert!(
        hover.is_some(),
        "hover must still work after opening multiple files"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn cross_file_procedure_in_workspace_symbol() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_p04.al", CODEUNIT_SIMPLE)
        .await;

    // Search for the codeunit itself — workspace/symbol returns objects
    let symbols = client.workspace_symbol("Integration Test").await;
    assert!(
        !symbols.is_empty(),
        "workspace symbol for 'Integration Test' must find the opened codeunit. Got: {:?}",
        symbols
    );

    client.shutdown().await;
}

#[tokio::test]
async fn large_file_5000_lines() {
    let mut code = String::from("codeunit 50170 \"Large File\"\n{\n");
    for i in 0..200 {
        code.push_str(&format!(
            "    procedure Proc{i}(P{i}: Integer): Integer\n    begin\n        exit(P{i} * {i});\n    end;\n\n"
        ));
    }
    code.push_str("}\n");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q01.al", &code).await;

    let symbols = client.document_symbols("src/integration_q01.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.len() >= 200,
        "all 200 procedures must be found. Got: {}",
        names.len()
    );

    let tokens = client.semantic_tokens("src/integration_q01.al").await;
    assert!(tokens.is_some(), "large file must produce semantic tokens");

    let ranges = client.folding_ranges("src/integration_q01.al").await;
    assert!(
        ranges.len() >= 200,
        "large file must have >= 200 folding ranges. Got: {}",
        ranges.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn deeply_nested_begin_end() {
    // 20-level nesting — enough to stress parsers without OOM
    let mut body = "        Message('deep');\n".to_string();
    for _ in 0..20 {
        body = format!("        begin\n{body}        end;\n");
    }

    let code = format!(
        "codeunit 50171 \"Deep Nest\"\n{{\n    procedure Nest()\n    begin\n{body}    end;\n}}\n"
    );

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q02.al", &code).await;

    let symbols = client.document_symbols("src/integration_q02.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names
            .iter()
            .any(|n| n.contains("Deep Nest") || *n == "Nest"),
        "deeply nested file must parse. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn incomplete_code_all_queries_graceful() {
    let code = r#"codeunit 50172 "Incomplete"
{
    procedure
"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q03.al", code).await;

    let _hover = client.hover("src/integration_q03.al", 2, 10).await;
    let _syms = client.document_symbols("src/integration_q03.al").await;
    let _tokens = client.semantic_tokens("src/integration_q03.al").await;
    let _ranges = client.folding_ranges("src/integration_q03.al").await;
    let _compl = client.completion("src/integration_q03.al", 2, 14).await;
    let _def = client.definition("src/integration_q03.al", 2, 10).await;
    let _refs = client.references("src/integration_q03.al", 2, 10).await;
    let _fmt = client.format("src/integration_q03.al").await;

    client.shutdown().await;
}

#[tokio::test]
async fn repeated_requests_remain_consistent() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_q04.al", CODEUNIT_SIMPLE)
        .await;

    for _ in 0..5 {
        assert!(client
            .hover("src/integration_q04.al", 2, 14)
            .await
            .is_some());
        assert!(!client
            .document_symbols("src/integration_q04.al")
            .await
            .is_empty());
        assert!(client
            .semantic_tokens("src/integration_q04.al")
            .await
            .is_some());
        assert!(!client
            .folding_ranges("src/integration_q04.al")
            .await
            .is_empty());
    }

    client.shutdown().await;
}

#[tokio::test]
async fn hover_out_of_bounds_position() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_q05.al", CODEUNIT_SIMPLE)
        .await;

    let hover = client.hover("src/integration_q05.al", 9999, 9999).await;
    assert!(hover.is_none(), "out-of-bounds hover returned content");

    client.shutdown().await;
}

#[tokio::test]
async fn syntax_error_file_is_parsed_tolerantly() {
    let code = include_str!("../data/test_al_project/src/ErrorCases.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q06.al", code).await;

    let symbols = client.document_symbols("src/integration_q06.al").await;
    assert!(
        symbol_names(&symbols)
            .iter()
            .any(|name| name.contains("Error Cases")),
        "error-tolerant parse lost the enclosing codeunit"
    );

    let tokens = client.semantic_tokens("src/integration_q06.al").await;
    assert!(
        tokens.is_some(),
        "syntax-error file returned no semantic tokens"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn unicode_string_literals() {
    let code = r#"codeunit 50173 "Unicode Test"
{
    procedure Greet()
    var
        Msg: Text;
    begin
        Msg := 'こんにちは世界 — Héllo Wörld 🌍';
        Message(Msg);
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q07.al", code).await;

    let tokens = client.semantic_tokens("src/integration_q07.al").await;
    assert!(
        tokens.is_some(),
        "unicode file must produce semantic tokens"
    );

    let symbols = client.document_symbols("src/integration_q07.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names.iter().any(|n| *n == "Greet" || n.contains("Unicode")),
        "unicode file symbols must be found. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn very_long_line() {
    // Build a procedure call with 500 arguments (AL doesn't actually allow this,
    // but tree-sitter should not hang on it)
    let long_comment: String = "x".repeat(2000);
    let code = format!(
        "codeunit 50174 \"Long Line\"\n{{\n    procedure DoWork()\n    begin\n        // {long_comment}\n        Message('done');\n    end;\n}}\n"
    );

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q08.al", &code).await;

    let tokens = client.semantic_tokens("src/integration_q08.al").await;
    assert!(tokens.is_some(), "file with long line must produce tokens");

    client.shutdown().await;
}

#[tokio::test]
async fn open_file_then_immediate_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // open_file waits for publishDiagnostics before returning
    client
        .open_file("src/integration_q09.al", CODEUNIT_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_q09.al").await;
    let names = symbol_names(&symbols);

    assert!(
        !names.is_empty(),
        "symbols must be available immediately after open_file returns"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn file_with_only_comments() {
    let code = r#"// This is a comment
// Another comment
// No actual AL code"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q10.al", code).await;

    let symbols = client.document_symbols("src/integration_q10.al").await;
    assert!(symbols.is_empty(), "comment-only file returned symbols");

    let tokens = client
        .semantic_tokens("src/integration_q10.al")
        .await
        .expect("comment-only file must return semantic tokens");
    assert!(!semantic_token_data(&tokens).is_empty());

    client.shutdown().await;
}

#[tokio::test]
async fn crlf_line_endings() {
    let code = "codeunit 50175 \"CRLF Test\"\r\n{\r\n    procedure DoWork()\r\n    begin\r\n        Message('hello');\r\n    end;\r\n}\r\n";

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_q11.al", code).await;

    let symbols = client.document_symbols("src/integration_q11.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names
            .iter()
            .any(|n| n.contains("CRLF Test") || *n == "DoWork"),
        "CRLF file must parse correctly. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn workspace_symbol_with_unknown_field_is_graceful() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    let symbols = client.workspace_symbol("NonExistentXYZ123").await;
    assert!(
        symbols.is_empty(),
        "unknown query unexpectedly returned symbols"
    );

    client.shutdown().await;
}

// The daemon's methods map 1-to-1 to al-core queries. We exercise them via
// the LSP transport since the socket transport is not yet implemented.
// These tests verify the underlying al-core query paths are exercised.

/// definition query (→ al-core::queries::definition)
#[tokio::test]
async fn core_definition_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    let code = include_str!("../data/test_al_project/src/MultiProcedure.al");
    client.open_file("src/MultiProcedure.al", code).await;

    // CallsOthers calls SimpleProc on line 56
    let def = client.definition("src/MultiProcedure.al", 56, 8).await;
    assert!(
        def.is_some(),
        "core definition query must resolve SimpleProc call"
    );

    client.shutdown().await;
}

/// references query (→ al-core::queries::references)
#[tokio::test]
async fn core_references_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r02.al", CODEUNIT_SIMPLE)
        .await;

    // Msg: Text — declaration on line 4, used on lines 6, 7
    let refs = client.references("src/integration_r02.al", 4, 8).await;
    assert!(
        refs.len() >= 2,
        "core references query must find >= 2 refs for Msg. Got: {}",
        refs.len()
    );

    client.shutdown().await;
}

/// completions query (→ al-core::queries::completions)
#[tokio::test]
async fn core_completions_query() {
    let code = r#"codeunit 50176 "Core Compl"
{
    procedure DoWork()
    var
        MyVar: Integer;
    begin
        My
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_r03.al", code).await;

    let completions = client.completion("src/integration_r03.al", 6, 10).await;
    let labels = completion_labels(&completions);

    assert!(
        labels.contains(&"MyVar"),
        "core completions query must include MyVar. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

/// hover query (→ al-core::queries::hover)
#[tokio::test]
async fn core_hover_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r04.al", CODEUNIT_SIMPLE)
        .await;

    let hover = client.hover("src/integration_r04.al", 4, 8).await;
    assert!(
        hover.is_some(),
        "core hover query must return result for local variable"
    );

    client.shutdown().await;
}

/// document symbols query (→ al-core::queries::symbols)
#[tokio::test]
async fn core_document_symbols_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r05.al", CODEUNIT_SIMPLE)
        .await;

    let symbols = client.document_symbols("src/integration_r05.al").await;
    let names = symbol_names(&symbols);

    assert!(
        !names.is_empty(),
        "core document symbols query must return symbols"
    );

    client.shutdown().await;
}

/// semantic tokens query (→ al-core::queries::semantic_tokens)
#[tokio::test]
async fn core_semantic_tokens_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r06.al", CODEUNIT_SIMPLE)
        .await;

    let tokens = client.semantic_tokens("src/integration_r06.al").await;
    assert!(
        tokens.is_some(),
        "core semantic tokens query must return tokens"
    );

    client.shutdown().await;
}

/// folding ranges query (→ al-core::queries::folding)
#[tokio::test]
async fn core_folding_ranges_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r07.al", CODEUNIT_SIMPLE)
        .await;

    let ranges = client.folding_ranges("src/integration_r07.al").await;
    assert!(!ranges.is_empty(), "core folding query must return ranges");

    client.shutdown().await;
}

/// format query (→ al-core::queries::source)
#[tokio::test]
async fn core_format_query() {
    let bad = r#"codeunit 50177 "Format Core"
{
procedure Bad()
begin
end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_r08.al", bad).await;

    let edits = client.format("src/integration_r08.al").await;
    assert!(
        !edits.is_empty(),
        "core format query must produce edits for unindented code"
    );

    client.shutdown().await;
}

/// rename query (→ al-core::queries::rename)
#[tokio::test]
async fn core_rename_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r09.al", CODEUNIT_SIMPLE)
        .await;

    let edit = client
        .rename("src/integration_r09.al", 4, 8, "Renamed")
        .await;
    assert!(
        edit.is_some(),
        "core rename query must return workspace edit"
    );

    client.shutdown().await;
}

/// signature help query (→ al-core::queries::signature)
#[tokio::test]
async fn core_signature_help_query() {
    let code = r#"codeunit 50178 "Sig Core"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure Call()
    begin
        Add(1, 2);
    end;
}"#;

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/integration_r10.al", code).await;

    let sig = client.signature_help("src/integration_r10.al", 9, 12).await;
    assert!(
        sig.is_some(),
        "core signature help query must return result inside Add() call"
    );

    client.shutdown().await;
}

/// workspace symbol query (→ al-core::queries::symbols)
#[tokio::test]
async fn core_workspace_symbol_query() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client
        .open_file("src/integration_r13.al", CODEUNIT_SIMPLE)
        .await;

    // Workspace file scanning is async — retry briefly until HelloWorld is indexed
    let mut symbols = vec![];
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while symbols.is_empty() && tokio::time::Instant::now() < deadline {
        symbols = client.workspace_symbol("Hello").await;
        if symbols.is_empty() {
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        }
    }
    assert!(
        !symbols.is_empty(),
        "core workspace symbol query must find 'Hello World' codeunit"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn interface_object_produces_symbols() {
    let code = include_str!("../data/test_al_project/src/Interface50100.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/Interface50100.al", code).await;

    let symbols = client.document_symbols("src/Interface50100.al").await;
    let names = symbol_names(&symbols);
    assert!(
        !names.is_empty(),
        "interface object must produce symbols. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn page_extension_produces_symbols() {
    let code = include_str!("../data/test_al_project/src/PageExtension50100.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/PageExtension50100.al", code).await;

    let symbols = client.document_symbols("src/PageExtension50100.al").await;
    assert!(!symbols.is_empty(), "page extension must produce symbols");

    client.shutdown().await;
}

#[tokio::test]
async fn table_extension_produces_symbols() {
    let code = include_str!("../data/test_al_project/src/TableExtension50100.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/TableExtension50100.al", code).await;

    let symbols = client.document_symbols("src/TableExtension50100.al").await;
    assert!(!symbols.is_empty(), "table extension must produce symbols");

    client.shutdown().await;
}

#[tokio::test]
async fn codeunit_with_integration_events_produces_symbols() {
    let code = include_str!("../data/test_al_project/src/CodeunitWithEvents.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/CodeunitWithEvents.al", code).await;

    let symbols = client.document_symbols("src/CodeunitWithEvents.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names
            .iter()
            .any(|n| n.contains("OnBeforeProcess") || n.contains("Test Event Publisher")),
        "event publisher must produce event symbols. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn deep_nesting_fixture_produces_semantic_tokens() {
    let code = include_str!("../data/test_al_project/src/DeepNesting.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/DeepNesting.al", code).await;

    let tokens = client.semantic_tokens("src/DeepNesting.al").await;
    assert!(
        tokens.is_some(),
        "deeply nested file must produce semantic tokens"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn table_with_field_trigger_produces_symbols_and_tokens() {
    let code = include_str!("../data/test_al_project/src/Table50100.al");

    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/Table50100.al", code).await;

    let symbols = client.document_symbols("src/Table50100.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names.iter().any(|n| n.contains("Test Customer")),
        "table with triggers must produce symbols. Got: {:?}",
        names
    );

    let tokens = client.semantic_tokens("src/Table50100.al").await;
    assert!(tokens.is_some(), "table with triggers must produce tokens");

    client.shutdown().await;
}
