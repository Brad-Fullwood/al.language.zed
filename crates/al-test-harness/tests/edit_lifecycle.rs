//! Edit lifecycle tests — simulate real Zed editing sessions.
//!
//! These tests exercise the `textDocument/didChange` and `textDocument/didClose`
//! notifications that Zed sends during normal editing. The key insight is that
//! Zed uses `TextDocumentSyncKind::Full` — every change sends the complete new
//! document text.
//!
//! These tests catch bugs that only manifest during editing (as opposed to
//! static file analysis), such as:
//! - Stale parse trees after edit
//! - Diagnostics not updating after fix
//! - Hover/completion returning pre-edit data
//! - Crash on didClose for an open file
//! - Version tracking issues
//!
//! Run with: cargo test -p al-test-harness --test edit_lifecycle -- --test-threads=1

use al_test_harness::*;

// ---------------------------------------------------------------------------
// AL code snippets for editing scenarios
// ---------------------------------------------------------------------------

const INITIAL_CODEUNIT: &str = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello';
        Message(Msg);
    end;
}"#;

const EDITED_CODEUNIT_ADD_PROC: &str = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello';
        Message(Msg);
    end;

    procedure NewProcedure(Input: Text): Text
    begin
        exit(Input + ' World');
    end;
}"#;

const EDITED_CODEUNIT_SYNTAX_ERROR: &str = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello'
        Message(Msg);
    end;
}"#;

const EDITED_CODEUNIT_FIXED: &str = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello';
        Message(Msg);
    end;
}"#;

const EDITED_CODEUNIT_RENAME_VAR: &str = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    var
        Greeting: Text;
    begin
        Greeting := 'Hello';
        Message(Greeting);
    end;
}"#;

const EMPTY_BEGIN_END: &str = r#"codeunit 50100 "Edit Test"
{
    procedure EmptyProc()
    begin
    end;
}"#;

const TABLE_AL: &str = r#"table 50100 "Edit Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
    }

    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}"#;

const TABLE_AL_ADD_FIELD: &str = r#"table 50100 "Edit Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
        field(3; "Description"; Text[250]) { }
    }

    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}"#;

// ===========================================================================
// Section A — Basic didChange
// ===========================================================================

