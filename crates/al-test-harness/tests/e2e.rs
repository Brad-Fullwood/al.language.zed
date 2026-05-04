//! End-to-end tests — spawn al-lsp and exercise every LSP capability.
//!
//! These tests use the real binary and real LSP protocol over stdio.
//! Run with: cargo test -p al-test-harness -- --test-threads=1

use al_test_harness::*;

/// Inline AL code for testing without needing a real project.
const CODEUNIT_AL: &str = r#"codeunit 50100 "Test Helper"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello, World!';
        Message(Msg);
    end;

    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    local procedure InternalHelper()
    begin
        // TODO: implement this
    end;
}"#;

const TABLE_AL: &str = r#"table 50100 "Test Table"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
        field(2; "Description"; Text[100])
        {
            Caption = 'Description';
        }
        field(3; "Amount"; Decimal)
        {
            Caption = 'Amount';
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
    }
}"#;

// ---------------------------------------------------------------------------
// Initialize / capabilities
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_starts_and_initializes() {
    let project_dir = test_project_dir();
    let client = LspClient::spawn(&project_dir).await.unwrap();
    // If we get here, initialize succeeded
    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Document symbols
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_document_symbols_codeunit() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test_codeunit.al", CODEUNIT_AL).await;

    let symbols = client.document_symbols("src/test_codeunit.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Test Helper")),
        "Should find codeunit name. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"HelloWorld"),
        "Should find HelloWorld procedure. Got: {:?}",
        names
    );
    assert!(
        names.contains(&"Add"),
        "Should find Add procedure. Got: {:?}",
        names
    );

    // T071 absence assertion: a codeunit-only file should NOT surface
    // unrelated AL object names from elsewhere in the workspace. This
    // guards against a regression where document_symbols accidentally
    // returns workspace-wide symbols for the active file.
    let unrelated = ["Sales Order Pageext", "test_table_field"];
    for name in unrelated {
        assert!(
            !names.iter().any(|n| n.contains(name)),
            "document_symbols(codeunit) leaked unrelated symbol `{name}`. Got: {names:?}"
        );
    }

    client.shutdown().await;
}

