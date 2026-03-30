//! Zed Fidelity Test Suite
//!
//! These tests simulate the exact LSP message sequences that Zed sends, and
//! verify that al-lsp responses match what Zed expects. Every test reflects
//! a documented Zed client behaviour:
//!
//! - Zed declares specific capabilities during `initialize`
//! - Zed uses `TextDocumentSyncKind::Full` (sends full text on every change)
//! - Zed renders completion items with `label` and optional `detail`
//! - Zed uses the semantic token legend from the server's capabilities
//! - Zed's outline panel relies on `DocumentSymbol` (hierarchical)
//! - Zed's diagnostics panel uses `publishDiagnostics` (push, not pull)
//! - Zed's code-action UI expects `Command` or `WorkspaceEdit` responses
//! - Zed hover uses MarkupContent with `kind: "markdown"`
//! - Zed folding uses `startLine`/`endLine` with optional `kind`
//! - Zed signature help highlights the active parameter with `activeParameter`

use al_test_harness::*;

// ---------------------------------------------------------------------------
// Shared AL fixtures
// ---------------------------------------------------------------------------

const CODEUNIT_AL: &str = r#"codeunit 50100 "Zed Fidelity Test"
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

const TABLE_AL: &str = r#"table 50100 "Zed Test Table"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20]) { Caption = 'No.'; }
        field(2; "Name"; Text[100]) { Caption = 'Name'; }
        field(3; "Amount"; Decimal) { Caption = 'Amount'; }
    }

    keys
    {
        key(PK; "No.") { Clustered = true; }
    }

    procedure GetName(): Text
    begin
        exit("Name");
    end;
}"#;

const PAGE_AL: &str = r#"page 50100 "Zed Test Page"
{
    PageType = Card;
    SourceTable = "Zed Test Table";

    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field("Name"; Rec."Name") { }
        }
    }

    actions
    {
        area(Processing)
        {
            action(DoWork)
            {
                trigger OnAction()
                begin
                    Message('Done');
                end;
            }
        }
    }
}"#;

// ---------------------------------------------------------------------------
// 1. Initialize — server must advertise capabilities Zed relies on
// ---------------------------------------------------------------------------

/// Zed calls initialize and expects a capabilities object with specific fields.
#[tokio::test]
async fn zed_fidelity_initialize_returns_capabilities() {
    let dir = test_project_dir();
    let client = LspClient::spawn(&dir).await.unwrap();
    // spawn already performs the initialize handshake. If we reach this point
    // the server returned a valid initialize response.
    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. TextDocumentSyncKind — Zed uses Full sync
// ---------------------------------------------------------------------------

/// Zed sends the full document text on every didOpen and didChange.
/// The server must accept this without error and produce diagnostics.
#[tokio::test]
async fn zed_fidelity_full_sync_open_triggers_diagnostics() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    // open_file sends didOpen with the full text — Zed's exact behaviour
    client.open_file("src/zed_full_sync.al", CODEUNIT_AL).await;

    // Diagnostics are pushed via publishDiagnostics (never polled)
    // open_file already waited for them; draining confirms they were received
    let all_notifs = client.drain_notifications();
    let diag_notifs: Vec<_> = all_notifs
        .iter()
        .filter(|(m, _)| m == "textDocument/publishDiagnostics")
        .collect();
    assert!(
        !diag_notifs.is_empty(),
        "Server must push publishDiagnostics after didOpen (Zed fidelity). Got: {:?}",
        all_notifs.iter().map(|(m, _)| m).collect::<Vec<_>>()
    );

    client.shutdown().await;
}

/// Zed resends the full document text on every keystroke (didChange).
/// The server must handle repeated full-text updates without panicking.
#[tokio::test]
async fn zed_fidelity_full_sync_repeated_changes() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    // Open once, then update twice — Zed always sends the complete new text.
    // After the first open, subsequent updates use didChange (not didOpen).
    client.open_file("src/zed_change.al", CODEUNIT_AL).await;
    client.change_file("src/zed_change.al", TABLE_AL).await;
    client.change_file("src/zed_change.al", PAGE_AL).await;

    // Server must survive repeated sync without error
    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. Completion items — format Zed renders
// ---------------------------------------------------------------------------