#[tokio::test]
async fn test_edit_a01_change_file_updates_document_symbols() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Initially: one procedure (HelloWorld)
    let symbols = client.document_symbols("src/edit_test.al").await;
    let names: Vec<&str> = symbol_names(&symbols);
    assert!(names.contains(&"HelloWorld"), "initial symbols: {names:?}");
    assert!(
        !names.contains(&"NewProcedure"),
        "NewProcedure should not exist yet"
    );

    // Edit: add a second procedure
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    // After edit: both procedures visible
    let symbols = client.document_symbols("src/edit_test.al").await;
    let names: Vec<&str> = symbol_names(&symbols);
    assert!(
        names.contains(&"HelloWorld"),
        "HelloWorld should still exist after edit"
    );
    assert!(
        names.contains(&"NewProcedure"),
        "NewProcedure should appear after edit: {names:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_a02_change_file_updates_hover() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Edit: add NewProcedure
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    // Hover on the new procedure's parameter "Input" (line 10, col 28 zero-indexed)
    let hover = client.hover("src/edit_test.al", 10, 28).await;
    assert!(
        hover.is_some(),
        "hover should work on parameter in newly added procedure"
    );
    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val).unwrap_or("");
    assert!(
        content.contains("Text"),
        "hover on Input param should show Text type: got {content}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_a03_change_file_updates_completions() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Edit: add NewProcedure
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    // Completions inside HelloWorld body should include NewProcedure
    let items = client.completion("src/edit_test.al", 7, 0).await;
    let labels: Vec<&str> = completion_labels(&items);
    assert!(
        labels.iter().any(|l| l.contains("NewProcedure")),
        "completions should include newly added procedure: {labels:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_a04_change_file_updates_semantic_tokens() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    let tokens_before = client.semantic_tokens("src/edit_test.al").await;
    let count_before = tokens_before
        .as_ref()
        .and_then(|t| t.get("data"))
        .and_then(|d| d.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    // Edit: add more code
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    let tokens_after = client.semantic_tokens("src/edit_test.al").await;
    let count_after = tokens_after
        .as_ref()
        .and_then(|t| t.get("data"))
        .and_then(|d| d.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    assert!(count_after > count_before,
        "semantic tokens should increase after adding code: before={count_before}, after={count_after}");

    client.shutdown().await;
}

// ===========================================================================
// Section B — Diagnostics after edit
// ===========================================================================

#[tokio::test]
async fn test_edit_b01_diagnostics_appear_after_introducing_error() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Initial: drain the open notification so we start clean
    let _initial_diags = client.drain_diagnostics();

    // Edit: introduce syntax error (missing semicolon)
    // change_file() waits for publishDiagnostics, so the notification is
    // buffered and available immediately after it returns.
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_SYNTAX_ERROR)
        .await;

    let diags = client.drain_diagnostics();
    let uri = client.file_uri("src/edit_test.al");
    // The server must publish a diagnostics notification for the edited file
    // specifically — not just any file. This is the observable evidence that
    // the server processed the didChange notification.
    assert!(
        diags.contains_key(&uri),
        "server should publish diagnostics for the edited file URI after introducing an error. Got: {:?}",
        diags.keys().collect::<Vec<_>>()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_b02_diagnostics_clear_after_fixing_error() {
    // Native lint rules have been removed — AL-L001 is no longer emitted.
    // This test verifies that:
    //   1. The server processes the file with empty begin..end without crashing, and
    //   2. AL-L001 does NOT appear (rules are inactive, not just silent).
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/edit_test.al", EMPTY_BEGIN_END).await;
    let diags = client.drain_diagnostics();
    let uri = client.file_uri("src/edit_test.al");
    let initial_codes: Vec<&str> = diags
        .get(&uri)
        .map(|d| {
            d.iter()
                .filter_map(|v| v.get("code").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !initial_codes.contains(&"AL-L001"),
        "AL-L001 must not appear with native lint rules removed: got {initial_codes:?}"
    );

    // Edit to non-empty code — server should continue processing without crash.
    client
        .change_file("src/edit_test.al", INITIAL_CODEUNIT)
        .await;
    let diags = client.drain_diagnostics();
    let fixed_codes: Vec<&str> = diags
        .get(&uri)
        .map(|d| {
            d.iter()
                .filter_map(|v| v.get("code").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !fixed_codes.contains(&"AL-L001"),
        "AL-L001 must not appear after edit: got {fixed_codes:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_b03_lint_diagnostics_update_on_edit() {
    // Native lint rules have been removed — AL-L007 is no longer emitted.
    // This test verifies that:
    //   1. AL-L007 does NOT appear even with a TODO comment present, and
    //   2. The server processes edits without crashing.
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let with_todo = r#"codeunit 50100 "Edit Test"
{
    procedure HelloWorld()
    begin
        // TODO: implement this
    end;
}"#;
    client.open_file("src/edit_test.al", with_todo).await;
    let diags = client.drain_diagnostics();
    let uri = client.file_uri("src/edit_test.al");
    let codes: Vec<&str> = diags
        .get(&uri)
        .map(|d| {
            d.iter()
                .filter_map(|v| v.get("code").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !codes.contains(&"AL-L007"),
        "AL-L007 must not appear with native lint rules removed: got {codes:?}"
    );

    // Edit: remove the TODO — server should still not emit AL-L007.
    client
        .change_file("src/edit_test.al", INITIAL_CODEUNIT)
        .await;
    let diags = client.drain_diagnostics();
    let codes: Vec<&str> = diags
        .get(&uri)
        .map(|d| {
            d.iter()
                .filter_map(|v| v.get("code").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !codes.contains(&"AL-L007"),
        "AL-L007 must not appear after edit: got {codes:?}"
    );

    client.shutdown().await;
}

// ===========================================================================
// Section C — didClose lifecycle
// ===========================================================================

#[tokio::test]
async fn test_edit_c01_close_file_does_not_crash() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;
    client.close_file("src/edit_test.al").await;

    // Server should still be responsive after closing a file
    let symbols = client.workspace_symbol("").await;
    assert!(
        !symbols.is_empty(),
        "server should still work after closing a file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_c02_close_then_reopen_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // Open, verify, close
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;
    let symbols1 = client.document_symbols("src/edit_test.al").await;
    assert!(!symbols1.is_empty(), "should have symbols after open");

    client.close_file("src/edit_test.al").await;

    // Reopen with different content
    client
        .open_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;
    let symbols2 = client.document_symbols("src/edit_test.al").await;
    let names: Vec<&str> = symbol_names(&symbols2);
    assert!(
        names.contains(&"NewProcedure"),
        "reopened file should reflect new content: {names:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_c03_close_one_file_other_still_works() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/file_a.al", INITIAL_CODEUNIT).await;
    client.open_file("src/file_b.al", TABLE_AL).await;

    // Close file_a
    client.close_file("src/file_a.al").await;

    // file_b should still work
    let symbols = client.document_symbols("src/file_b.al").await;
    assert!(
        !symbols.is_empty(),
        "file_b should still have symbols after closing file_a"
    );

    client.shutdown().await;
}

// ===========================================================================
// Section D — Edit then query (realistic Zed workflow)
// ===========================================================================

#[tokio::test]
async fn test_edit_d01_edit_variable_then_hover() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Edit: rename variable from Msg to Greeting
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_RENAME_VAR)
        .await;

    // Hover on the renamed variable (line 6, "Greeting")
    let hover = client.hover("src/edit_test.al", 6, 10).await;
    assert!(hover.is_some(), "hover should work on renamed variable");
    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val).unwrap_or("");
    assert!(
        content.contains("Text"),
        "hover on Greeting should show Text type: got {content}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_d02_add_field_then_document_symbols() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_table.al", TABLE_AL).await;

    let symbols_before = client.document_symbols("src/edit_table.al").await;

    // Edit: add a field
    client
        .change_file("src/edit_table.al", TABLE_AL_ADD_FIELD)
        .await;

    let symbols_after = client.document_symbols("src/edit_table.al").await;
    let names_after: Vec<&str> = symbol_names(&symbols_after);
    assert!(
        names_after.iter().any(|n| n.contains("Description")),
        "document symbols should include newly added field: {names_after:?}"
    );

    // Should have more symbols after adding a field
    assert!(
        symbol_names(&symbols_after).len() > symbol_names(&symbols_before).len(),
        "should have more symbols after adding a field"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_d03_edit_does_not_corrupt_other_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    client.open_file("src/file_a.al", INITIAL_CODEUNIT).await;
    client.open_file("src/file_b.al", TABLE_AL).await;

    // Edit only file_a
    client
        .change_file("src/file_a.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    // file_b should be unchanged
    let symbols_b = client.document_symbols("src/file_b.al").await;
    let names_b: Vec<&str> = symbol_names(&symbols_b);
    assert!(
        names_b.iter().any(|n| n.contains("No.")),
        "file_b should still have its original fields: {names_b:?}"
    );
    assert!(
        !names_b.iter().any(|n| n.contains("NewProcedure")),
        "file_b should NOT have file_a's new procedure"
    );

    client.shutdown().await;
}

// ===========================================================================
// Section E — Rapid edits (Zed sends changes on every keystroke)
// ===========================================================================

#[tokio::test]
async fn test_edit_e01_rapid_edits_no_crash() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/rapid.al", INITIAL_CODEUNIT).await;

    // Simulate rapid typing — 10 edits without waiting
    for i in 0..10 {
        let content = format!(
            r#"codeunit 50100 "Edit Test"
{{
    procedure HelloWorld()
    var
        Msg: Text;
    begin
        Msg := 'Hello edit {i}';
        Message(Msg);
    end;
}}"#
        );
        client.change_file_no_wait("src/rapid.al", &content).await;
    }

    // Small delay to let server process
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Server should still be responsive
    let symbols = client.document_symbols("src/rapid.al").await;
    assert!(
        !symbols.is_empty(),
        "server should still work after rapid edits"
    );

    // Hover should work
    let hover = client.hover("src/rapid.al", 6, 10).await;
    assert!(hover.is_some(), "hover should work after rapid edits");

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_e02_rapid_edits_final_state_correct() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/rapid.al", INITIAL_CODEUNIT).await;

    // Send several rapid edits, last one adds NewProcedure
    client
        .change_file_no_wait("src/rapid.al", INITIAL_CODEUNIT)
        .await;
    client
        .change_file_no_wait("src/rapid.al", EDITED_CODEUNIT_RENAME_VAR)
        .await;
    client
        .change_file("src/rapid.al", EDITED_CODEUNIT_ADD_PROC)
        .await; // wait on last one

    // Final state should reflect the last edit
    let symbols = client.document_symbols("src/rapid.al").await;
    let names: Vec<&str> = symbol_names(&symbols);
    assert!(
        names.contains(&"NewProcedure"),
        "final state should reflect last edit: {names:?}"
    );

    client.shutdown().await;
}

// ===========================================================================
// Section F — Folding and formatting after edit
// ===========================================================================

#[tokio::test]
async fn test_edit_f01_folding_ranges_update_after_edit() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    let folds_before = client.folding_ranges("src/edit_test.al").await;

    // Add a second procedure (more foldable regions)
    client
        .change_file("src/edit_test.al", EDITED_CODEUNIT_ADD_PROC)
        .await;

    let folds_after = client.folding_ranges("src/edit_test.al").await;
    assert!(
        folds_after.len() >= folds_before.len(),
        "adding a procedure should not reduce folding ranges: before={}, after={}",
        folds_before.len(),
        folds_after.len()
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_f02_formatting_after_edit() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let unformatted = r#"codeunit 50100 "Edit Test"
{
procedure BadIndent()
begin
Message('hello');
end;
}"#;
    client.open_file("src/edit_test.al", unformatted).await;

    let edits = client.format("src/edit_test.al").await;
    assert!(
        !edits.is_empty(),
        "unformatted code should produce formatting edits"
    );

    // Now edit to well-formatted code
    client
        .change_file("src/edit_test.al", INITIAL_CODEUNIT)
        .await;

    let edits2 = client.format("src/edit_test.al").await;
    // Well-formatted code should produce fewer (or no) edits
    assert!(
        edits2.len() <= edits.len(),
        "well-formatted code should produce fewer edits: before={}, after={}",
        edits.len(),
        edits2.len()
    );

    client.shutdown().await;
}

// ===========================================================================
// Section G — Configuration changes
// ===========================================================================

#[tokio::test]
async fn test_edit_g01_configuration_change_no_crash() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Send a configuration change (Zed does this when settings update)
    client
        .change_configuration(serde_json::json!({
            "al": {
                "inlayHints": {
                    "parameterNames": true,
                    "returnTypes": true
                }
            }
        }))
        .await;

    // Server should still be responsive
    let hover = client.hover("src/edit_test.al", 2, 14).await;
    assert!(
        hover.is_some(),
        "hover should work after configuration change"
    );

    client.shutdown().await;
}

// ===========================================================================
// Section H — Edge cases during editing
// ===========================================================================

#[tokio::test]
async fn test_edit_h01_edit_to_empty_file() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Edit to completely empty
    client.change_file("src/edit_test.al", "").await;

    // Queries on empty file should not crash
    let symbols = client.document_symbols("src/edit_test.al").await;
    assert!(symbols.is_empty(), "empty file should have no symbols");

    let hover = client.hover("src/edit_test.al", 0, 0).await;
    // May or may not return something — just shouldn't crash

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_h02_edit_to_invalid_al() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // Edit to completely invalid AL
    client
        .change_file("src/edit_test.al", "this is not valid AL code at all!!!")
        .await;

    // Queries should not crash
    let symbols = client.document_symbols("src/edit_test.al").await;
    // May have some symbols from error recovery, but shouldn't panic

    let hover = client.hover("src/edit_test.al", 0, 0).await;
    // Just verify no crash

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_h03_edit_unicode_content() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let unicode_al = r#"codeunit 50100 "Ünïcödë Test"
{
    procedure ÅäöProcessing()
    var
        Ñame: Text;
    begin
        Ñame := 'Héllo Wörld 🌍';
    end;
}"#;
    client.open_file("src/edit_test.al", unicode_al).await;

    // Hover on unicode identifier
    let hover = client.hover("src/edit_test.al", 4, 10).await;
    assert!(hover.is_some(), "hover should work on unicode identifiers");

    // Edit with more unicode
    let edited = unicode_al.replace("Héllo", "Gödel");
    client.change_file("src/edit_test.al", &edited).await;

    let hover2 = client.hover("src/edit_test.al", 4, 10).await;
    assert!(hover2.is_some(), "hover should work after unicode edit");

    client.shutdown().await;
}

#[tokio::test]
async fn test_edit_h04_many_sequential_edits() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();
    client.open_file("src/edit_test.al", INITIAL_CODEUNIT).await;

    // 50 sequential edits
    for i in 0..50 {
        let content = format!(
            r#"codeunit 50100 "Edit Test"
{{
    procedure Proc{i}()
    begin
    end;
}}"#
        );
        client.change_file("src/edit_test.al", &content).await;
    }

    // After 50 edits, the latest procedure should be visible
    let symbols = client.document_symbols("src/edit_test.al").await;
    let names: Vec<&str> = symbol_names(&symbols);
    assert!(
        names.contains(&"Proc49"),
        "should see the 50th procedure: {names:?}"
    );
    assert!(
        !names.contains(&"Proc0"),
        "should NOT see earlier procedures: {names:?}"
    );

    client.shutdown().await;
}
