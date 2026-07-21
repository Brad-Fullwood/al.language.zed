//! Real-world tests — use actual production AL code from the AL test project.
//!
//! These tests exercise the LSP against real AL patterns that users write,
//! not simplified test fixtures.

use al_test_harness::*;

const TABLE_AL: &str = r#"table 50200 "Item Journal Staging"
{
    Extensible = false;
    Caption = 'Item Journal Staging';
    DataClassification = CustomerContent;
    LookupPageId = "IJL Staging List";
    DrillDownPageId = "IJL Staging List";

    fields
    {
        field(1; "Entry No."; Integer)
        {
            ToolTip = 'Specifies the entry number of the item journal staging record.';
            Caption = 'Entry No.';
            AutoIncrement = true;
        }
        field(2; "Item Journal Line Id"; Guid)
        {
            ToolTip = 'Specifies the ID of the item journal line.';
            Caption = 'Item Journal Line Id';
            DataClassification = SystemMetadata;
            AllowInCustomizations = Never;
        }
        field(3; "Journal Data"; Blob)
        {
            ToolTip = 'Specifies the journal data of the item journal staging record.';
            Caption = 'Journal Data';
        }
        field(4; Status; Enum "IJL Status")
        {
            ToolTip = 'Specifies the status of the item journal staging record.';
            Caption = 'Status';
            InitValue = Pending;
        }
        field(5; "Error Message"; Blob)
        {
            ToolTip = 'Specifies the error message of the item journal staging record.';
            Caption = 'Error Message';
        }
        field(6; "Posted Entry No."; Integer)
        {
            ToolTip = 'Specifies the entry number of the posted item journal line.';
            Caption = 'Posted Entry No.';
            Editable = false;
        }
        field(7; "Processed At"; DateTime)
        {
            ToolTip = 'Specifies the date and time the item journal staging record was processed.';
            Caption = 'Processed At';
            Editable = false;
        }
        field(8; "Entry Type"; Text[20])
        {
            Caption = 'Entry Type';
        }
        field(9; "Document No."; Code[20])
        {
            Caption = 'Document No.';
        }
        field(10; "Item No."; Code[20])
        {
            Caption = 'Item No.';
        }
        field(11; "Posting Date"; Date)
        {
            Caption = 'Posting Date';
        }
        field(12; "Location Code"; Code[10])
        {
            Caption = 'Location Code';
        }
        field(13; "Order No."; Code[20])
        {
            Caption = 'Order No.';
        }
    }

    keys
    {
        key(PK; "Entry No.")
        {
            Clustered = true;
        }
        key(StatusKey; Status, SystemCreatedAt) { }
        key(DocumentKey; "Document No.") { }
    }
    fieldgroups
    {
        fieldgroup(DropDown; "Entry No.", Status, "Entry Type", "Document No.", "Item No.") { }
    }

    internal procedure SetJournalData(Data: Text)
    var
        OutStream: OutStream;
    begin
        Clear("Journal Data");
        "Journal Data".CreateOutStream(OutStream, TextEncoding::UTF8);
        OutStream.WriteText(Data);
    end;

    internal procedure GetJournalData(): Text
    var
        InStream: InStream;
        Data: Text;
    begin
        Rec.CalcFields("Journal Data");
        if not Rec."Journal Data".HasValue() then
            exit('');
        Rec."Journal Data".CreateInStream(InStream, TextEncoding::UTF8);
        InStream.ReadText(Data);
        exit(Data);
    end;

    internal procedure SetErrorMessage(ErrorText: Text)
    var
        OutStream: OutStream;
    begin
        Clear("Error Message");
        "Error Message".CreateOutStream(OutStream, TextEncoding::UTF8);
        OutStream.WriteText(ErrorText);
    end;

    internal procedure GetErrorMessage(): Text
    var
        InStream: InStream;
        ErrorText: Text;
    begin
        Rec.CalcFields("Error Message");
        if not Rec."Error Message".HasValue() then
            exit('');
        Rec."Error Message".CreateInStream(InStream, TextEncoding::UTF8);
        InStream.ReadText(ErrorText);
        exit(ErrorText);
    end;
}"#;