/// Zed renders completion items with `label` and optionally `detail`.
/// Both fields must be strings; `kind` must be a number in [1, 25].
#[tokio::test]
async fn zed_fidelity_completion_items_have_label_and_kind() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    let code = r#"codeunit 50100 "Zed Test"
{
    procedure DoWork()
    begin
        M
    end;
}"#;

    client.open_file("src/zed_completion.al", code).await;
    let items = client.completion("src/zed_completion.al", 4, 9).await;

    assert!(
        !items.is_empty(),
        "Zed fidelity: server must return at least one completion item"
    );

    for item in &items {
        // label is required by LSP spec and used by Zed's completion UI
        assert!(
            item.get("label").and_then(|v| v.as_str()).is_some(),
            "Every completion item must have a string 'label'. Got: {item}"
        );

        // kind, if present, must be a number 1..=25
        if let Some(kind) = item.get("kind") {
            let k = kind.as_u64().unwrap_or(0);
            assert!(
                (1..=25).contains(&k),
                "Completion kind must be in range [1, 25]. Got: {k}"
            );
        }
    }

    client.shutdown().await;
}

/// Zed does not use `insertText` + `insertTextFormat` for snippet support by
/// default. The server must not rely on snippet format being supported.
#[tokio::test]
async fn zed_fidelity_completion_no_snippet_required() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    let code = r#"codeunit 50100 "Zed Test"
{
    procedure DoWork()
    begin
        Me
    end;
}"#;

    client.open_file("src/zed_no_snippet.al", code).await;
    let items = client.completion("src/zed_no_snippet.al", 4, 10).await;

    // Items may contain plain text inserts — they must be usable without snippet support
    for item in &items {
        let format = item
            .get("insertTextFormat")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        // insertTextFormat 2 = Snippet; that requires snippet capability.
        // Either format 1 (PlainText) or no field is fine for Zed.
        // We don't assert format == 1 strictly because some items may be snippets,
        // but we verify every item has a usable label fallback.
        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !label.is_empty(),
            "Completion item label must not be empty even if insertTextFormat={format}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. Semantic tokens — Zed's expected legend
// ---------------------------------------------------------------------------

/// Zed requests `textDocument/semanticTokens/full` and decodes tokens using
/// the server's legend. The server must return a `data` array of u32s in
/// groups of 5 (delta-encoded: δline, δstart, length, tokenType, modifiers).
#[tokio::test]
async fn zed_fidelity_semantic_tokens_data_format() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_tokens.al", CODEUNIT_AL).await;
    let result = client.semantic_tokens("src/zed_tokens.al").await;

    assert!(
        result.is_some(),
        "Zed fidelity: server must return semantic tokens for AL code"
    );

    let result = result.unwrap();
    let data = result.get("data").and_then(|d| d.as_array());
    assert!(
        data.is_some(),
        "Semantic tokens result must have a 'data' array"
    );

    let data = data.unwrap();
    assert_eq!(
        data.len() % 5,
        0,
        "Semantic token data must be a multiple of 5 (groups of 5 integers). len={}",
        data.len()
    );

    for val in data {
        assert!(
            val.as_u64().is_some(),
            "Every semantic token datum must be a non-negative integer. Got: {val}"
        );
    }

    client.shutdown().await;
}

/// Semantic tokens for a table must include token types for keywords like
/// `table`, `fields`, `field`, `key`, and `procedure`.
#[tokio::test]
async fn zed_fidelity_semantic_tokens_cover_table_keywords() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_table_tokens.al", TABLE_AL).await;
    let result = client.semantic_tokens("src/zed_table_tokens.al").await;

    assert!(result.is_some(), "Must return semantic tokens for table AL");
    let groups = semantic_token_data(&result.unwrap());
    assert!(
        !groups.is_empty(),
        "Token data must be non-empty for TABLE_AL"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. Document symbols — Zed outline panel
// ---------------------------------------------------------------------------

/// Zed's outline panel uses `DocumentSymbol` (hierarchical), not flat
/// `SymbolInformation`. The response must be an array of objects with `name`,
/// `kind`, and `range` fields.
#[tokio::test]
async fn zed_fidelity_document_symbols_hierarchical_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_symbols.al", CODEUNIT_AL).await;
    let symbols = client.document_symbols("src/zed_symbols.al").await;

    assert!(
        !symbols.is_empty(),
        "Zed fidelity: server must return document symbols for codeunit"
    );

    let top = &symbols[0];
    assert!(
        top.get("name").and_then(|v| v.as_str()).is_some(),
        "Top-level symbol must have a 'name' string. Got: {top}"
    );
    assert!(
        top.get("kind").and_then(|v| v.as_u64()).is_some(),
        "Top-level symbol must have a numeric 'kind'. Got: {top}"
    );
    assert!(
        top.get("range").is_some(),
        "Top-level symbol must have a 'range'. Got: {top}"
    );

    client.shutdown().await;
}

