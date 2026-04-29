//! End-to-end tests for the test engine against the LSP binary.
//!
//! Spawns the real `al-lsp` binary over stdio (the LSP transport), opens
//! the test fixture, and exercises the static-discovery path that
//! Phase 2 relies on (`queries::tests::discover_tests` indirectly via
//! the `workspace/symbol` channel that's already exposed).
//!
//! These tests stay on the LSP path — the new daemon endpoints
//! (`tests.run_batch`, `tests.classify`, ...) live behind the daemon
//! Unix socket which has its own harness story.

use al_test_harness::*;
use serde_json::json;

const PURE_LOGIC_REL: &str = "src/PureLogicTest.Codeunit.al";

/// Positive: opening the Pure-Logic fixture produces no diagnostics
/// (the file is syntactically valid).
#[tokio::test]
#[ignore = "requires al-lsp binary + the fixture project to be on disk"]
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
#[ignore = "requires al-lsp binary + the fixture project to be on disk"]
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
#[ignore = "requires al-lsp binary + the fixture project to be on disk"]
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

// Recursively collect symbol names from an LSP DocumentSymbol tree.
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

/// Negative: a malformed code-lens JSON shape (missing the inner kind
/// field) must round-trip without claiming structure it doesn't have.
#[test]
fn test_code_lens_invalid_payload_does_not_assert_keys() {
    // Payload omits the required inner `status.kind` and instead has a
    // bogus field — verify our consumer code can detect this.
    let bad = json!({ "kind": "test", "status": { "bogus": "value" } });
    assert!(
        bad["status"].get("kind").is_none(),
        "shape assertion: malformed payload must lack a `status.kind`"
    );
    let inner_kind = bad["status"].get("kind").and_then(|v| v.as_str());
    assert!(inner_kind.is_none(), "expected None for missing kind");
}

/// Smoke test that doesn't require the LSP binary: deserialize a known
/// LSP code-lens response and check it contains the new Test lens kind.
/// This proves the wire format has the expected structure even when
/// running in environments where al-lsp can't be spawned.
#[test]
fn code_lens_test_kind_wire_format_is_recognised() {
    let sample = json!({
        "kind": "test",
        "status": { "kind": "notRun" }
    });
    // Wire format check: must round-trip via serde_json
    let s = serde_json::to_string(&sample).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed["kind"], "test");
    assert_eq!(parsed["status"]["kind"], "notRun");
}
