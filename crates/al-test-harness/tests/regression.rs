//! End-to-end regressions for LSP behavior and lifecycle edge cases.

use al_test_harness::*;

#[tokio::test]
async fn hover_on_unopened_file_returns_error() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let hover = client.hover("src/nonexistent_file.al", 0, 0).await;
    assert!(hover.is_none(), "hover on unopened file should return None");

    let symbols = client.workspace_symbol("").await;
    assert!(
        !symbols.is_empty(),
        "server should still work after hover on unopened file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn format_on_unopened_file_returns_empty() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let edits = client.format("src/nonexistent_file.al").await;
    assert!(
        edits.is_empty(),
        "format on unopened file should return empty"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn definition_on_unopened_file_returns_none() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let def = client.definition("src/nonexistent_file.al", 0, 0).await;
    assert!(
        def.is_none(),
        "definition on unopened file should return None"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn code_actions_have_titles() {
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

#[tokio::test]
async fn inlay_hints_show_parameter_names() {
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

    assert!(
        !hints.is_empty(),
        "inlay hints should be non-empty for a call with named parameters: {hints:?}"
    );

    let has_label = hints.iter().any(|h| h.get("label").is_some());
    assert!(has_label, "inlay hints should have labels: {hints:?}");

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

#[tokio::test]
async fn utf16_position_after_multibyte() {
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

    let hover = client.hover("src/utf16_test.al", 4, 8).await;
    assert!(
        hover.is_some(),
        "hover on Ø identifier must resolve. \
         find_node_at_position must convert LSP UTF-16 column to byte offset."
    );
    let symbols = client.document_symbols("src/utf16_test.al").await;
    assert!(
        !symbols.is_empty(),
        "server should not crash on UTF-16 edge case"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn prepare_rename_returns_range() {
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

    let result = client.prepare_rename("src/rename_test.al", 4, 10).await;
    let range = result.expect(
        "prepareRename on a renamable identifier (MyVar) must return Some — \
         accepting None hides regressions in prepareRename support",
    );
    assert!(
        range.get("start").is_some() || range.get("range").is_some(),
        "prepareRename should return a range or range+placeholder: {range}"
    );

    let on_keyword = client.prepare_rename("src/rename_test.al", 2, 4).await;
    assert!(
        on_keyword.is_none(),
        "prepareRename on the `procedure` keyword must return None, got: {on_keyword:?}"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn no_ghost_diagnostics_after_close_during_debounce() {
    let mut client = LspClient::spawn(test_project_dir()).await.unwrap();

    let code = "codeunit 50100 \"Ghost Test\"\n{\n    procedure Broken(\n    begin\n    end;\n}\n";
    client.open_file("src/ghost_test.al", code).await;

    let _ = client.drain_diagnostics();

    let edit = "codeunit 50100 \"Ghost Test\"\n{\n    procedure Broken(arg: Integer\n    begin\n    end;\n}\n";
    client.change_file_no_wait("src/ghost_test.al", edit).await;
    client.close_file("src/ghost_test.al").await;

    // did_close clears the file with an empty publish. That clear is the anchor:
    // every publish for this URI after it must be empty too. A non-empty one is
    // the debounced pass from the didChange waking up after the close and
    // painting squiggles on a document the editor no longer shows.
    let uri = format!("file://{}/src/ghost_test.al", test_project_dir().display());
    let mut sequence: Vec<String> = Vec::new();
    let mut cleared = false;
    let mut ghosts: Vec<String> = Vec::new();

    // One window, long enough to outlast the server's 400 ms debounce, so a
    // ghost that arrives in the same batch as the clear is still caught.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        for (published_uri, diagnostics) in client.drain_diagnostic_publishes() {
            if published_uri != uri {
                continue;
            }
            let described = describe_publish(&diagnostics);
            if cleared && !diagnostics.is_empty() {
                ghosts.push(described.clone());
            }
            cleared |= diagnostics.is_empty();
            sequence.push(described);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
    }

    assert!(
        cleared,
        "did_close must clear diagnostics for {uri} with an empty publish; publishes seen: {sequence:?}"
    );
    assert!(
        ghosts.is_empty(),
        "ghost diagnostics published for {uri} after did_close cleared it: {}\nfull publish sequence: {:?}",
        ghosts.join(" | "),
        sequence
    );

    client.shutdown().await;
}

/// One publishDiagnostics payload as a short line, so an assertion failure
/// names the diagnostics that were published instead of printing a count.
fn describe_publish(diagnostics: &[serde_json::Value]) -> String {
    if diagnostics.is_empty() {
        return "[] (cleared)".to_string();
    }
    let described: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| {
            let message = diagnostic["message"].as_str().unwrap_or("<no message>");
            match diagnostic["range"]["start"]["line"].as_u64() {
                Some(line) => format!("line {line}: {message}"),
                None => message.to_string(),
            }
        })
        .collect();
    format!("[{}]", described.join("; "))
}