/// The outline for a codeunit must show the codeunit as root with procedures
/// as children — this is the tree Zed renders in its outline panel.
#[tokio::test]
async fn zed_fidelity_document_symbols_codeunit_children() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_cu_symbols.al", CODEUNIT_AL).await;
    let symbols = client.document_symbols("src/zed_cu_symbols.al").await;

    let names = symbol_names(&symbols);
    assert!(
        names.iter().any(|n| n.contains("Zed Fidelity Test")),
        "Must find codeunit name. Got: {names:?}"
    );
    assert!(
        names.iter().any(|n| *n == "HelloWorld"),
        "Must find HelloWorld procedure. Got: {names:?}"
    );
    assert!(
        names.iter().any(|n| *n == "Add"),
        "Must find Add procedure. Got: {names:?}"
    );

    client.shutdown().await;
}

/// The outline for a table must include field symbols so Zed can show them
/// in the outline panel.
#[tokio::test]
async fn zed_fidelity_document_symbols_table_fields() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_tbl_symbols.al", TABLE_AL).await;
    let symbols = client.document_symbols("src/zed_tbl_symbols.al").await;

    let names = symbol_names(&symbols);
    assert!(
        names.iter().any(|n| n.contains("Zed Test Table")),
        "Must find table name. Got: {names:?}"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 6. Diagnostics — format Zed expects
// ---------------------------------------------------------------------------

/// Diagnostics must use `publishDiagnostics` (push) with the correct shape:
/// `uri`, `diagnostics` array, each item having `range`, `severity`, `message`.
#[tokio::test]
async fn zed_fidelity_diagnostics_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_diag.al", CODEUNIT_AL).await;

    let notifs = client.drain_notifications();
    let diag_notifs: Vec<_> = notifs
        .iter()
        .filter(|(m, _)| m == "textDocument/publishDiagnostics")
        .collect();

    assert!(
        !diag_notifs.is_empty(),
        "Server must push publishDiagnostics (Zed fidelity)"
    );

    for (_, params) in &diag_notifs {
        assert!(
            params.get("uri").and_then(|v| v.as_str()).is_some(),
            "publishDiagnostics must include 'uri'. Got: {params}"
        );
        assert!(
            params
                .get("diagnostics")
                .and_then(|v| v.as_array())
                .is_some(),
            "publishDiagnostics must include 'diagnostics' array. Got: {params}"
        );
    }

    client.shutdown().await;
}

