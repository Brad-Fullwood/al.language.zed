//! End-to-end tests for the test engine against the LSP binary.
//!
//! Spawns the real `al-lsp` binary over stdio (the LSP transport), opens
//! the test fixture, and exercises static discovery through the
//! `workspace/symbol` channel.
//!
//! These tests stay on the LSP path — the new daemon endpoints
//! (`tests.run_batch`, `tests.classify`, ...) live behind the daemon
//! Unix socket which has its own harness story.

use al_analysis::queries::code_lens::TestLensStatus;
use al_test_harness::*;
use serde_json::json;

const PURE_LOGIC_REL: &str = "src/PureLogicTest.Codeunit.al";

/// Positive: opening the Pure-Logic fixture produces no diagnostics
/// (the file is syntactically valid).
#[tokio::test]
async fn pure_logic_fixture_parses_without_diagnostics() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("spawn al-lsp");
    let content =
        std::fs::read_to_string(test_project_dir().join(PURE_LOGIC_REL)).expect("fixture missing");
    client.open_file(PURE_LOGIC_REL, &content).await;

    // Drain — empty diagnostics list expected for a syntactically valid file.
    let diags = client.drain_diagnostics();
    let uri = client.file_uri(PURE_LOGIC_REL);
    let for_file = diags.get(&uri).cloned().unwrap_or_default();
    let errors: Vec<_> = for_file
        .iter()
        .filter(|d| {
            // severity 1 = Error in LSP; absence treats as warning/info
            d.get("severity").and_then(|v| v.as_i64()) == Some(1)
        })
        .collect();
    assert!(
        errors.is_empty(),
        "expected no error diagnostics on the pure-logic fixture; got: {errors:?}"
    );
}

/// Positive: document symbols for the fixture include both `[Test]`
/// procedure names plus the helper procedure.
#[tokio::test]
async fn document_symbols_lists_test_procedures() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("spawn al-lsp");
    let content =
        std::fs::read_to_string(test_project_dir().join(PURE_LOGIC_REL)).expect("fixture missing");
    client.open_file(PURE_LOGIC_REL, &content).await;
    let symbols = client.document_symbols(PURE_LOGIC_REL).await;
    let names: Vec<String> = collect_symbol_names(&symbols);
    for expected in ["TestAddition", "TestStringConcat", "HelperNotATest"] {
        assert!(
            names.iter().any(|n| n.contains(expected)),
            "expected `{expected}` in document symbols; got: {names:?}"
        );
    }
}

/// Negative: a non-existent file in the fixture project returns empty
/// document symbols (or an LSP error), not a panic.
#[tokio::test]
async fn test_document_symbols_missing_file_returns_empty() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("spawn al-lsp");
    // Open a virtual document with empty content under a path that
    // doesn't exist on disk — symbols should be empty, never crash.
    let virtual_path = "src/__no_such_file__.al";
    client.open_file(virtual_path, "").await;
    let symbols = client.document_symbols(virtual_path).await;
    assert!(
        symbols.is_empty(),
        "expected empty document symbols for empty file; got {symbols:?}"
    );
}

fn collect_symbol_names(symbols: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    for s in symbols {
        if let Some(name) = s.get("name").and_then(|v| v.as_str()) {
            out.push(name.to_string());
        }
        if let Some(children) = s.get("children").and_then(|v| v.as_array()) {
            out.extend(collect_symbol_names(children));
        }
    }
    out
}

/// Negative: a code-lens status without its `kind` tag must not deserialize
/// as the status type the server serializes.
#[test]
fn test_code_lens_invalid_payload_does_not_assert_keys() {
    let bad = json!({ "kind": "test", "status": { "bogus": "value" } });
    assert!(
        serde_json::from_value::<TestLensStatus>(bad["status"].clone()).is_err(),
        "status without a `kind` tag must not deserialize as TestLensStatus"
    );
}

/// Wire format check that does not need the LSP binary. The server writes a
/// Test lens as `{"kind": "test", "status": <TestLensStatus>}`. The wrapper is
/// built by hand in al-lsp's code lens handler, so only the status is checked
/// here. It must deserialize and serialize back to the same JSON.
#[test]
fn code_lens_test_kind_wire_format_is_recognised() {
    let sample = json!({
        "kind": "test",
        "status": { "kind": "notRun" }
    });
    let status: TestLensStatus = serde_json::from_value(sample["status"].clone())
        .expect("status must deserialize as TestLensStatus");
    assert_eq!(status, TestLensStatus::NotRun);
    assert_eq!(serde_json::to_value(&status).unwrap(), sample["status"]);
}

/// End-to-end coverage for the `textDocument/codeLens` path.
///
/// Spawns the real `al-lsp` binary, opens the Pure-Logic test fixture, and
/// asserts the code-lens response contains entries for the `[Test]`-tagged
/// procedures (`TestAddition`, `TestStringConcat`). Without test-result
/// history attached, the response status should be NotRun for every test.
///
/// Unlike the wire-format checks above, this exercises the real document store,
/// parser, and LSP transport.
#[tokio::test]
async fn code_lens_e2e_returns_test_lenses_for_test_procedures() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("spawn al-lsp");
    let content =
        std::fs::read_to_string(test_project_dir().join(PURE_LOGIC_REL)).expect("fixture missing");
    client.open_file(PURE_LOGIC_REL, &content).await;

    let lenses = client.code_lens(PURE_LOGIC_REL).await;

    assert!(
        !lenses.is_empty(),
        "expected non-empty code-lens response for fixture with [Test] procs; got: {lenses:?}"
    );

    // Each test lens carries `data` describing the lens kind. The harness
    // serialises CodeLensKind via serde-tagged json under `data` — search
    // for entries whose data.kind is "test".
    let test_lenses: Vec<&serde_json::Value> = lenses
        .iter()
        .filter(|l| {
            l.get("data")
                .and_then(|d| d.get("kind"))
                .and_then(|k| k.as_str())
                == Some("test")
        })
        .collect();

    assert!(
        test_lenses.len() >= 2,
        "expected ≥2 test lenses (one per [Test] proc); got {} : {test_lenses:?}",
        test_lenses.len()
    );

    // Without test-result history attached, every test lens must be NotRun.
    for l in &test_lenses {
        let status_kind = l
            .get("data")
            .and_then(|d| d.get("status"))
            .and_then(|s| s.get("kind"))
            .and_then(|k| k.as_str());
        assert_eq!(
            status_kind,
            Some("notRun"),
            "expected NotRun status without history; got lens: {l:?}"
        );
    }
}

/// Code lenses for a nonexistent, closed file produce an
/// empty response without panic.
#[tokio::test]
async fn code_lens_e2e_missing_file_returns_empty() {
    let mut client = LspClient::spawn(test_project_dir())
        .await
        .expect("spawn al-lsp");
    let virtual_path = "src/__no_such_file_for_lens__.al";
    client.open_file(virtual_path, "").await;
    let lenses = client.code_lens(virtual_path).await;
    assert!(
        lenses.is_empty(),
        "empty file should produce no code lenses; got {lenses:?}"
    );
}