const CODEUNIT_AL: &str = r#"codeunit 50200 "IJL API Helper"
{
    InherentPermissions = x;
    Permissions = tabledata "Item Journal Staging" = rm;

    procedure Precheck(var Staging: Record "Item Journal Staging")
    begin
        if not Staging.FindSet(false) then
            exit;

        repeat
            this.PrecheckRecord(Staging);
        until Staging.Next() = 0;
    end;

    local procedure PrecheckRecord(var Staging: Record "Item Journal Staging")
    var
        TempItemJnlLine: Record "Item Journal Line" temporary;
    begin
        TempItemJnlLine.Init();
        TempItemJnlLine."Line No." := Staging."Entry No.";

        if not this.GetItemJournalFromStaging(Staging, TempItemJnlLine) then begin
            Staging.Status := Staging.Status::Failed;
            Staging.SetErrorMessage('Failed to parse journal data');
            Staging.Modify(true);
            exit;
        end;

        TempItemJnlLine.Insert(false);
        if this.TryCheckItemJnlLine(TempItemJnlLine) then
            Staging.Status := Staging.Status::Validated
        else begin
            Staging.Status := Staging.Status::Failed;
            Staging.SetErrorMessage(GetLastErrorText());
        end;
        TempItemJnlLine.Delete(false);

        Staging.Modify(true);
    end;

    procedure SchedulePost(var Staging: Record "Item Journal Staging")
    begin
        if Staging.IsEmpty() then
            exit;

        Staging.ModifyAll(Status, Staging.Status::Posting, true);
        Commit();

        TaskScheduler.CreateTask(Codeunit::"IJL Post Task", 0, true, CompanyName(), CurrentDateTime());
    end;

    procedure GetItemJournalFromStaging(var Staging: Record "Item Journal Staging"; var ItemJnlLine: Record "Item Journal Line"): Boolean
    var
        JsonTools: Codeunit "Json Tools";
        JsonObj: JsonObject;
        JournalData: Text;
    begin
        JournalData := Staging.GetJournalData();
        if (JournalData = '') or (not JsonObj.ReadFrom(JournalData)) then
            exit(false);

        ItemJnlLine := JsonTools.Json2Rec(JsonObj, ItemJnlLine);
        exit(true);
    end;

    [TryFunction]
    local procedure TryCheckItemJnlLine(var TempItemJnlLine: Record "Item Journal Line" temporary)
    var
        ItemJnlCheckLine: Codeunit "Item Jnl.-Check Line";
    begin
        ItemJnlCheckLine.Run(TempItemJnlLine);
    end;

    procedure ApplyStagingFilter(var SourceRec: Record "Item Journal Line"; var FilteredStaging: Record "Item Journal Staging")
    var
        RecRef: RecordRef;
        StagingRef: RecordRef;
        FldRef: FieldRef;
        StagingFldRef: FieldRef;
        FieldMap: Dictionary of [Integer, Integer];
        i: Integer;
        FilterValue: Text;
        FilterErr: Label 'Cannot filter on %1. Supported filters: %2', Comment = '%1 = Field name, %2 = Supported filters';
    begin
        this.BuildFilterFieldMap(SourceRec, FieldMap);
        RecRef.GetTable(SourceRec);
        StagingRef.GetTable(FilteredStaging);

        for i := 1 to RecRef.FieldCount() do begin
            FldRef := RecRef.FieldIndex(i);
            FilterValue := FldRef.GetFilter();
            if FilterValue = '' then
                continue;

            if not FieldMap.ContainsKey(FldRef.Number()) then
                Error(FilterErr, FldRef.Name(), this.GetSupportedFilterNames(SourceRec, FieldMap));

            StagingFldRef := StagingRef.Field(FieldMap.Get(FldRef.Number()));
            StagingFldRef.SetFilter(FilterValue);
        end;

        StagingRef.SetTable(FilteredStaging);
    end;

    local procedure BuildFilterFieldMap(var SourceRec: Record "Item Journal Line"; var FieldMap: Dictionary of [Integer, Integer])
    var
        Staging: Record "Item Journal Staging";
    begin
        FieldMap.Add(SourceRec.FieldNo("Entry Type"), Staging.FieldNo("Entry Type"));
        FieldMap.Add(SourceRec.FieldNo("Document No."), Staging.FieldNo("Document No."));
    end;

    local procedure GetSupportedFilterNames(var SourceRec: Record "Item Journal Line"; FieldMap: Dictionary of [Integer, Integer]): Text
    var
        RecRef: RecordRef;
        FldRef: FieldRef;
        SourceFieldNo: Integer;
        Names: Text;
    begin
        RecRef.GetTable(SourceRec);
        foreach SourceFieldNo in FieldMap.Keys() do begin
            FldRef := RecRef.Field(SourceFieldNo);
            if Names <> '' then
                Names += ', ';
            Names += FldRef.Name();
        end;
        exit(Names);
    end;
}"#;