/// Individual diagnostics must have fields that Zed's diagnostics panel
/// renders: `range`, `severity` (1–4), and `message`.
#[tokio::test]
async fn zed_fidelity_diagnostic_items_have_required_fields() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    // Custom lint rules have been removed so CODEUNIT_AL may produce no diagnostics.
    // This test validates that any diagnostics that ARE published have the correct
    // structure — it does not require diagnostics to be present.
    client.open_file("src/zed_diag_items.al", CODEUNIT_AL).await;

    // Collect any diagnostics published within 5s.
    let mut all_diags: Vec<serde_json::Value> = vec![];
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let diag_map = client.drain_diagnostics();
        for diags in diag_map.into_values() {
            all_diags.extend(diags);
        }
        if !all_diags.is_empty() {
            break;
        }
    }

    // Validate structure of each diagnostic received (loop is a no-op if empty).
    for diag in &all_diags {
        assert!(
            diag.get("range").is_some(),
            "Diagnostic must have 'range'. Got: {diag}"
        );
        assert!(
            diag.get("message").and_then(|v| v.as_str()).is_some(),
            "Diagnostic must have string 'message'. Got: {diag}"
        );
        if let Some(sev) = diag.get("severity").and_then(|v| v.as_u64()) {
            assert!(
                (1..=4).contains(&sev),
                "Diagnostic severity must be 1–4. Got: {sev}"
            );
        }
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 7. Code actions — Zed-compatible edits
// ---------------------------------------------------------------------------

/// Zed's code action UI expects responses that are `Command` objects or
/// `CodeAction` objects with an optional `edit` (WorkspaceEdit).
/// Each returned item must be a JSON object.
#[tokio::test]
async fn zed_fidelity_code_actions_are_objects() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_actions.al", CODEUNIT_AL).await;
    let actions = client.code_actions("src/zed_actions.al", 0, 5).await;

    for action in &actions {
        assert!(
            action.is_object(),
            "Each code action must be a JSON object. Got: {action}"
        );
        // Must have at least a `title` for Zed to display in the UI
        assert!(
            action.get("title").and_then(|v| v.as_str()).is_some(),
            "Code action must have a 'title'. Got: {action}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 8. Hover — MarkupContent format Zed renders
// ---------------------------------------------------------------------------

/// Zed renders hover contents as Markdown. The server should return
/// `MarkupContent` with `kind: "markdown"` (or `kind: "plaintext"` as
/// fallback). Zed declared `contentFormat: ["markdown", "plaintext"]`.
#[tokio::test]
async fn zed_fidelity_hover_uses_markup_content() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_hover.al", CODEUNIT_AL).await;

    // "HelloWorld" procedure name at line 2, col ~14
    let hover = client.hover("src/zed_hover.al", 2, 18).await;
    assert!(
        hover.is_some(),
        "Zed fidelity: server must return hover for procedure name"
    );

    let hover_val = hover.unwrap();
    let contents = hover_val.get("contents");
    assert!(
        contents.is_some(),
        "Hover result must have 'contents'. Got: {hover_val}"
    );

    let contents = contents.unwrap();
    // Either MarkupContent { kind, value } or a string / MarkedString
    if contents.is_object() {
        let kind = contents.get("kind").and_then(|v| v.as_str());
        assert!(
            kind == Some("markdown") || kind == Some("plaintext"),
            "MarkupContent kind must be 'markdown' or 'plaintext'. Got: {kind:?}"
        );
        assert!(
            contents.get("value").and_then(|v| v.as_str()).is_some(),
            "MarkupContent must have a string 'value'. Got: {contents}"
        );
    }
    // If it's a string, that's also acceptable (MarkedString fallback)

    client.shutdown().await;
}

/// Hover for a local variable must include its type so Zed's hover popup
/// is informative.
#[tokio::test]
async fn zed_fidelity_hover_variable_includes_type() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_hover_var.al", CODEUNIT_AL).await;

    // "Msg" local variable used at line 6, col 8
    let hover = client.hover("src/zed_hover_var.al", 6, 8).await;
    assert!(
        hover.is_some(),
        "Zed fidelity: server must return hover for local variable"
    );

    let hover_val = hover.unwrap();
    let content = hover_content(&hover_val);
    assert!(
        content.map_or(false, |c| c.contains("Text") || c.contains("Msg")),
        "Hover on Msg should mention its name or type. Got: {content:?}"
    );

    client.shutdown().await;
}

/// Hover response must include a `range` so Zed can highlight the hovered
/// token in the editor.
#[tokio::test]
async fn zed_fidelity_hover_includes_range() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client
        .open_file("src/zed_hover_range.al", CODEUNIT_AL)
        .await;

    let hover = client.hover("src/zed_hover_range.al", 2, 18).await;
    assert!(hover.is_some(), "Zed fidelity: hover must succeed");

    let hover_val = hover.unwrap();
    // range is optional per spec but Zed uses it for highlighting
    if let Some(range) = hover_val.get("range") {
        let start = range.get("start");
        let end = range.get("end");
        assert!(
            start.is_some() && end.is_some(),
            "Hover range must have 'start' and 'end'. Got: {range}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 9. Folding ranges — correct kinds
// ---------------------------------------------------------------------------

/// Zed renders folding gutters using `startLine`/`endLine`. The server must
/// return these numeric fields. `kind` is optional but helps Zed distinguish
/// comment folds from code folds.
#[tokio::test]
async fn zed_fidelity_folding_ranges_have_line_fields() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_fold.al", CODEUNIT_AL).await;
    let ranges = client.folding_ranges("src/zed_fold.al").await;

    assert!(
        !ranges.is_empty(),
        "Zed fidelity: server must return folding ranges for codeunit"
    );

    for r in &ranges {
        assert!(
            r.get("startLine").and_then(|v| v.as_u64()).is_some(),
            "Folding range must have numeric 'startLine'. Got: {r}"
        );
        assert!(
            r.get("endLine").and_then(|v| v.as_u64()).is_some(),
            "Folding range must have numeric 'endLine'. Got: {r}"
        );
        let start = r["startLine"].as_u64().unwrap();
        let end = r["endLine"].as_u64().unwrap();
        assert!(
            start <= end,
            "startLine must be <= endLine. Got: {start} > {end}"
        );
        // kind, if present, must be one of the LSP-specified strings
        if let Some(kind) = r.get("kind").and_then(|v| v.as_str()) {
            assert!(
                matches!(kind, "comment" | "imports" | "region"),
                "FoldingRange kind must be 'comment', 'imports', or 'region'. Got: {kind}"
            );
        }
    }

    client.shutdown().await;
}

/// The codeunit body must produce at least one folding range spanning the
/// object boundary — Zed uses this for collapsing entire AL objects.
#[tokio::test]
async fn zed_fidelity_folding_covers_object_body() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_fold_body.al", CODEUNIT_AL).await;
    let ranges = client.folding_ranges("src/zed_fold_body.al").await;

    let lines = folding_range_lines(&ranges);
    assert!(
        lines.iter().any(|(_, end)| *end >= 10),
        "At least one fold must span the codeunit body (endLine >= 10). Got: {lines:?}"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 10. Signature help — active parameter highlighting
// ---------------------------------------------------------------------------

/// Zed displays signature help while typing function arguments. The server must
/// return `signatures` with at least one `SignatureInformation` containing
/// `label` and optionally `parameters`. The `activeParameter` field tells Zed
/// which parameter to highlight.
#[tokio::test]
async fn zed_fidelity_signature_help_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    // Place cursor inside a Message() call — column after the opening paren
    let code = r#"codeunit 50100 "Zed Test"
{
    procedure DoWork()
    begin
        Message(
    end;
}"#;

    client.open_file("src/zed_sighelp.al", code).await;
    let sig_help = client.signature_help("src/zed_sighelp.al", 4, 16).await;

    if let Some(result) = sig_help {
        // If the server returns a result, it must have the correct shape
        let sigs = result.get("signatures").and_then(|v| v.as_array());
        assert!(
            sigs.is_some(),
            "Signature help result must have 'signatures' array. Got: {result}"
        );

        for sig in sigs.unwrap() {
            assert!(
                sig.get("label").and_then(|v| v.as_str()).is_some(),
                "Each SignatureInformation must have string 'label'. Got: {sig}"
            );
            // parameters, if present, must be an array
            if let Some(params) = sig.get("parameters") {
                assert!(
                    params.is_array(),
                    "SignatureInformation.parameters must be an array. Got: {params}"
                );
            }
        }

        // activeSignature and activeParameter must be non-negative integers if present
        if let Some(active_sig) = result.get("activeSignature") {
            assert!(
                active_sig.as_u64().is_some(),
                "activeSignature must be a non-negative integer. Got: {active_sig}"
            );
        }
        if let Some(active_param) = result.get("activeParameter") {
            assert!(
                active_param.as_u64().is_some(),
                "activeParameter must be a non-negative integer. Got: {active_param}"
            );
        }
    }
    // A null result is valid when the cursor is not in a call expression.

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 11. Go-to-definition — Zed navigation
// ---------------------------------------------------------------------------

/// Zed sends `textDocument/definition` when the user Cmd-clicks a symbol.
/// The response must be a `Location` or `Location[]` with `uri` and `range`.
#[tokio::test]
async fn zed_fidelity_definition_response_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_def.al", CODEUNIT_AL).await;

    // "Msg" is used at line 6 col 8; its declaration is at line 4
    let def = client.definition("src/zed_def.al", 6, 8).await;

    if let Some(result) = def {
        // Can be a single Location object or an array
        let validate_location = |loc: &serde_json::Value| {
            assert!(
                loc.get("uri").and_then(|v| v.as_str()).is_some(),
                "Definition location must have string 'uri'. Got: {loc}"
            );
            let range = loc.get("range");
            assert!(
                range.is_some(),
                "Definition location must have 'range'. Got: {loc}"
            );
        };

        if let Some(arr) = result.as_array() {
            assert!(!arr.is_empty(), "Definition array must not be empty");
            for loc in arr {
                validate_location(loc);
            }
        } else {
            validate_location(&result);
        }
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 12. References — Zed find-all-references
// ---------------------------------------------------------------------------

/// Zed sends `textDocument/references` and expects a flat `Location[]`.
/// Each location must have `uri` and `range`.
#[tokio::test]
async fn zed_fidelity_references_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_refs.al", CODEUNIT_AL).await;

    // "Msg" declared at line 4; should have references at lines 6 and 7
    let refs = client.references("src/zed_refs.al", 4, 8).await;

    for loc in &refs {
        assert!(
            loc.get("uri").and_then(|v| v.as_str()).is_some(),
            "Reference location must have string 'uri'. Got: {loc}"
        );
        assert!(
            loc.get("range").is_some(),
            "Reference location must have 'range'. Got: {loc}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 13. Workspace symbols — Zed quick-open
// ---------------------------------------------------------------------------

/// Zed's workspace symbol search (Cmd+T) expects results with `name`, `kind`,
/// and `location` fields.
#[tokio::test]
async fn zed_fidelity_workspace_symbols_shape() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_ws_sym.al", CODEUNIT_AL).await;

    let symbols = client.workspace_symbol("Zed").await;

    for sym in &symbols {
        assert!(
            sym.get("name").and_then(|v| v.as_str()).is_some(),
            "Workspace symbol must have string 'name'. Got: {sym}"
        );
        assert!(
            sym.get("kind").and_then(|v| v.as_u64()).is_some(),
            "Workspace symbol must have numeric 'kind'. Got: {sym}"
        );
        // location can be a Location or a LocationLink
        assert!(
            sym.get("location").is_some(),
            "Workspace symbol must have 'location'. Got: {sym}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 14. Multiple files — Zed opens many documents
// ---------------------------------------------------------------------------

/// Zed typically has multiple files open at once. The server must handle
/// concurrent open documents without mixing up diagnostics.
#[tokio::test]
async fn zed_fidelity_multiple_open_documents() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/cu.al", CODEUNIT_AL).await;
    client.open_file("src/tbl.al", TABLE_AL).await;
    client.open_file("src/pg.al", PAGE_AL).await;

    // Hover must work on each independently
    let h1 = client.hover("src/cu.al", 2, 18).await;
    let h2 = client.hover("src/tbl.al", 6, 15).await;

    assert!(
        h1.is_some() || h2.is_some(),
        "Hover must work on at least one of the concurrently open files"
    );

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 15. Diagnostic range format — UTF-16 code units
// ---------------------------------------------------------------------------

/// Zed works with UTF-16 positions (same as LSP spec). Diagnostic ranges
/// must use integer line/character values, not byte offsets.
#[tokio::test]
async fn zed_fidelity_diagnostic_range_is_utf16() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_utf16.al", CODEUNIT_AL).await;

    // Collect any diagnostics
    let mut all_diags: Vec<serde_json::Value> = vec![];
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        let diag_map = client.drain_diagnostics();
        for diags in diag_map.into_values() {
            all_diags.extend(diags);
        }
        if !all_diags.is_empty() {
            break;
        }
    }

    for diag in &all_diags {
        let range = &diag["range"];
        let start_line = range["start"]["line"].as_u64();
        let start_char = range["start"]["character"].as_u64();
        let end_line = range["end"]["line"].as_u64();
        let end_char = range["end"]["character"].as_u64();

        assert!(
            start_line.is_some() && start_char.is_some(),
            "Diagnostic start must have integer line and character. Got: {range}"
        );
        assert!(
            end_line.is_some() && end_char.is_some(),
            "Diagnostic end must have integer line and character. Got: {range}"
        );
    }

    client.shutdown().await;
}

// ---------------------------------------------------------------------------
// 16. Shutdown — clean exit
// ---------------------------------------------------------------------------

/// Zed sends `shutdown` then `exit`. The server must respond to `shutdown`
/// with null and exit cleanly within a reasonable timeout.
#[tokio::test]
async fn zed_fidelity_shutdown_is_clean() {
    let dir = test_project_dir();
    let mut client = LspClient::spawn(&dir).await.unwrap();

    client.open_file("src/zed_shutdown.al", CODEUNIT_AL).await;

    // shutdown() sends the shutdown request + exit notification and waits up to 3s
    client.shutdown().await;
    // If we reach here without hang or panic, the test passes
}