#[tokio::test]
async fn test_document_symbols_table() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test_table.al", TABLE_AL).await;

    let symbols = client.document_symbols("src/test_table.al").await;
    let names = symbol_names(&symbols);

    assert!(
        names.iter().any(|n| n.contains("Test Table")),
        "Should find table name. Got: {:?}",
        names
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_semantic_tokens_produced() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    let tokens = client.semantic_tokens("src/test.al").await;
    assert!(tokens.is_some(), "Should return semantic tokens");

    let data = semantic_token_data(&tokens.unwrap());
    assert!(!data.is_empty(), "Should have at least one token");

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hover_on_procedure_name() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // "HelloWorld" is on line 2, starts at column 14
    let hover = client.hover("src/test.al", 2, 18).await;
    assert!(
        hover.is_some(),
        "Should return hover info for procedure name"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("HelloWorld")),
        "Hover should mention HelloWorld. Got: {:?}",
        content
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_parameter() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // "A" parameter on line 10 (procedure Add(A: Integer; B: Integer))
    // A is at column 18
    let hover = client.hover("src/test.al", 10, 18).await;
    assert!(hover.is_some(), "Should return hover info for parameter A");

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains('A') || c.contains("Integer")),
        "Hover on parameter A should mention parameter name or type. Got: {:?}",
        content
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Completions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_in_procedure_body() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Put cursor after "M" to get completions
    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    begin
        M
    end;
}"#;

    client.open_file("src/test.al", code).await;

    let completions = client.completion("src/test.al", 4, 9).await;
    let labels = completion_labels(&completions);

    assert!(
        !labels.is_empty(),
        "Should return completions. Got: {:?}",
        labels
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Go to definition
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_goto_definition_local_variable() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // "Msg" used on line 6 should go to its declaration on line 4
    let def = client.definition("src/test.al", 6, 8).await;
    assert!(
        def.is_some(),
        "Should find definition for local variable Msg"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_find_references_variable() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // "Msg" is declared on line 4, used on lines 6 and 7
    let refs = client.references("src/test.al", 4, 8).await;
    assert!(
        refs.len() >= 2,
        "Should find at least 2 references for Msg (decl + usage). Got: {}",
        refs.len()
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Folding ranges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_folding_ranges() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    let ranges = client.folding_ranges("src/test.al").await;
    assert!(
        !ranges.is_empty(),
        "Should return folding ranges for codeunit"
    );

    let lines = folding_range_lines(&ranges);
    // The codeunit body should be foldable (starts at line 1 = open brace)
    assert!(
        lines.iter().any(|(_, e)| *e > 10),
        "Should have a fold spanning the codeunit body. Got: {:?}",
        lines
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_formatting() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Poorly indented code
    let code = r#"codeunit 50100 "Test"
{
procedure DoWork()
begin
Message('hello');
end;
}"#;

    client.open_file("src/test.al", code).await;

    let edits = client.format("src/test.al").await;
    // Should produce formatting edits (or empty if already formatted)
    // The code above is NOT properly indented, so we expect edits
    assert!(
        !edits.is_empty(),
        "Should produce formatting edits for poorly indented code"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_diagnostics_published_on_open() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Open a file to trigger diagnostics publication.
    // Custom lint rules have been removed, so AL-L007 will not appear,
    // but the server must still publish a diagnostics notification.
    // open_file() already waits for publishDiagnostics (5s timeout), so the
    // buffered notification is available immediately after it returns.
    client.open_file("src/test.al", CODEUNIT_AL).await;

    let uri = client.file_uri("src/test.al");
    let diags = client.drain_diagnostics();
    // Accept any notification for the opened URI (including an empty array) —
    // the key invariant is that the server published diagnostics for this file.
    assert!(
        diags.contains_key(&uri),
        "Should publish a diagnostics notification for the opened file. Got: {:?}",
        diags
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rename_variable() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // Rename "Msg" on line 4 to "MyMessage"
    let edit = client.rename("src/test.al", 4, 8, "MyMessage").await;
    assert!(
        edit.is_some(),
        "Should produce a workspace edit for renaming Msg"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Workspace symbols
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_workspace_symbol_search() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    // Open a file so it gets indexed
    client.open_file("src/test.al", CODEUNIT_AL).await;

    // initialize() polls workspace/symbol until symbols are present, so after
    // open_file() the index should include at least the codeunit we just opened.
    let symbols = client.workspace_symbol("Test").await;
    assert!(
        !symbols.is_empty(),
        "workspace/symbol should return results after opening a file with 'Test' in its name. Got 0 results."
    );

    // Verify basic structure: each symbol must have name and location
    for sym in &symbols {
        assert!(
            sym.get("name").and_then(|n| n.as_str()).is_some(),
            "workspace symbol must have a name: {sym}"
        );
        assert!(
            sym.get("location").is_some() || sym.get("containerName").is_some(),
            "workspace symbol must have location: {sym}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Hover on local variables (TypeResolver integration)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hover_on_local_variable() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    client.open_file("src/test.al", CODEUNIT_AL).await;

    // "Msg" is a local variable declared on line 4, used on line 6
    // Hover on Msg usage at line 6 should show its type (Text)
    let hover = client.hover("src/test.al", 6, 8).await;
    assert!(
        hover.is_some(),
        "Should return hover info for local variable Msg"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Msg") && c.contains("Text")),
        "Hover on Msg should show name and type. Got: {:?}",
        content
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_hover_on_local_variable_with_record_type() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    var
        CustomerRec: Record "Customer";
    begin
        CustomerRec.Name := 'test';
    end;
}"#;

    client.open_file("src/test.al", code).await;

    // Hover on CustomerRec at line 6, col 8
    let hover = client.hover("src/test.al", 6, 8).await;
    assert!(
        hover.is_some(),
        "Should return hover info for Record variable"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.is_some_and(|c| c.contains("Record") && c.contains("Customer")),
        "Hover should show Record type with Customer subtype. Got: {:?}",
        content
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// Completions include local variables
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_include_local_variables() {
    let project_dir = test_project_dir();
    let mut client = LspClient::spawn(&project_dir).await.unwrap();

    let code = r#"codeunit 50100 "Test"
{
    procedure DoWork()
    var
        MyAmount: Decimal;
        MyName: Text;
    begin

    end;
}"#;

    client.open_file("src/test.al", code).await;

    // Get completions inside the procedure body (line 7, inside begin..end)
    let completions = client.completion("src/test.al", 7, 8).await;
    let labels = completion_labels(&completions);

    assert!(
        labels.contains(&"MyAmount"),
        "Completions should include local variable MyAmount. Got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"MyName"),
        "Completions should include local variable MyName. Got: {:?}",
        labels
    );

    client.shutdown().await;
}