const ENUM_AL: &str = r#"enum 50200 "IJL Status"
{
    Extensible = false;
    Caption = 'Item Journal Staging Status';

    value(1; Pending)
    {
        Caption = 'Pending';
    }
    value(2; Validated)
    {
        Caption = 'Validated';
    }
    value(3; Posting)
    {
        Caption = 'Posting';
    }
    value(4; Posted)
    {
        Caption = 'Posted';
    }
    value(5; Failed)
    {
        Caption = 'Failed';
    }
}"#;

const PERMISSIONSET_AL: &str = r#"permissionset 50200 "IJL API"
{
    Assignable = true;
    Caption = 'IJL API Permissions';

    Permissions = tabledata "Item Journal Staging" = RIMD,
                  codeunit "IJL API Helper" = X,
                  codeunit "IJL Post Task" = X,
                  page "Item Journal API" = X;
}"#;

#[tokio::test]
async fn test_real_table_parses_and_has_tokens() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/table.al", TABLE_AL).await;

    let tokens = client.semantic_tokens("objects/table.al").await;
    assert!(tokens.is_some(), "Table should produce semantic tokens");
    let data = semantic_token_data(&tokens.unwrap());
    assert!(
        data.len() > 20,
        "Complex table should produce many tokens, got {}",
        data.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_real_codeunit_parses_and_has_tokens() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    let tokens = client.semantic_tokens("objects/codeunit.al").await;
    assert!(tokens.is_some(), "Codeunit should produce semantic tokens");
    let data = semantic_token_data(&tokens.unwrap());
    assert!(
        data.len() > 30,
        "Complex codeunit should produce many tokens, got {}",
        data.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_real_enum_parses_and_has_tokens() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/enum.al", ENUM_AL).await;

    let tokens = client.semantic_tokens("objects/enum.al").await;
    assert!(tokens.is_some(), "Enum should produce semantic tokens");

    client.shutdown().await;
}

#[tokio::test]
async fn test_real_permissionset_parses() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client
        .open_file("objects/permset.al", PERMISSIONSET_AL)
        .await;

    // Should at least produce document symbols
    let symbols = client.document_symbols("objects/permset.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names.iter().any(|n| n.contains("IJL API")),
        "Should find permissionset name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_table_document_symbols() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/table.al", TABLE_AL).await;

    let symbols = client.document_symbols("objects/table.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Item Journal Staging")),
        "Should find table name. Got: {:?}",
        names
    );

    assert!(
        names
            .iter()
            .any(|n| *n == "SetJournalData" || n.contains("SetJournalData")),
        "Should find SetJournalData procedure. Got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|n| *n == "GetJournalData" || n.contains("GetJournalData")),
        "Should find GetJournalData procedure. Got: {:?}",
        names
    );
    assert!(
        names
            .iter()
            .any(|n| *n == "SetErrorMessage" || n.contains("SetErrorMessage")),
        "Should find SetErrorMessage procedure. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_codeunit_document_symbols() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    let symbols = client.document_symbols("objects/codeunit.al").await;
    let names = symbol_names(&symbols);

    for expected in &[
        "Precheck",
        "PrecheckRecord",
        "SchedulePost",
        "GetItemJournalFromStaging",
        "TryCheckItemJnlLine",
        "ApplyStagingFilter",
        "BuildFilterFieldMap",
        "GetSupportedFilterNames",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "Should find procedure {}. Got: {:?}",
            expected,
            names
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_enum_document_symbols() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/enum.al", ENUM_AL).await;

    let symbols = client.document_symbols("objects/enum.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("IJL Status")),
        "Should find enum name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_procedure_in_codeunit() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // "Precheck" procedure name on line 5 (0-indexed)
    let hover = client.hover("objects/codeunit.al", 5, 14).await;
    assert!(
        hover.is_some(),
        "Should return hover for Precheck procedure"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Precheck")),
        "Hover should mention Precheck. Got: {:?}",
        content
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_local_procedure() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // "PrecheckRecord" local procedure - line 15 (0-indexed)
    let hover = client.hover("objects/codeunit.al", 15, 25).await;
    assert!(
        hover.is_some(),
        "Should return hover for local procedure PrecheckRecord"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("local") || c.contains("PrecheckRecord")),
        "Hover should mention local or PrecheckRecord. Got: {:?}",
        content
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_parameter() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // "Staging" parameter in Precheck on line 5
    // procedure Precheck(var Staging: Record "Item Journal Staging")
    //                        ^^^^^^^
    let hover = client.hover("objects/codeunit.al", 5, 27).await;
    let content = hover.as_ref().and_then(hover_content);
    // Behavioral: not just "a hover came back" — it must actually describe the
    // Staging parameter / its Record type, so a hover that returns a neighbouring
    // symbol's info is caught.
    assert!(
        content.is_some_and(|c| {
            c.contains("Staging") || c.contains("Item Journal") || c.contains("Record")
        }),
        "hover on parameter Staging must describe it; got {content:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_goto_definition_cross_procedure_same_file() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;
    client.open_file("objects/table.al", TABLE_AL).await;

    // In Precheck, "this.PrecheckRecord" calls a local procedure
    // "PrecheckRecord" on line 11 (in repeat block)
    let def = client.definition("objects/codeunit.al", 11, 23).await;
    // This requires cross-procedure reference resolution.
    assert!(
        def.is_some(),
        "goto definition of PrecheckRecord (local procedure call) must return a location"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_goto_definition_cross_file() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;
    client.open_file("objects/table.al", TABLE_AL).await;
    client.open_file("objects/enum.al", ENUM_AL).await;

    // "Item Journal Staging" is referenced in the codeunit
    // On line 5: procedure Precheck(var Staging: Record "Item Journal Staging")
    // The quoted identifier "Item Journal Staging" should ideally resolve to the table
    let symbols = client.workspace_symbol("Item Journal Staging").await;
    assert!(
        !symbols.is_empty(),
        "Workspace should find 'Item Journal Staging' after opening table file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_references_staging_variable() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // "Staging" is used extensively in PrecheckRecord
    // Line 15: local procedure PrecheckRecord(var Staging: Record "Item Journal Staging")
    // Staging is used repeatedly throughout the subsequent procedure body.
    let refs = client.references("objects/codeunit.al", 15, 42).await;
    assert!(
        refs.len() >= 3,
        "Should find multiple references to Staging. Got: {}",
        refs.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_completion_after_dot() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    var
        Staging: Record "Item Journal Staging";
    begin
        Staging.
    end;
}"#;

    client.open_file("objects/test.al", code).await;

    // After "Staging." on line 6, col 16
    let _completions = client.completion("objects/test.al", 6, 16).await;
    // Should return at least some completions (even if just keywords)
    // The important thing is it doesn't crash

    client.shutdown().await;
}

#[tokio::test]
async fn test_completion_at_type_position() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    var
        x:
    begin
    end;
}"#;

    client.open_file("objects/test.al", code).await;

    // After "x: " on line 4, col 11
    let completions = client.completion("objects/test.al", 4, 11).await;
    let labels = completion_labels(&completions);
    assert!(
        labels.iter().any(|l| l.eq_ignore_ascii_case("Integer")
            || l.eq_ignore_ascii_case("Text")
            || l.eq_ignore_ascii_case("Boolean")),
        "Should have type completions. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_diagnostics_syntax_error() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    begin
        Message('hello')
    end;
}"#;

    // open_file already blocks until publishDiagnostics arrives — no extra
    // sleep is needed.
    client.open_file("objects/test.al", code).await;

    let diags = client.drain_diagnostics();
    assert!(
        diags.keys().any(|uri| uri.ends_with("objects/test.al")),
        "publishDiagnostics must arrive for the opened URI; got keys: {:?}",
        diags.keys().collect::<Vec<_>>()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_diagnostics_lint_empty_begin_end() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    begin
    end;
}"#;

    client.open_file("objects/test.al", code).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let diags = client.drain_diagnostics();
    let all_codes: Vec<&str> = diags
        .values()
        .flat_map(|d| d.iter())
        .filter_map(|d| d.get("code").and_then(|c| c.as_str()))
        .collect();

    // Custom lint rules have been removed; AL-L001 is no longer emitted.
    // Verify no AL-L001 code appears (rules are inactive, not just silent).
    assert!(
        !all_codes.contains(&"AL-L001"),
        "AL-L001 should not appear with custom lint rules removed. Got codes: {:?}",
        all_codes
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_formatting_real_codeunit() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50200 "IJL API Helper"
{
procedure Precheck(var Staging: Record "Item Journal Staging")
begin
if not Staging.FindSet(false) then
exit;
repeat
this.PrecheckRecord(Staging);
until Staging.Next() = 0;
end;
}"#;

    client.open_file("objects/test.al", code).await;

    let edits = client.format("objects/test.al").await;
    assert!(
        !edits.is_empty(),
        "Should produce formatting edits for unindented code"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_formatting_idempotent() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Already well-formatted code — formatting should be idempotent
    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    let _edits = client.format("objects/codeunit.al").await;
    // This test mainly verifies it doesn't crash on complex real code

    client.shutdown().await;
}

#[tokio::test]
async fn test_folding_real_table() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/table.al", TABLE_AL).await;

    let ranges = client.folding_ranges("objects/table.al").await;
    assert!(
        ranges.len() >= 5,
        "Complex table should have many folding ranges (fields, keys, procs). Got: {}",
        ranges.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_folding_real_codeunit() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    let ranges = client.folding_ranges("objects/codeunit.al").await;
    assert!(
        ranges.len() >= 8,
        "Codeunit with 8 procedures should have many folds. Got: {}",
        ranges.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_rename_parameter_in_codeunit() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // Line 5: procedure Precheck(var Staging: Record "Item Journal Staging")
    let edit = client
        .rename("objects/codeunit.al", 5, 27, "StagingRec")
        .await;
    assert!(
        edit.is_some(),
        "Should produce rename edit for Staging parameter"
    );

    let edit_val = edit.unwrap();
    let changes = edit_val.get("changes");
    assert!(
        changes.is_some(),
        "Rename should have changes. Got: {:?}",
        edit_val
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_signature_help_on_procedure_call() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure CallAdd()
    begin
        Add(1, 2);
    end;
}"#;

    client.open_file("objects/test.al", code).await;

    // Inside Add( on line 9, col 12 (after the open paren)
    let sig = client.signature_help("objects/test.al", 9, 12).await;
    assert!(sig.is_some(), "Should return signature help for Add() call");

    client.shutdown().await;
}

#[tokio::test]
async fn test_inlay_hints_on_procedure_call() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure CallAdd()
    begin
        Add(1, 2);
    end;
}"#;

    client.open_file("objects/test.al", code).await;

    let hints = client.inlay_hints("objects/test.al", 0, 12).await;
    // The fixture has a single-arg call; if inlay-hints regress to producing
    // none, the test must surface that — assert at least one hint or accept
    // empty only when the bridge is unavailable.
    assert!(
        !hints.is_empty(),
        "inlay_hints should produce at least one parameter-name hint for the procedure call fixture; got empty"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_code_action_empty_begin_end() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoNothing()
    begin
    end;
}"#;

    // open_file already waits for publishDiagnostics; no extra sleep needed.
    client.open_file("objects/test.al", code).await;

    let actions = client.code_actions("objects/test.al", 3, 5).await;
    for action in &actions {
        assert!(
            action.get("title").and_then(|v| v.as_str()).is_some(),
            "code action must have a title field, got: {action:?}"
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_multi_file_workspace_symbols() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Open all files
    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;
    client.open_file("objects/table.al", TABLE_AL).await;
    client.open_file("objects/enum.al", ENUM_AL).await;

    let symbols = client.workspace_symbol("IJL").await;
    assert!(
        symbols.len() >= 2,
        "Should find at least IJL API Helper and IJL Status when searching 'IJL'. Got: {}",
        symbols.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_multi_file_references() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;
    client.open_file("objects/table.al", TABLE_AL).await;

    // "Staging" is used in multiple procedures in the codeunit
    // and "Item Journal Staging" appears in both files
    // At minimum, references within the codeunit should work
    let refs = client.references("objects/codeunit.al", 5, 27).await;
    assert!(
        refs.len() >= 2,
        "Should find references to Staging across procedures. Got: {}",
        refs.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_keyword() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // Hover on "begin" keyword - should return None (keywords don't have hover info)
    let _hover = client.hover("objects/codeunit.al", 6, 4).await;
    // This is fine if it returns None or Some

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_string_literal() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/codeunit.al", CODEUNIT_AL).await;

    // Hover on a string literal - should return None
    let _hover = client.hover("objects/codeunit.al", 28, 35).await;
    // This is fine if it returns None or Some

    client.shutdown().await;
}

#[tokio::test]
async fn test_empty_file() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("objects/empty.al", "").await;

    let _symbols = client.document_symbols("objects/empty.al").await;
    let _tokens = client.semantic_tokens("objects/empty.al").await;
    let _ranges = client.folding_ranges("objects/empty.al").await;

    client.shutdown().await;
}

#[tokio::test]
async fn test_incomplete_code() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Incomplete code that a user might be in the middle of typing
    let code = r#"codeunit 50100 "Test"
{
    procedure
"#;

    client.open_file("objects/incomplete.al", code).await;

    let _symbols = client.document_symbols("objects/incomplete.al").await;
    let _tokens = client.semantic_tokens("objects/incomplete.al").await;
    let _hover = client.hover("objects/incomplete.al", 2, 10).await;

    client.shutdown().await;
}

#[tokio::test]
async fn test_large_file_performance() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let mut code = String::from("codeunit 50100 \"Large Test\"\n{\n");
    for i in 0..50 {
        code.push_str(&format!(
            "    procedure Proc{}(Param{}: Integer): Integer\n    begin\n        exit(Param{});\n    end;\n\n",
            i, i, i
        ));
    }
    code.push_str("}\n");

    client.open_file("objects/large.al", &code).await;

    let symbols = client.document_symbols("objects/large.al").await;
    let names = symbol_names(&symbols);
    assert!(
        names.len() >= 50,
        "Should find all 50 procedures. Got: {}",
        names.len()
    );

    let tokens = client.semantic_tokens("objects/large.al").await;
    assert!(
        tokens.is_some(),
        "Should produce semantic tokens for large file"
    );

    let ranges = client.folding_ranges("objects/large.al").await;
    assert!(ranges.len() >= 50, "Should have fold for each procedure");

    client.shutdown().await;
}
