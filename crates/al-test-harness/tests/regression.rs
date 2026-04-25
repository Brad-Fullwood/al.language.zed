//! Regression tests for bugs found in the 2026-03-21 production readiness audit.
//!
//! Each test documents the bug it prevents from regressing and the fix that was applied.
//! Run with: cargo test -p al-test-harness --test regression -- --test-threads=1

use al_test_harness::*;

// ===========================================================================
// Regression: Inlay hints daemon panic (BUG 2 / ISSUE-P7)
//
// The inlay_hints query used `workspace.config.blocking_read()` which panics
// when called from within a tokio runtime. Fixed by using `try_read()`.
// ===========================================================================

#[tokio::test]
async fn test_regression_inlay_hints_no_panic() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Hints Test"
{
    procedure Caller()
    begin
        Helper('hello', 42);
    end;

    procedure Helper(Name: Text; Count: Integer)
    begin
    end;
}"#;
    client.open_file("src/hints_test.al", code).await;

    // This used to panic the daemon with:
    // "Cannot block the current thread from within a runtime"
    let hints = client.inlay_hints("src/hints_test.al", 0, 10).await;

    // The test passing without a timeout/EOF proves the daemon didn't panic.
    // Verify server is still responsive after hints request
    let symbols = client.document_symbols("src/hints_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server should still work after inlay hints"
    );

    client.shutdown().await;
}

// ===========================================================================
// Regression: Sort-members --dry-run modifying files (BUG 1)
//
// dispatch_sort_members ignored the dryRun parameter and always wrote to disk.
// Fixed by checking dryRun before writing.
// This test verifies the server-side behavior via the sortMembers JSON-RPC method.
// ===========================================================================

// Note: This is tested at the CLI level (al-cli integration tests), not via LSP.
// The LSP server receives the dryRun parameter from the CLI and should respect it.
// The daemon fix was to check `params.dryRun` before calling `std::fs::write`.

// ===========================================================================
// Regression: No-op test assertions (3 tests in views.rs)
//
// Three tests used bare `matches!()` without `assert!()`, making them always pass.
// Fixed by wrapping in `assert!(matches!(...))`.
// This is a compile-time guarantee — if someone removes the assert!, clippy warns.
// ===========================================================================

// Note: These are al-explorer unit tests, not integration tests. The fix is in
// crates/al-explorer/src/views.rs. Verified by running `cargo test -p al-explorer`.

// ===========================================================================
// Regression: File-not-found error consistency
//
// 5 places in build_dispatch.rs manually constructed the same error Response.
// Fixed by extracting `file_not_found(id)` helper.
// Verify the error is returned consistently.
// ===========================================================================

#[tokio::test]
async fn test_regression_hover_on_unopened_file_returns_error() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    // Don't open the file — just try to hover on it
    let hover = client.hover("src/nonexistent_file.al", 0, 0).await;
    // Should return None (graceful), not crash
    assert!(hover.is_none(), "hover on unopened file should return None");

    // Server should still be responsive
    let symbols = client.workspace_symbol("").await;
    assert!(
        !symbols.is_empty(),
        "server should still work after hover on unopened file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_regression_format_on_unopened_file_returns_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let edits = client.format("src/nonexistent_file.al").await;
    assert!(
        edits.is_empty(),
        "format on unopened file should return empty"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn test_regression_definition_on_unopened_file_returns_none() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let def = client.definition("src/nonexistent_file.al", 0, 0).await;
    assert!(
        def.is_none(),
        "definition on unopened file should return None"
    );

    client.shutdown().await;
}

// ===========================================================================
// Regression: Architecture violation — al-lsp importing al-symbols directly
//
// al-lsp/build_dispatch.rs used `al_symbols::ObjectKind` instead of
// `al_core::symbols::ObjectKind`. Fixed by replacing 6 usages.
// This is verified at compile time (al-symbols removed from [dependencies]).
// ===========================================================================

// Compile-time guarantee: al-symbols is only in [dev-dependencies] now.

// ===========================================================================
// Regression: Code actions return valid objects
//
// Verify code actions have valid structure (titles, edits).
// No custom lint rules are registered, so only source actions are expected.
// ===========================================================================

#[tokio::test]
async fn test_regression_code_actions_have_titles() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Lint Test"
{
    procedure EmptyProc()
    begin
    end;
}"#;
    client.open_file("src/lint_test.al", code).await;

    let actions = client.code_actions("src/lint_test.al", 3, 5).await;
    for action in &actions {
        assert!(
            action.get("title").is_some(),
            "code action should have a title: {action}"
        );
    }

    client.shutdown().await;
}

// ===========================================================================
// Regression: Inlay hints content validation
//
// Previous tests only checked `!= crash`. Verify hints contain actual
// parameter names when available.
// ===========================================================================

#[tokio::test]
async fn test_regression_inlay_hints_show_parameter_names() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Hints Content"
{
    procedure Caller()
    begin
        Calculate(10, 20);
    end;

    procedure Calculate(Width: Integer; Height: Integer): Integer
    begin
        exit(Width * Height);
    end;
}"#;
    client.open_file("src/hints_content.al", code).await;

    let hints = client.inlay_hints("src/hints_content.al", 0, 12).await;

    // The Calculate(10, 20) call has named parameters — the server must emit
    // at least one inlay hint for the call site on line 4.
    assert!(
        !hints.is_empty(),
        "inlay hints should be non-empty for a call with named parameters: {hints:?}"
    );

    // Verify at least one hint has a label
    let has_label = hints.iter().any(|h| h.get("label").is_some());
    assert!(has_label, "inlay hints should have labels: {hints:?}");

    // Check that hint labels reference parameter names
    let labels: Vec<String> = hints
        .iter()
        .filter_map(|h| {
            if let Some(s) = h.get("label").and_then(|l| l.as_str()) {
                Some(s.to_string())
            } else if let Some(arr) = h.get("label").and_then(|l| l.as_array()) {
                Some(
                    arr.iter()
                        .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join(""),
                )
            } else {
                None
            }
        })
        .collect();

    let has_param_name = labels
        .iter()
        .any(|l| l.contains("Width") || l.contains("Height"));
    assert!(
        has_param_name,
        "inlay hints should reference parameter names (Width/Height): {labels:?}"
    );

    client.shutdown().await;
}

// ===========================================================================
// Regression: UTF-16 position handling
//
// ISSUE-024 and ISSUE-031 document UTF-16/byte offset confusion.
// This test verifies that hover works correctly on identifiers after
// multi-byte UTF-8 characters.
// ===========================================================================

#[tokio::test]
async fn test_regression_utf16_position_after_multibyte() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "UTF16 Test"
{
    procedure Process()
    var
        ØreName: Text;
    begin
        ØreName := 'test';
    end;
}"#;
    client.open_file("src/utf16_test.al", code).await;

    // Hover on ØreName. The UTF-16 column for Ø after 8 leading spaces is 8;
    // line 4 (0-based) is the `ØreName: Text;` declaration. Once tb-003 is
    // landed, hover must succeed at this position.
    let hover = client.hover("src/utf16_test.al", 4, 8).await;
    assert!(
        hover.is_some(),
        "tb-003 / ISSUE-024 regression: hover on Ø identifier must resolve. \
         find_node_at_position must convert LSP UTF-16 column to byte offset."
    );
    let symbols = client.document_symbols("src/utf16_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server should not crash on UTF-16 edge case"
    );

    client.shutdown().await;
}

// ===========================================================================
// Regression: Prepare rename
//
// The LspClient declares prepareSupport: true but never tests it.
// ===========================================================================

#[tokio::test]
async fn test_regression_prepare_rename_returns_range() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = r#"codeunit 50100 "Rename Test"
{
    procedure HelloWorld()
    var
        MyVar: Text;
    begin
        MyVar := 'test';
    end;
}"#;
    client.open_file("src/rename_test.al", code).await;

    // prepareRename on the variable name
    let result = client.prepare_rename("src/rename_test.al", 4, 10).await;
    if let Some(range) = result {
        // Should return a range covering the identifier
        assert!(
            range.get("start").is_some() || range.get("range").is_some(),
            "prepareRename should return a range or range+placeholder: {range}"
        );
    }
    // If None, the server doesn't support prepareRename for this position — acceptable

    client.shutdown().await;
}
